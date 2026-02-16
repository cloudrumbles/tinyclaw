use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    pub name: String,
    pub bot_id: String,
    pub provider: String,
    pub model: String,
    /// Idle timeout in seconds. If the CLI produces no output for this
    /// long, the process is killed. Defaults to 180s if not set.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Persona ID. Defaults to bot_id if not set.
    /// Persona defines the bot's personality (soul.md) and skills.
    #[serde(default)]
    pub persona: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub channels: Option<ChannelsConfig>,
    pub bot: Option<BotConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsConfig {
    pub telegram: Option<TelegramChannelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    pub bot_token: Option<String>,
}


/// Claude model name → full model ID
pub fn resolve_claude_model(model: &str) -> &str {
    match model {
        "sonnet" | "claude-sonnet-4-5" => "claude-sonnet-4-5",
        "opus" | "claude-opus-4-6" => "claude-opus-4-6",
        other => other,
    }
}

