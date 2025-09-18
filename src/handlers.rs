use crate::db;
use anyhow::Result;
use regex::Regex;
use sqlx::SqlitePool;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{MediaKind, MessageKind};
use tracing::{info, instrument, warn};

#[instrument(skip_all)]
pub async fn handle_update(
    bot: &Bot,
    pool: &SqlitePool,
    data_dir: &str,
    msg: &Message,
) -> Result<()> {
    let user = match msg.from() {
        Some(u) => u,
        None => return Ok(()),
    };

    let tg_user_id = user.id.0 as i64;
    let username = user.username.as_deref();
    let full_name = format!(
        "{} {}",
        user.first_name,
        user.last_name.clone().unwrap_or_default()
    );
    let user_id = db::get_or_create_user(pool, tg_user_id, username, Some(&full_name)).await?;

    let message_id = msg.id.0 as i32;

    // If awaiting title input, handle it before any other processing
    if let Some(state) = db::current_batch_state(pool, user_id).await? {
        if state == crate::model::BatchState::WAITING_TITLE {
            if let Some(text) = msg.text() {
                let trimmed = text.trim();
                if trimmed.eq_ignore_ascii_case("/rollback") {
                    if let Err(err) = db::rollback_batch(pool, user_id).await {
                        warn!(?err, "failed to rollback batch");
                    } else {
                        let _ = bot
                            .send_message(msg.chat.id, "Rolled back.")
                            .await;
                    }
                    return Ok(());
                }
                // Disallow commands as titles while waiting for title
                if trimmed.starts_with('/') {
                    let _ = bot
                        .send_message(
                            msg.chat.id,
                            "Invalid input: title must be a text message. Please send text.",
                        )
                        .await;
                    return Ok(());
                }

                if trimmed.is_empty() {
                    let _ = bot
                        .send_message(
                            msg.chat.id,
                            "Invalid input: title must be a non-empty text message. Please send text.",
                        )
                        .await;
                    return Ok(());
                }

                // Use this text as the title and commit
                if let Err(err) = db::commit_batch(pool, user_id, Some(trimmed)).await {
                    warn!(?err, "failed to commit batch with provided title");
                } else {
                    let _ = bot
                        .send_message(
                            msg.chat.id,
                            format!("Committed batch with title: {}", trimmed),
                        )
                        .await;
                }
                return Ok(());
            } else {
                // Non-text input while waiting for title
                let _ = bot
                    .send_message(
                        msg.chat.id,
                        "Invalid input: title must be a text message. Please send text.",
                    )
                    .await;
                return Ok(());
            }
        }
    }
    match &msg.kind {
        MessageKind::Common(common) => {
            let text_content = msg.text().map(str::to_owned);
            let caption = msg.caption().map(str::to_owned);

            if let Some(text) = text_content.as_deref() {
                handle_text_content(bot, msg, pool, user_id, message_id, text, true).await?;
                return Ok(());
            }

            if let Some(caption) = caption.as_deref() {
                handle_text_content(bot, msg, pool, user_id, message_id, caption, false).await?;
            }

            match &common.media_kind {
                MediaKind::Text(_) => {}
                MediaKind::Photo(photo) => {
                    if let Some(size) = photo.photo.last() {
                        let path = download_file(
                            bot,
                            data_dir,
                            tg_user_id,
                            message_id,
                            size.file.id.as_ref(),
                        )
                        .await?;
                        let batch_id = db::current_open_batch_id(pool, user_id).await?;
                        let _rid = db::insert_resource(
                            pool, user_id, batch_id, "photo", &path, message_id,
                        )
                        .await?;
                        let ack = if batch_id.is_some() {
                            "Saved photo (in batch)."
                        } else {
                            "Saved photo."
                        };
                        let _ = bot.send_message(msg.chat.id, ack).await;
                    }
                }
                MediaKind::Video(video) => {
                    let path = download_file(
                        bot,
                        data_dir,
                        tg_user_id,
                        message_id,
                        video.video.file.id.as_ref(),
                    )
                    .await?;
                    // Generate thumbnail before persisting; treat failure as overall failure
                    match crate::thumbnail::generate_thumbnail(&path, data_dir).await {
                        Ok(thumb_path) => {
                            info!(video=%path, thumb=%thumb_path.display(), "generated thumbnail");
                        }
                        Err(err) => {
                            warn!(?err, video=%path, "failed to generate thumbnail; aborting save");
                            let _ = bot
                                .send_message(
                                    msg.chat.id,
                                    "Failed to save video (thumbnail generation error).",
                                )
                                .await;
                            return Ok(());
                        }
                    }
                    let batch_id = db::current_open_batch_id(pool, user_id).await?;
                    let _rid =
                        db::insert_resource(pool, user_id, batch_id, "video", &path, message_id)
                            .await?;
                    let ack = if batch_id.is_some() {
                        "Saved video (in batch)."
                    } else {
                        "Saved video."
                    };
                    let _ = bot.send_message(msg.chat.id, ack).await;
                }
                _ => {
                    let _ = bot
                        .send_message(msg.chat.id, "Unsupported message type.")
                        .await;
                }
            }
        }
        _ => {}
    }

    Ok(())
}

async fn handle_text_content(
    bot: &Bot,
    msg: &Message,
    pool: &SqlitePool,
    user_id: i64,
    message_id: i32,
    text_content: &str,
    allow_commands: bool,
) -> Result<()> {
    let trimmed = text_content.trim();

    // Ignore /start here (UI is handled in main.rs); do not persist it
    if allow_commands && trimmed == "/start" {
        return Ok(());
    }

    // Ping health check
    if allow_commands && (trimmed == "/ping") {
        let _ = bot.send_message(msg.chat.id, "PONG").await;
        return Ok(());
    }
    if allow_commands && trimmed == "/begin" {
        if let Err(err) = db::open_batch(pool, user_id).await {
            warn!(?err, "failed to open batch");
        } else {
            info!(user_id, "opened batch");
            let _ = bot.send_message(msg.chat.id, "Opened batch.").await;
        }
        return Ok(());
    }

    if allow_commands && trimmed == "/commit" {
        match db::current_open_batch_id(pool, user_id).await? {
            None => {
                let _ = bot
                    .send_message(msg.chat.id, "No open batch to commit.")
                    .await;
            }
            Some(_) => {
                if let Err(err) = db::mark_current_batch_waiting_title(pool, user_id).await {
                    warn!(?err, "failed to mark batch waiting title");
                } else {
                    let _ = bot
                        .send_message(msg.chat.id, "Please input title:")
                        .await;
                }
            }
        }
        return Ok(());
    }

    if allow_commands && trimmed == "/rollback" {
        if let Err(err) = db::rollback_batch(pool, user_id).await {
            warn!(?err, "failed to rollback batch");
        } else {
            info!(user_id, "rolled back batch");
            let _ = bot.send_message(msg.chat.id, "Rolled back.").await;
        }
        return Ok(());
    }

    // Unknown slash command: reply and do not persist
    if allow_commands && trimmed.starts_with('/') {
        let _ = bot
            .send_message(msg.chat.id, "Unknown command.")
            .await;
        return Ok(());
    }

    let batch_id = db::current_open_batch_id(pool, user_id).await?;
    let _rid =
        db::insert_resource(pool, user_id, batch_id, "text", text_content, message_id).await?;
    let ack = if batch_id.is_some() {
        "Saved (in batch)."
    } else {
        "Saved."
    };
    let _ = bot.send_message(msg.chat.id, ack).await;
    Ok(())
}

fn parse_commit_title(text: &str) -> Option<Option<String>> {
    // Deprecated: old pattern no longer supported
    let _ = text; // keep signature; always return None
    None
}

async fn download_file(
    bot: &Bot,
    data_dir: &str,
    tg_user_id: i64,
    msg_id: i32,
    file_id: &str,
) -> Result<String> {
    // Resolve file path from Telegram API, then download to local storage
    let file = bot.get_file(file_id).await?;
    let dir = format!("{}/media/{}/", data_dir, tg_user_id);
    tokio::fs::create_dir_all(&dir).await.ok();
    // Try to preserve the original file extension from Telegram's file path
    let ext = std::path::Path::new(&file.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let path = format!("{}{}_{}.{}", dir, msg_id, file.meta.unique_id, ext);
    let mut dst = tokio::fs::File::create(&path).await?;
    bot.download_file(&file.path, &mut dst).await?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_title_parsing() {
        assert_eq!(parse_commit_title("==COMMIT=="), Some(None));
        assert_eq!(
            parse_commit_title("==COMMIT== (Title)"),
            Some(Some("Title".to_string()))
        );
        assert_eq!(parse_commit_title("==COMMIT==()"), None);
        assert_eq!(parse_commit_title("random"), None);
    }
}
