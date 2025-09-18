use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Ensure `ffmpeg` binary is available on PATH by invoking `ffmpeg -version`.
pub async fn ensure_ffmpeg_available() -> Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-version")
        .kill_on_drop(true)
        .status()
        .await;
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(anyhow!("ffmpeg not available (exit status {})", s)),
        Err(e) => Err(anyhow!("ffmpeg not available: {}", e)),
    }
}

/// Generate a JPEG thumbnail for a given video into `{data_dir}/media/thumbs/`.
/// The thumbnail file name uses the video's file stem, with `.jpg` extension.
/// If the thumbnail already exists, returns its path without re-generating.
pub async fn generate_thumbnail<P: AsRef<Path>>(video_path: P, data_dir: &str) -> Result<PathBuf> {
    let video_path = video_path.as_ref();
    let stem = video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("invalid video file name"))?;

    let thumbs_dir = Path::new(data_dir).join("media").join("thumbs");
    tokio::fs::create_dir_all(&thumbs_dir)
        .await
        .with_context(|| format!("failed to create thumbs dir: {}", thumbs_dir.display()))?;

    let thumb_path = thumbs_dir.join(format!("{}.jpg", stem));
    if tokio::fs::try_exists(&thumb_path).await.unwrap_or(false) {
        return Ok(thumb_path);
    }

    // Run ffmpeg: first frame, scale to max width 480, keep aspect, good quality.
    // Use simple scale=480:-2 to avoid shell quoting issues.
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-ss")
        .arg("0.25")
        .arg("-i")
        .arg(video_path.as_os_str())
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg("scale=480:-2:flags=lanczos")
        .arg("-q:v")
        .arg("6")
        .arg(thumb_path.as_os_str())
        .kill_on_drop(true)
        .status()
        .await
        .with_context(|| format!("failed to spawn ffmpeg for {}", video_path.display()))?;

    if !status.success() {
        return Err(anyhow!(
            "ffmpeg exited with status {} for {}",
            status,
            video_path.display()
        ));
    }

    Ok(thumb_path)
}

