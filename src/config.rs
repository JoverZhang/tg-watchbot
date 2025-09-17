//! Configuration loader and validator for the Telegram→Notion bot.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML parse error: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("Invalid configuration: {0}")]
    Invalid(&'static str),
}

/// Root configuration struct mirroring the YAML schema exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub app: App,
    pub telegram: Telegram,
    pub notion: Notion,
}

/// App-level settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct App {
    pub data_dir: String,
    pub poll_interval_ms: u64,
    pub max_backoff_seconds: u64,
}

/// Telegram bot settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Telegram {
    pub bot_token: String,
    pub allowed_users: Vec<i64>,
}

/// Notion API settings and database mappings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Notion {
    pub token: String,
    pub version: String,
    pub databases: Databases,
}

/// Database mapping configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Databases {
    pub main: DbMain,
    pub resource: DbResource,
}

/// Main (batch) database mapping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbMain {
    pub id: String,
    pub fields: DbMainFields,
}

/// Fields for the main database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbMainFields {
    pub title: String,
}

/// Resource database mapping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbResource {
    pub id: String,
    pub fields: DbResourceFields,
}

/// Fields for the resource database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbResourceFields {
    pub relation: String,
    pub order: String,
    pub text: String,
    pub media: String,
}

impl Config {
    /// Ensure required directories exist (creates `app.data_dir` if missing).
    pub fn ensure_dirs(&self) -> Result<(), std::io::Error> {
        if self.app.data_dir.trim().is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.app.data_dir)
    }
}

/// Load configuration from a YAML file and validate it.
/// - If `path` is None, uses `config.yaml` in the current working directory.
pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
    let path = path.unwrap_or_else(|| Path::new("config.yaml"));
    let content = fs::read_to_string(path)?;
    let cfg: Config = serde_yaml::from_str(&content)?;
    validate(&cfg)?;
    Ok(cfg)
}

/// Validate a configuration instance.
fn validate(cfg: &Config) -> Result<(), ConfigError> {
    if cfg.app.data_dir.trim().is_empty() {
        return Err(ConfigError::Invalid("app.data_dir must be non-empty"));
    }
    if cfg.app.poll_interval_ms == 0 {
        return Err(ConfigError::Invalid("app.poll_interval_ms must be > 0"));
    }
    // max_backoff_seconds is u64; it's inherently >= 0

    if cfg.telegram.bot_token.trim().is_empty() {
        return Err(ConfigError::Invalid("telegram.bot_token must be non-empty"));
    }

    if cfg.notion.token.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.token must be non-empty"));
    }
    if cfg.notion.version.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.version must be non-empty"));
    }

    if cfg.notion.databases.main.id.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.databases.main.id must be non-empty"));
    }
    if cfg.notion.databases.main.fields.title.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.databases.main.fields.title must be non-empty"));
    }

    if cfg.notion.databases.resource.id.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.databases.resource.id must be non-empty"));
    }
    let rf = &cfg.notion.databases.resource.fields;
    if rf.relation.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.databases.resource.fields.relation must be non-empty"));
    }
    if rf.order.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.databases.resource.fields.order must be non-empty"));
    }
    if rf.text.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.databases.resource.fields.text must be non-empty"));
    }
    if rf.media.trim().is_empty() {
        return Err(ConfigError::Invalid("notion.databases.resource.fields.media must be non-empty"));
    }

    Ok(())
}

/// Returns the exact example YAML content requested.
pub fn example() -> &'static str {
    // Keep exactly as provided.
    r#"app:
  data_dir: "./data"
  poll_interval_ms: 500
  max_backoff_seconds: 60

telegram:
  bot_token: "YOUR_TELEGRAM_BOT_TOKEN"
  allowed_users:
    - 123456789

notion:
  token: "YOUR_NOTION_INTEGRATION_TOKEN"
  version: "2022-06-28"

  databases:
    main:
      id: "NOTION_MAIN_DATABASE_ID"
      fields:
        title: "标题"
    resource:
      id: "NOTION_RESOURCE_DATABASE_ID"
      fields:
        relation: "关联主表"
        order: "序号"
        text: "文本"
        media: "图片/视频"
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::io::Write;

    #[test]
    fn parse_example_ok() {
        let cfg: Config = serde_yaml::from_str(example()).unwrap();
        validate(&cfg).unwrap();
    }

    #[test]
    fn invalid_bot_token() {
        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        let mut cfg = cfg;
        cfg.telegram.bot_token = "".into();
        let err = validate(&cfg).unwrap_err();
        match err { ConfigError::Invalid(msg) => assert!(msg.contains("telegram.bot_token")), _ => panic!("wrong error") }
    }

    #[test]
    fn invalid_notion_db_ids() {
        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.notion.databases.main.id = "".into();
        let err = validate(&cfg).unwrap_err();
        match err { ConfigError::Invalid(msg) => assert!(msg.contains("main.id")), _ => panic!("wrong error") }

        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.notion.databases.resource.id = "".into();
        let err = validate(&cfg).unwrap_err();
        match err { ConfigError::Invalid(msg) => assert!(msg.contains("resource.id")), _ => panic!("wrong error") }
    }

    #[test]
    fn invalid_field_mappings() {
        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.notion.databases.main.fields.title = "".into();
        let err = validate(&cfg).unwrap_err();
        match err { ConfigError::Invalid(msg) => assert!(msg.contains("fields.title")), _ => panic!("wrong error") }

        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.notion.databases.resource.fields.relation = "".into();
        assert!(matches!(validate(&cfg), Err(ConfigError::Invalid(_))));

        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.notion.databases.resource.fields.order = "".into();
        assert!(matches!(validate(&cfg), Err(ConfigError::Invalid(_))));

        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.notion.databases.resource.fields.text = "".into();
        assert!(matches!(validate(&cfg), Err(ConfigError::Invalid(_))));

        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.notion.databases.resource.fields.media = "".into();
        assert!(matches!(validate(&cfg), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn ensure_dirs_creates_data_dir() {
        let td = tempdir().unwrap();
        let data_path = td.path().join("data");
        let mut cfg: Config = serde_yaml::from_str(example()).unwrap();
        cfg.app.data_dir = data_path.to_string_lossy().to_string();
        cfg.ensure_dirs().unwrap();
        assert!(data_path.exists());
    }

    #[test]
    fn load_from_file_ok() {
        let td = tempdir().unwrap();
        let p = td.path().join("config.yaml");
        fs::write(&p, example()).unwrap();
        let cfg = load(Some(&p)).unwrap();
        assert_eq!(cfg.telegram.allowed_users, vec![123456789]);
    }
}
