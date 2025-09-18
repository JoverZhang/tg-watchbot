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
    match &msg.kind {
        MessageKind::Common(common) => {
            let text_content = msg.text().map(str::to_owned);
            let caption = msg.caption().map(str::to_owned);

            if let Some(text) = text_content.as_deref() {
                handle_text_content(pool, user_id, message_id, text, true).await?;
                return Ok(());
            }

            if let Some(caption) = caption.as_deref() {
                handle_text_content(pool, user_id, message_id, caption, false).await?;
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
                    let batch_id = db::current_open_batch_id(pool, user_id).await?;
                    let _rid =
                        db::insert_resource(pool, user_id, batch_id, "video", &path, message_id)
                            .await?;
                }
                _ => {}
            }
        }
        _ => {}
    }

    Ok(())
}

async fn handle_text_content(
    pool: &SqlitePool,
    user_id: i64,
    message_id: i32,
    text_content: &str,
    allow_commands: bool,
) -> Result<()> {
    if allow_commands && text_content.trim() == "==BEGIN==" {
        if let Err(err) = db::open_batch(pool, user_id).await {
            warn!(?err, "failed to open batch");
        } else {
            info!(user_id, "opened batch");
        }
        return Ok(());
    }

    if allow_commands {
        if let Some(title) = parse_commit_title(text_content) {
            if let Err(err) = db::commit_batch(pool, user_id, title.as_deref()).await {
                warn!(?err, "failed to commit batch");
            } else {
                info!(user_id, "committed batch");
            }
            return Ok(());
        }
    }

    if allow_commands && text_content.trim() == "==ROLLBACK==" {
        if let Err(err) = db::rollback_batch(pool, user_id).await {
            warn!(?err, "failed to rollback batch");
        } else {
            info!(user_id, "rolled back batch");
        }
        return Ok(());
    }

    let batch_id = db::current_open_batch_id(pool, user_id).await?;
    let _rid =
        db::insert_resource(pool, user_id, batch_id, "text", text_content, message_id).await?;
    Ok(())
}

fn parse_commit_title(text: &str) -> Option<Option<String>> {
    // Accept patterns: "==COMMIT==" or "==COMMIT== (title)"
    let trimmed = text.trim();
    if trimmed == "==COMMIT==" {
        return Some(None);
    }
    static RE: once_cell::sync::OnceCell<Regex> = once_cell::sync::OnceCell::new();
    let re = RE.get_or_init(|| Regex::new(r"^==COMMIT==\s*\((?P<title>.+)\)\s*$").unwrap());
    if let Some(caps) = re.captures(trimmed) {
        return Some(Some(caps.name("title").unwrap().as_str().to_string()));
    }
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
