use anyhow::{anyhow, Context, Result};
use clap::Parser;
use reqwest::Url;
use serde_json::{json, Value};
use std::path::PathBuf;
use tg_watchbot::config::{self, Config};
use tg_watchbot::notion::NotionClient;

#[derive(Debug, Parser)]
#[command(
    about = "Export a batch from Notion to a local HTML. Images render via Notion URLs; videos are downloaded to html/video but not rendered."
)]
struct Args {
    /// Path to YAML config file
    #[arg(long, default_value = "config.yaml")]
    config: PathBuf,

    /// Unique key to identify a main table row (e.g., slug or custom property value)
    #[arg(long)]
    key: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = config::load(Some(&args.config))?;
    run(&cfg, &args.key).await
}

async fn run(cfg: &Config, key: &str) -> Result<()> {
    let notion = NotionClient::new(cfg.notion.token.clone(), cfg.notion.version.clone());

    // Determine filter operator for the unique property by inspecting schema
    let main_schema = notion
        .retrieve_database(&cfg.notion.databases.main.id)
        .await
        .context("failed to retrieve main database schema")?;
    // Resolve unique property (accept name or id from config) and its type
    let unique_prop_cfg = &cfg.notion.databases.main.fields.unique;
    let (unique_prop_name, unique_prop_type) = resolve_prop_name_and_type(&main_schema, unique_prop_cfg)
        .ok_or_else(|| {
            anyhow!(
                "unique property '{}' not found by name or id in main database",
                unique_prop_cfg
            )
        })?;

    let http = reqwest::Client::builder()
        .user_agent("tg-watchbot/export-html")
        .no_proxy()
        .build()?;

    // Query main DB for the page matching the key
    let unique_filter = build_unique_filter(&unique_prop_name, &unique_prop_type, key);
    let q_main = json!({
        "filter": unique_filter,
        "page_size": 1
    });
    let main_res = notion_post_json(
        &http,
        &cfg.notion.token,
        &cfg.notion.version,
        &format!("v1/databases/{}/query", cfg.notion.databases.main.id),
        q_main,
    )
    .await?;
    let results = main_res
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("invalid Notion response for main query"))?;
    let main_page = results.get(0).ok_or_else(|| {
        anyhow!(
            "no main row matched key '{}' on property '{}'",
            key,
            unique_prop_cfg
        )
    })?;
    let main_page_id = main_page
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("main page missing id"))?;

    // Query resource DB for related items ordered by order ascending
    // Resolve resource database property names from config (which may contain ids)
    let res_schema = notion
        .retrieve_database(&cfg.notion.databases.resource.id)
        .await
        .context("failed to retrieve resource database schema")?;
    let rel_prop =
        resolve_prop_name(&res_schema, &cfg.notion.databases.resource.fields.relation)
            .ok_or_else(|| anyhow!("resource relation property not found (by name or id)"))?;
    let order_prop = resolve_prop_name(&res_schema, &cfg.notion.databases.resource.fields.order)
        .ok_or_else(|| anyhow!("resource order property not found (by name or id)"))?;
    let text_prop = resolve_prop_name(&res_schema, &cfg.notion.databases.resource.fields.text)
        .ok_or_else(|| anyhow!("resource text property not found (by name or id)"))?;
    let media_prop = resolve_prop_name(&res_schema, &cfg.notion.databases.resource.fields.media)
        .ok_or_else(|| anyhow!("resource media property not found (by name or id)"))?;

    let q_res = json!({
        "filter": { "property": rel_prop, "relation": { "contains": main_page_id } },
        "sorts": [ { "property": order_prop, "direction": "ascending" } ],
        "page_size": 100
    });
    let res_res = notion_post_json(
        &http,
        &cfg.notion.token,
        &cfg.notion.version,
        &format!("v1/databases/{}/query", cfg.notion.databases.resource.id),
        q_res,
    )
    .await?;
    let items = res_res
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("invalid Notion response for resource query"))?;

    // Map to presentation: sequence (order), maybe text, else files (urls with names)
    let mut rows: Vec<Row> = Vec::new();
    for page in items {
        let props = page.get("properties").and_then(|v| v.as_object());
        let Some(props) = props else { continue };

        let ord = extract_title_number(props.get(&order_prop)).unwrap_or(0);
        let text = extract_rich_text(props.get(&text_prop));
        let files = extract_files(props.get(&media_prop));
        let kind = if text.is_some() { "text" } else { "media" };
        rows.push(Row {
            ord,
            kind: kind.to_string(),
            text,
            files,
            video_local_rel: None,
        });
    }

    // Build HTML
    let out_dir = PathBuf::from(cfg.app.resolved_data_dir()).join("html");
    let static_dir = out_dir.join("static");
    tokio::fs::create_dir_all(&static_dir)
        .await
        .with_context(|| format!("failed to create {}", static_dir.display()))?;

    // Ensure video directory exists and is empty before any downloads
    let video_dir = out_dir.join("video");
    if video_dir.exists() {
        tokio::fs::remove_dir_all(&video_dir)
            .await
            .with_context(|| format!("failed to clear {}", video_dir.display()))?;
    }
    tokio::fs::create_dir_all(&video_dir)
        .await
        .with_context(|| format!("failed to create {}", video_dir.display()))?;

    for r in rows.iter_mut() {
        if r.text.is_some() {
            continue;
        }

        // Enforce strict file rules. Invalid cases cause immediate error.
        match r.files.len() {
            0 => {
                return Err(anyhow!("row #{} has no files and no text (invalid)", r.ord));
            }
            1 => {
                let f = &r.files[0];
                if looks_like_image(&f.name) || looks_like_image_url(&f.url) {
                    // Image only: render via URL; nothing to download.
                } else if looks_like_video(&f.name) || looks_like_video_url(&f.url) {
                    // Video only: download to html/video/{order}.{ext}
                    let ext = derive_video_ext(&f.name, &f.url);
                    let file_name = format!("{}.{}", r.ord, ext);
                    let dest = video_dir.join(&file_name);
                    let mut need = true;
                    if let Ok(meta) = std::fs::metadata(&dest) {
                        if meta.len() > 0 {
                            need = false;
                        }
                    }
                    if need {
                        download_file_to(&http, &f.url, &dest)
                            .await
                            .with_context(|| format!("failed to download video {}", f.url))?;
                    }
                    r.video_local_rel = Some(format!("video/{}", file_name));
                } else {
                    return Err(anyhow!(
                        "row #{} has one file but not image/video: {}",
                        r.ord,
                        f.name
                    ));
                }
            }
            2 => {
                let first = &r.files[0];
                let second = &r.files[1];
                let first_is_img =
                    looks_like_image(&first.name) || looks_like_image_url(&first.url);
                let second_is_vid =
                    looks_like_video(&second.name) || looks_like_video_url(&second.url);
                if !(first_is_img && second_is_vid) {
                    return Err(anyhow!(
                        "row #{} with 2 files must be [image, video]; got: [{}, {}]",
                        r.ord,
                        first.name,
                        second.name
                    ));
                }
                // Ignore thumbnail; download video
                let ext = derive_video_ext(&second.name, &second.url);
                let file_name = format!("{}.{}", r.ord, ext);
                let dest = video_dir.join(&file_name);
                let mut need = true;
                if let Ok(meta) = std::fs::metadata(&dest) {
                    if meta.len() > 0 {
                        need = false;
                    }
                }
                if need {
                    download_file_to(&http, &second.url, &dest)
                        .await
                        .with_context(|| format!("failed to download video {}", second.url))?;
                }
                r.video_local_rel = Some(format!("video/{}", file_name));
            }
            _ => {
                return Err(anyhow!(
                    "row #{} has {} files (only 1 or 2 allowed)",
                    r.ord,
                    r.files.len()
                ));
            }
        }
    }

    let index_html = render_html(key, &rows);
    let index_path = out_dir.join("index.html");
    tokio::fs::write(&index_path, index_html)
        .await
        .with_context(|| format!("failed to write {}", index_path.display()))?;

    let style_css = DEFAULT_STYLE;
    let css_path = static_dir.join("style.css");
    tokio::fs::write(&css_path, style_css)
        .await
        .with_context(|| format!("failed to write {}", css_path.display()))?;

    println!("Wrote {} and {}", index_path.display(), css_path.display());

    println!("================================");
    println!(
        "Index full path: {}",
        absolute_path(&index_path).display()
    );
    println!(
        "Video full path: {}",
        absolute_path(&video_dir).display()
    );
    Ok(())
}

fn render_html(key: &str, rows: &[Row]) -> String {
    let mut body = String::new();
    for r in rows {
        let mut section = String::new();
        section.push_str(&format!(
            "<div class=\"row\"><div class=\"seq noselect\">#{}</div>",
            r.ord
        ));
        if let Some(t) = &r.text {
            section.push_str(&format!("<div class=\"text\">{}</div>", html_escape(t)));
        } else {
            // If a local video exists for this row, skip rendering any images (thumbnail).
            if r.video_local_rel.is_none() {
                for f in &r.files {
                    if looks_like_image(&f.name) || looks_like_image_url(&f.url) {
                        section.push_str(&format!(
                            "<img src=\"{}\" alt=\"{}\" />",
                            html_attr(&f.url),
                            html_attr(&f.name)
                        ));
                    }
                }
            }
            // Intentionally do not render videos in HTML; they are saved to html/video/ only.
        }
        section.push_str("</div>\n");
        body.push_str(&section);
    }

    format!(
        r#"<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{}</title>
    <link rel="stylesheet" href="static/style.css">
  </head>
  <body>
    <header>
      <h1 class="noselect">{}</h1>
    </header>
    <main>
      {}
    </main>
  </body>
</html>"#,
        html_escape(key),
        html_escape(key),
        body
    )
}

#[derive(Debug, Clone)]
struct Row {
    ord: i64,
    kind: String,
    text: Option<String>,
    files: Vec<FileEntry>,
    // If present, relative path under html/ pointing to downloaded video (e.g., "video/2.mp4")
    video_local_rel: Option<String>,
}

#[derive(Debug, Clone)]
struct FileEntry {
    name: String,
    url: String,
}

fn absolute_path(p: &std::path::Path) -> std::path::PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p),
        Err(_) => p.to_path_buf(),
    }
}

fn extract_title_number(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    let title = v.get("title")?.as_array()?;
    let text = title
        .iter()
        .filter_map(|t| t.get("plain_text").and_then(|s| s.as_str()))
        .collect::<String>();
    // Accept formats like "#12" or "12"
    let trimmed = text.trim();
    let digits = trimmed.trim_start_matches('#');
    digits.parse::<i64>().ok()
}

fn extract_rich_text(v: Option<&Value>) -> Option<String> {
    let v = v?;
    let arr = v.get("rich_text")?.as_array()?;
    let text = arr
        .iter()
        .filter_map(|t| t.get("plain_text").and_then(|s| s.as_str()))
        .collect::<String>();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn extract_files(v: Option<&Value>) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let Some(v) = v else { return out };
    let Some(arr) = v.get("files").and_then(|x| x.as_array()) else {
        return out;
    };
    for item in arr {
        let name = item
            .get("name")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let url = match item.get("type").and_then(|s| s.as_str()) {
            Some("external") => item
                .get("external")
                .and_then(|m| m.get("url"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string()),
            Some("file") => item
                .get("file")
                .and_then(|m| m.get("url"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string()),
            Some("file_upload") => item
                .get("file_upload")
                .and_then(|m| m.get("url"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
        if let Some(u) = url {
            out.push(FileEntry {
                name: if name.is_empty() { u.clone() } else { name },
                url: u,
            });
        }
    }
    out
}

async fn notion_post_json(
    http: &reqwest::Client,
    token: &str,
    version: &str,
    path: &str,
    body: Value,
) -> Result<Value> {
    let base = Url::parse("https://api.notion.com/")?;
    let url = base.join(path)?;
    let res = http
        .post(url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Notion-Version", version)
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(anyhow!("notion error {}: {}", status, text));
    }
    Ok(res.json::<Value>().await?)
}

fn looks_like_image(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".jpg")
        || n.ends_with(".jpeg")
        || n.ends_with(".png")
        || n.ends_with(".gif")
        || n.ends_with(".webp")
}
fn looks_like_video(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".mp4")
        || n.ends_with(".mov")
        || n.ends_with(".avi")
        || n.ends_with(".mkv")
        || n.ends_with(".webm")
}
fn looks_like_image_url(url: &str) -> bool {
    looks_like_image(url)
}
fn looks_like_video_url(url: &str) -> bool {
    looks_like_video(url)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn html_attr(s: &str) -> String {
    html_escape(s).replace('"', "&quot;")
}

const DEFAULT_STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --fg: #222;
  --bg: #fff;
  --muted: #666;
}

@media (prefers-color-scheme: dark) {
  :root {
    --fg: #eee;
    --bg: #121212;
    --muted: #aaa;
  }
}

html,
body {
  margin: 0;
  padding: 0;
  background: var(--bg);
  color: var(--fg);
  font: 14px/1.6 -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto,
        'Helvetica Neue', Arial, 'Noto Sans', 'Apple Color Emoji',
        'Segoe UI Emoji', 'Segoe UI Symbol', sans-serif;
}

header {
  padding: 16px;
  border-bottom: 1px solid #ddd4;
}

main {
  padding: 16px;
  max-width: 820px;
  margin: 0 auto;
}

.hint {
  color: var(--muted);
}

.row {
  margin: 8px 0 16px;
  padding-bottom: 10px;
  border-bottom: 1px dashed #ddd3;
}

.row .seq {
  font-weight: 600;
  color: var(--muted);
}

.row .text {
  white-space: pre-wrap;
}

img,
video {
  max-width: 100%;
  display: block;
  margin: 8px 0;
}

a.file {
  color: #0b7285;
  text-decoration: none;
  word-break: break-all;
}

.noselect {
  user-select: none;
  -webkit-user-select: none;
  -moz-user-select: none;
  -ms-user-select: none;
}

.nocopy {
  pointer-events: none;
  user-select: none;     
}
"#;

fn derive_video_ext(name: &str, url: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    for ext in ["mp4", "mov", "webm", "mkv", "avi"] {
        if lower.ends_with(&format!(".{}", ext)) {
            return ext;
        }
    }
    let lower_u = url.to_ascii_lowercase();
    for ext in ["mp4", "mov", "webm", "mkv", "avi"] {
        if lower_u.contains(&format!(".{}", ext)) {
            return ext;
        }
    }
    "mp4"
}

async fn download_file_to(http: &reqwest::Client, url: &str, dest: &std::path::Path) -> Result<()> {
    let start = std::time::Instant::now();
    println!("Downloading {}", dest.display());

    let res = http.get(url).send().await?;
    if !res.status().is_success() {
        return Err(anyhow!("download error {} for {}", res.status(), url));
    }
    let bytes = res.bytes().await?;
    if let Some(p) = dest.parent() {
        tokio::fs::create_dir_all(p).await.ok();
    }
    tokio::fs::write(dest, &bytes).await?;

    println!("Downloaded in {:?}ms", start.elapsed().as_millis());
    Ok(())
}

/// Build a Notion database query filter for a unique property.
/// Supports: title, rich_text, formula(string output).
fn build_unique_filter(prop_name: &str, prop_type: &str, key: &str) -> Value {
    match prop_type {
        "title" => json!({ "property": prop_name, "title": { "equals": key } }),
        "rich_text" => json!({ "property": prop_name, "rich_text": { "equals": key } }),
        // Assume string output for formula; adjust if your formula outputs number/date/checkbox
        "formula" => json!({ "property": prop_name, "formula": { "string": { "equals": key } } }),
        // Fallback: try rich_text equals as a best-effort
        _ => json!({ "property": prop_name, "rich_text": { "equals": key } }),
    }
}

/// Given a database schema and a string that may be a property name or id,
/// return the canonical property name.
fn resolve_prop_name(
    schema: &tg_watchbot::notion::model::RetrieveDatabaseResp,
    name_or_id: &str,
) -> Option<String> {
    if schema.properties.contains_key(name_or_id) {
        return Some(name_or_id.to_string());
    }
    for (k, v) in &schema.properties {
        if v.id == name_or_id {
            return Some(k.clone());
        }
    }
    None
}

fn resolve_prop_name_and_type(
    schema: &tg_watchbot::notion::model::RetrieveDatabaseResp,
    name_or_id: &str,
) -> Option<(String, String)> {
    if let Some(p) = schema.properties.get(name_or_id) {
        return Some((name_or_id.to_string(), p.typ.clone()));
    }
    for (k, v) in &schema.properties {
        if v.id == name_or_id {
            return Some((k.clone(), v.typ.clone()));
        }
    }
    None
}
