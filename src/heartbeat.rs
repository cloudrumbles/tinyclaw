use std::path::PathBuf;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config;
use crate::queue::IncomingMessage;

/// Process a heartbeat trigger from the cron inbox.
/// Reads the agent's heartbeat.md prompt and injects a message into the queue.
/// Unlike a timer-based heartbeat, this is invoked externally (via cron-job.org)
/// so it works even when the Sprite has been asleep.
pub async fn beat(
    tinyclaw_home: &PathBuf,
    incoming_tx: &mpsc::Sender<IncomingMessage>,
    agent_id: &str,
    chat_id: Option<i64>,
) {
    let settings = config::get_settings(tinyclaw_home);
    let workspace = config::workspace_path(&settings);
    let agents = config::get_agents(&settings);

    let agent = match agents.get(agent_id) {
        Some(a) => a,
        None => {
            warn!("Heartbeat: unknown agent '{agent_id}'");
            return;
        }
    };

    // Resolve active workspace directory
    let ws_name = agent.workspace.as_deref()
        .map(String::from)
        .unwrap_or_else(|| config::active_workspace(&workspace, agent_id));
    let agent_dir = config::agent_workspace_dir(&workspace, agent_id, &ws_name);

    // Read agent-specific heartbeat prompt, or use default
    let heartbeat_file = agent_dir.join("heartbeat.md");
    let prompt = if heartbeat_file.exists() {
        std::fs::read_to_string(&heartbeat_file)
            .unwrap_or_else(|_| default_prompt())
    } else {
        default_prompt()
    };

    // Check system vitals
    let vitals = check_vitals(tinyclaw_home);

    let full_message = format!(
        "@{agent_id} [HEARTBEAT]\n{vitals}\n{prompt}"
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let now_millis = now.as_millis() as u64;
    let rand_val: u32 = rand::random();
    let message_id = format!("heartbeat_{agent_id}_{now_millis}_{rand_val:08x}");

    let msg = IncomingMessage {
        channel: "telegram".into(),
        sender: "Heartbeat".into(),
        sender_id: chat_id.map(|c| c.to_string()).unwrap_or_else(|| format!("heartbeat_{agent_id}")),
        message: full_message,
        timestamp: now_millis,
        message_id: message_id.clone(),
        agent: Some(agent_id.to_string()),
        files: vec![],
        chat_id,
        reply_to_message_id: None,
    };

    if incoming_tx.send(msg).await.is_err() {
        warn!("Heartbeat: failed to send for @{agent_id}");
    } else {
        info!("Heartbeat: sent for @{agent_id}");
    }
}

/// Check system vitals and return a summary string.
fn check_vitals(tinyclaw_home: &PathBuf) -> String {
    let mut lines = Vec::new();

    // Queue health: check for stuck messages in processing/
    let processing_dir = tinyclaw_home.join("queue").join("processing");
    if processing_dir.exists() {
        let count = std::fs::read_dir(&processing_dir)
            .map(|d| d.count())
            .unwrap_or(0);
        if count > 0 {
            lines.push(format!("⚠ {count} message(s) stuck in processing queue"));
        }
    }

    // Disk usage
    if let Ok(output) = std::process::Command::new("df")
        .args(["-h", "/home/sprite"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = stdout.lines().nth(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                let use_pct = parts[4];
                let avail = parts[3];
                lines.push(format!("Disk: {use_pct} used, {avail} available"));
            }
        }
    }

    // Cron jobs count
    let jobs_file = tinyclaw_home.join("cron-jobs.json");
    if jobs_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&jobs_file) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                let count = parsed["jobs"].as_object().map(|j| j.len()).unwrap_or(0);
                lines.push(format!("{count} cron job(s) registered"));
            }
        }
    }

    // Log file size
    let log_file = tinyclaw_home.join("logs").join("tinyclaw.log");
    if let Ok(meta) = std::fs::metadata(&log_file) {
        let size_mb = meta.len() as f64 / (1024.0 * 1024.0);
        if size_mb > 10.0 {
            lines.push(format!("⚠ Log file is {size_mb:.1}MB"));
        }
    }

    if lines.is_empty() {
        "System vitals: all clear.".to_string()
    } else {
        format!("System vitals:\n{}", lines.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n"))
    }
}

fn default_prompt() -> String {
    "Quick status check: Any pending tasks? Keep response brief.".into()
}
