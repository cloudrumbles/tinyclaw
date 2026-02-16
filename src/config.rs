use std::path::{Path, PathBuf};

use tracing::info;

use crate::types::{BotConfig, ChannelsConfig, Settings, TelegramChannelConfig};

/// Resolve TINYCLAW_HOME: TINYCLAW_HOME env var > local `.tinyclaw/` > `~/.tinyclaw/`.
pub fn resolve_tinyclaw_home() -> PathBuf {
    if let Ok(home) = std::env::var("TINYCLAW_HOME") {
        return PathBuf::from(home);
    }

    let local = PathBuf::from(".tinyclaw");
    if local.join("settings.json").exists() {
        return local;
    }

    dirs::home_dir()
        .map(|h| h.join(".tinyclaw"))
        .unwrap_or_else(|| PathBuf::from(".tinyclaw"))
}

pub fn settings_file(tinyclaw_home: &Path) -> PathBuf {
    tinyclaw_home.join("settings.json")
}

pub fn files_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("files")
}

pub fn reset_flag(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("reset_flag")
}

pub fn persona_dir(tinyclaw_home: &Path, persona_id: &str) -> PathBuf {
    tinyclaw_home.join(persona_id)
}

/// Resolve the workspace directory for the bot: ~/{bot_id}-workspace/
pub fn bot_workspace(bot_id: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("{bot_id}-workspace"))
}

/// Compute the Claude CLI project directory path hash for a given workspace dir.
/// Claude CLI uses the canonical path with `/` replaced by `-` and leading `-`.
pub fn claude_project_hash(workspace_dir: &Path) -> String {
    let canonical = std::fs::canonicalize(workspace_dir)
        .unwrap_or_else(|_| workspace_dir.to_path_buf());
    canonical.to_string_lossy().replace('/', "-")
}

/// Bootstrap tinyclaw home directory with default config files (only writes files
/// that don't already exist). Per-persona dirs are created by ensure_persona.
pub fn bootstrap(tinyclaw_home: &Path) {
    std::fs::create_dir_all(tinyclaw_home).ok();

    // Write default settings.json if missing
    let settings_path = settings_file(tinyclaw_home);
    if !settings_path.exists() {
        let settings = hardcoded_settings();
        if let Ok(json) = serde_json::to_string_pretty(&settings) {
            std::fs::write(&settings_path, json).ok();
            info!("Bootstrapped settings.json");
        }
    }
}

/// Hardcoded default settings baked into the binary.
fn hardcoded_settings() -> Settings {
    Settings {
        channels: Some(ChannelsConfig {
            telegram: Some(TelegramChannelConfig { bot_token: None }),
        }),
        bot: Some(BotConfig {
            name: "Sultana".to_string(),
            bot_id: "sultana".to_string(),
            provider: "anthropic".to_string(),
            model: "opus".to_string(),
            timeout: None,
            persona: None,
        }),
    }
}

/// Read and parse settings.json. Returns default Settings on any error.
pub fn get_settings(tinyclaw_home: &Path) -> Settings {
    let path = settings_file(tinyclaw_home);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return default_settings();
    };

    serde_json::from_str(&data).unwrap_or_else(|_| default_settings())
}

fn default_settings() -> Settings {
    Settings {
        channels: None,
        bot: None,
    }
}

/// Get the bot config from settings. Returns a hardcoded default if not set.
pub fn get_bot_config(settings: &Settings) -> BotConfig {
    settings.bot.clone().unwrap_or_else(|| BotConfig {
        name: "Sultana".to_string(),
        bot_id: "sultana".to_string(),
        provider: "anthropic".to_string(),
        model: "opus".to_string(),
        timeout: None,
        persona: None,
    })
}
