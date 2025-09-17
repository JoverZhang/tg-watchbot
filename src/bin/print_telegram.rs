use anyhow::Result;
use clap::Parser;
use std::fs;
use std::path::PathBuf;
use teloxide::prelude::*;
use teloxide::types::{
    Animation, Audio, Document, MediaKind, Message, MessageKind, Sticker, Video, VideoNote, Voice,
};
use tg_watchbot::config::Telegram as TelegramCfg;

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Listen and print Telegram messages with media URLs"
)]
struct Args {
    /// Path to YAML config file (reads only `telegram`)
    #[arg(long, default_value = "config.yaml")]
    config: PathBuf,

    /// Resolve file paths via getFile and print download URLs
    #[arg(long, default_value_t = true)]
    resolve_paths: bool,
}

#[derive(Debug, serde::Deserialize)]
struct TelegramOnlyConfig {
    telegram: TelegramCfg,
}

// Whether to resolve file_path (will call getFile)
// true: resolve download URL for each media; false: only print metadata (saves API quota)
pub async fn print_message_expanded(
    bot: &Bot,
    bot_token: &str,
    msg: &Message,
    resolve_file_path: bool,
) -> Result<()> {
    let chat_id = msg.chat.id.0;
    let user = msg
        .from()
        .and_then(|u| u.username.clone())
        .unwrap_or_default();

    // Text/caption
    if let Some(t) = msg.text() {
        println!("[chat:{} user:@{}] text: {}", chat_id, user, t);
    }
    if let Some(c) = msg.caption() {
        println!("[chat:{} user:@{}] caption: {}", chat_id, user, c);
    }

    match &msg.kind {
        MessageKind::Common(common) => {
            match &common.media_kind {
                MediaKind::Photo(p) => {
                    // Multiple sizes of the same image: print each one
                    for (i, sz) in p.photo.iter().enumerate() {
                        let file_id = &sz.file.id;
                        if resolve_file_path {
                            let file = bot.get_file(file_id.clone()).await?;
                            let url = format!(
                                "https://api.telegram.org/file/bot{}/{}",
                                bot_token, file.path
                            );
                            println!(
                                "[chat:{} user:@{}] photo[{}]: {}x{} size={} file_id={} path={} url={}",
                                chat_id, user, i, sz.width, sz.height, sz.file.size,
                                file_id, file.path, url
                            );
                        } else {
                            println!(
                                "[chat:{} user:@{}] photo[{}]: {}x{} size={} file_id={}",
                                chat_id, user, i, sz.width, sz.height, sz.file.size, file_id
                            );
                        }
                    }
                }
                MediaKind::Video(v) => {
                    print_video(bot, bot_token, chat_id, &user, &v.video, resolve_file_path)
                        .await?;
                }
                MediaKind::Document(d) => {
                    print_document(
                        bot,
                        bot_token,
                        chat_id,
                        &user,
                        &d.document,
                        resolve_file_path,
                    )
                    .await?;
                }
                MediaKind::Animation(a) => {
                    print_animation(
                        bot,
                        bot_token,
                        chat_id,
                        &user,
                        &a.animation,
                        resolve_file_path,
                    )
                    .await?;
                }
                MediaKind::Audio(a) => {
                    print_audio(bot, bot_token, chat_id, &user, &a.audio, resolve_file_path)
                        .await?;
                }
                MediaKind::Voice(v) => {
                    print_voice(bot, bot_token, chat_id, &user, &v.voice, resolve_file_path)
                        .await?;
                }
                MediaKind::VideoNote(vn) => {
                    print_video_note(
                        bot,
                        bot_token,
                        chat_id,
                        &user,
                        &vn.video_note,
                        resolve_file_path,
                    )
                    .await?;
                }
                MediaKind::Sticker(s) => {
                    print_sticker(
                        bot,
                        bot_token,
                        chat_id,
                        &user,
                        &s.sticker,
                        resolve_file_path,
                    )
                    .await?;
                }
                other => {
                    // Many other types (location, poll, contact...) can be added as needed
                    println!("[chat:{} user:@{}] other kind: {:?}", chat_id, user, other);
                }
            }
        }
        other => {
            println!("[chat:{} user:@{}] non-common: {:?}", chat_id, user, other);
        }
    }

    Ok(())
}

// Print utilities for each media type
// You could also merge into one generic function: pass file_id + tag name; separated here to print more specific metadata

async fn print_video(
    bot: &Bot,
    token: &str,
    chat_id: i64,
    user: &str,
    v: &Video,
    resolve: bool,
) -> Result<()> {
    let file_id = &v.file.id;
    if resolve {
        let f = bot.get_file(file_id.clone()).await?;
        let url = format!("https://api.telegram.org/file/bot{}/{}", token, f.path);
        println!(
            "[chat:{} user:@{}] video: {}x{} dur={}s size={} file_id={} path={} url={}",
            chat_id, user, v.width, v.height, v.duration, v.file.size, file_id, f.path, url
        );
    } else {
        println!(
            "[chat:{} user:@{}] video: {}x{} dur={}s size={} file_id={}",
            chat_id, user, v.width, v.height, v.duration, v.file.size, file_id
        );
    }
    Ok(())
}

async fn print_document(
    bot: &Bot,
    token: &str,
    chat_id: i64,
    user: &str,
    d: &Document,
    resolve: bool,
) -> Result<()> {
    let file_id = &d.file.id;
    if resolve {
        let f = bot.get_file(file_id.clone()).await?;
        let url = format!("https://api.telegram.org/file/bot{}/{}", token, f.path);
        println!("[chat:{} user:@{}] document: filename={:?} mime={:?} size={} file_id={} path={} url={}",
            chat_id, user, d.file_name, d.mime_type, d.file.size, file_id, f.path, url);
    } else {
        println!(
            "[chat:{} user:@{}] document: filename={:?} mime={:?} size={} file_id={}",
            chat_id, user, d.file_name, d.mime_type, d.file.size, file_id
        );
    }
    Ok(())
}

async fn print_animation(
    bot: &Bot,
    token: &str,
    chat_id: i64,
    user: &str,
    a: &Animation,
    resolve: bool,
) -> Result<()> {
    let file_id = &a.file.id;
    if resolve {
        let f = bot.get_file(file_id.clone()).await?;
        let url = format!("https://api.telegram.org/file/bot{}/{}", token, f.path);
        println!("[chat:{} user:@{}] animation: {}x{} dur={}s mime={:?} size={} file_id={} path={} url={}",
            chat_id, user, a.width, a.height, a.duration, a.mime_type, a.file.size,
            file_id, f.path, url);
    } else {
        println!(
            "[chat:{} user:@{}] animation: {}x{} dur={}s mime={:?} size={} file_id={}",
            chat_id, user, a.width, a.height, a.duration, a.mime_type, a.file.size, file_id
        );
    }
    Ok(())
}

async fn print_audio(
    bot: &Bot,
    token: &str,
    chat_id: i64,
    user: &str,
    a: &Audio,
    resolve: bool,
) -> Result<()> {
    let file_id = &a.file.id;
    if resolve {
        let f = bot.get_file(file_id.clone()).await?;
        let url = format!("https://api.telegram.org/file/bot{}/{}", token, f.path);
        println!("[chat:{} user:@{}] audio: dur={}s performer={:?} title={:?} mime={:?} size={} file_id={} path={} url={}",
            chat_id, user, a.duration, a.performer, a.title, a.mime_type, a.file.size,
            file_id, f.path, url);
    } else {
        println!("[chat:{} user:@{}] audio: dur={}s performer={:?} title={:?} mime={:?} size={} file_id={}",
            chat_id, user, a.duration, a.performer, a.title, a.mime_type, a.file.size, file_id);
    }
    Ok(())
}

async fn print_voice(
    bot: &Bot,
    token: &str,
    chat_id: i64,
    user: &str,
    v: &Voice,
    resolve: bool,
) -> Result<()> {
    let file_id = &v.file.id;
    if resolve {
        let f = bot.get_file(file_id.clone()).await?;
        let url = format!("https://api.telegram.org/file/bot{}/{}", token, f.path);
        println!(
            "[chat:{} user:@{}] voice: dur={}s mime={:?} size={} file_id={} path={} url={}",
            chat_id, user, v.duration, v.mime_type, v.file.size, file_id, f.path, url
        );
    } else {
        println!(
            "[chat:{} user:@{}] voice: dur={}s mime={:?} size={} file_id={}",
            chat_id, user, v.duration, v.mime_type, v.file.size, file_id
        );
    }
    Ok(())
}

async fn print_video_note(
    bot: &Bot,
    token: &str,
    chat_id: i64,
    user: &str,
    vn: &VideoNote,
    resolve: bool,
) -> Result<()> {
    let file_id = &vn.file.id;
    if resolve {
        let f = bot.get_file(file_id.clone()).await?;
        let url = format!("https://api.telegram.org/file/bot{}/{}", token, f.path);
        println!(
            "[chat:{} user:@{}] video_note: len={} size={} file_id={} path={} url={}",
            chat_id, user, vn.length, vn.file.size, file_id, f.path, url
        );
    } else {
        println!(
            "[chat:{} user:@{}] video_note: len={} size={} file_id={}",
            chat_id, user, vn.length, vn.file.size, file_id
        );
    }
    Ok(())
}

async fn print_sticker(
    bot: &Bot,
    token: &str,
    chat_id: i64,
    user: &str,
    s: &Sticker,
    resolve: bool,
) -> Result<()> {
    let file_id = &s.file.id;
    if resolve {
        let f = bot.get_file(file_id.clone()).await?;
        let url = format!("https://api.telegram.org/file/bot{}/{}", token, f.path);
        println!("[chat:{} user:@{}] sticker: {}x{} is_animated={} is_video={} file_id={} path={} url={}",
            chat_id, user, s.width, s.height, s.is_animated(), s.is_video(), file_id, f.path, url);
    } else {
        println!(
            "[chat:{} user:@{}] sticker: {}x{} is_animated={} is_video={} file_id={}",
            chat_id,
            user,
            s.width,
            s.height,
            s.is_animated(),
            s.is_video(),
            file_id
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Logging setup
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    // Args + config
    let args = Args::parse();
    let raw = fs::read_to_string(&args.config)?;
    let cfg: TelegramOnlyConfig = serde_yaml::from_str(&raw)?;

    let token = cfg.telegram.bot_token.clone();
    let allowed = cfg.telegram.allowed_users.clone();
    let resolve = args.resolve_paths;
    let bot = Bot::new(token.clone());

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let token = token.clone();
        let allowed = allowed.clone();
        async move {
            // filter allowed users if provided
            if let Some(from) = msg.from() {
                let uid = from.id.0 as i64;
                if !allowed.is_empty() && !allowed.contains(&uid) {
                    return respond(());
                }
            }

            if let Err(err) = print_message_expanded(&bot, &token, &msg, resolve).await {
                eprintln!("error handling message: {:#}", err);
            }
            respond(())
        }
    })
    .await;

    Ok(())
}
