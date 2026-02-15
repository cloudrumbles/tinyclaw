use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{AgentConfig, Settings, TeamConfig};

/// Resolve TINYCLAW_HOME: prefer local `.tinyclaw/` if it has settings.json,
/// otherwise fall back to `~/.tinyclaw/`.
pub fn resolve_tinyclaw_home() -> PathBuf {
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

pub fn chats_dir(tinyclaw_home: &Path) -> PathBuf {
    tinyclaw_home.join("chats")
}

pub fn pairing_file(tinyclaw_home: &Path) -> PathBuf {
    tinyclaw_home.join("pairing.json")
}

pub fn files_dir(tinyclaw_home: &Path) -> PathBuf {
    tinyclaw_home.join("files")
}

pub fn reset_flag(tinyclaw_home: &Path) -> PathBuf {
    tinyclaw_home.join("reset_flag")
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
        workspace: None,
        channels: None,
        agents: None,
        teams: None,
        monitoring: None,
    }
}

/// Get all configured agents. Agents must be defined in settings.json.
pub fn get_agents(settings: &Settings) -> HashMap<String, AgentConfig> {
    settings.agents.clone().unwrap_or_default()
}

/// Get all configured teams.
pub fn get_teams(settings: &Settings) -> HashMap<String, TeamConfig> {
    settings.teams.clone().unwrap_or_default()
}

/// Resolve the workspace path from settings, defaulting to ~/tinyclaw-workspace.
pub fn workspace_path(settings: &Settings) -> PathBuf {
    settings
        .workspace
        .as_ref()
        .and_then(|w| w.path.as_deref())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("tinyclaw-workspace")
        })
}
