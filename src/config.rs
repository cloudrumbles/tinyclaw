use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::pairing::{PairingApprovedEntry, PairingState, save_pairing_state};
use crate::types::{
    AgentConfig, ChannelsConfig, MonitoringConfig, Settings, TeamConfig, TelegramChannelConfig,
    WorkspaceConfig,
};

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

/// Bootstrap tinyclaw home directory with all required subdirectories
/// and default config files (only writes files that don't already exist).
pub fn bootstrap(tinyclaw_home: &Path) {
    // Create all required directories
    for subdir in &["logs", "files", "chats", "cron-inbox"] {
        std::fs::create_dir_all(tinyclaw_home.join(subdir)).ok();
    }

    // Write default settings.json if missing
    let settings_path = settings_file(tinyclaw_home);
    if !settings_path.exists() {
        let settings = hardcoded_settings();
        if let Ok(json) = serde_json::to_string_pretty(&settings) {
            std::fs::write(&settings_path, json).ok();
            info!("Bootstrapped settings.json");
        }
    }

    // Write default pairing.json if missing
    let pairing_path = pairing_file(tinyclaw_home);
    if !pairing_path.exists() {
        let state = hardcoded_pairing();
        let _ = save_pairing_state(&pairing_path, &state);
        info!("Bootstrapped pairing.json");
    }
}

/// Hardcoded default settings baked into the binary.
fn hardcoded_settings() -> Settings {
    let mut agents = HashMap::new();
    agents.insert(
        "sultana".to_string(),
        AgentConfig {
            name: "Sultana".to_string(),
            provider: "anthropic".to_string(),
            model: "opus".to_string(),
            working_directory: "sultana".to_string(),
            timeout: None,
        },
    );

    Settings {
        workspace: Some(WorkspaceConfig { path: None }),
        channels: Some(ChannelsConfig {
            telegram: Some(TelegramChannelConfig { bot_token: None }),
        }),
        agents: Some(agents),
        teams: None,
        monitoring: Some(MonitoringConfig {
            heartbeat_interval: Some(7200),
        }),
    }
}

/// Hardcoded default pairing with Shah pre-approved.
fn hardcoded_pairing() -> PairingState {
    PairingState {
        pending: vec![],
        approved: vec![PairingApprovedEntry {
            channel: "telegram".to_string(),
            sender_id: "525365593".to_string(),
            sender: "Shah".to_string(),
            approved_at: 0,
            approved_code: Some("HARDCODED".to_string()),
        }],
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
