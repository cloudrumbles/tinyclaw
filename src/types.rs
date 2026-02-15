use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub working_directory: String,
    /// Per-agent idle timeout in seconds. If the CLI produces no output for this
    /// long, the process is killed. Defaults to 60s if not set.
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    pub agents: Vec<String>,
    pub leader_agent: String,
}

#[derive(Debug, Clone)]
pub struct ChainStep {
    pub agent_id: String,
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub workspace: Option<WorkspaceConfig>,
    pub channels: Option<ChannelsConfig>,
    pub agents: Option<HashMap<String, AgentConfig>>,
    pub teams: Option<HashMap<String, TeamConfig>>,
    pub monitoring: Option<MonitoringConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsConfig {
    pub telegram: Option<TelegramChannelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    pub bot_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    pub heartbeat_interval: Option<u64>,
}

/// Claude model name → full model ID
pub fn resolve_claude_model(model: &str) -> &str {
    match model {
        "sonnet" | "claude-sonnet-4-5" => "claude-sonnet-4-5",
        "opus" | "claude-opus-4-6" => "claude-opus-4-6",
        other => other,
    }
}

/// Codex model name → full model ID
pub fn resolve_codex_model(model: &str) -> &str {
    match model {
        "gpt-5.2" => "gpt-5.2",
        "gpt-5.3-codex" => "gpt-5.3-codex",
        other => other,
    }
}
