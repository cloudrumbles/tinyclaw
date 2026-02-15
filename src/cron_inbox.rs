use std::path::PathBuf;

use tokio::sync::mpsc;
use tracing::{info, warn, error};

use crate::heartbeat;
use crate::queue::IncomingMessage;

/// Watch the cron-inbox directory for JSON trigger files.
/// Heartbeat triggers are routed through the heartbeat module.
/// All other triggers are injected as cron job messages.
pub async fn run_cron_inbox(
    tinyclaw_home: PathBuf,
    incoming_tx: mpsc::Sender<IncomingMessage>,
) {
    let inbox_dir = tinyclaw_home.join("cron-inbox");
    std::fs::create_dir_all(&inbox_dir).ok();

    info!("Cron inbox watcher started: {}", inbox_dir.display());

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let entries = match std::fs::read_dir(&inbox_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read cron inbox file {}: {e}", path.display());
                    continue;
                }
            };

            let parsed: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to parse cron inbox file {}: {e}", path.display());
                    std::fs::remove_file(&path).ok();
                    continue;
                }
            };

            // Remove the trigger file first
            if let Err(e) = std::fs::remove_file(&path) {
                warn!("Failed to remove cron inbox file {}: {e}", path.display());
            }

            let job_name = parsed["name"].as_str().unwrap_or("Cron Job");
            let agent_id = parsed["agent_id"].as_str().unwrap_or("sultana");
            let chat_id = parsed["chat_id"].as_i64();
            let is_heartbeat = job_name.eq_ignore_ascii_case("heartbeat");

            if is_heartbeat {
                // Route through heartbeat module for vitals + health checks
                info!("Cron inbox: heartbeat for @{agent_id}");
                heartbeat::beat(&tinyclaw_home, &incoming_tx, agent_id, chat_id).await;
            } else {
                // Regular cron job
                let job_id = parsed["job_id"].as_str().unwrap_or("unknown");
                let prompt = parsed["prompt"].as_str().unwrap_or("");

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let now_millis = now.as_millis() as u64;
                let rand_val: u32 = rand::random();

                let message = format!("@{agent_id} [CRON JOB: {job_name}]\n{prompt}");
                let message_id = format!("cron_{job_id}_{now_millis}_{rand_val:08x}");

                let msg = IncomingMessage {
                    channel: "telegram".into(),
                    sender: "Cron".into(),
                    sender_id: chat_id.map(|c| c.to_string()).unwrap_or_else(|| format!("cron_{job_id}")),
                    message,
                    timestamp: now_millis,
                    message_id: message_id.clone(),
                    agent: Some(agent_id.to_string()),
                    files: vec![],
                    chat_id,
                    reply_to_message_id: None,
                };

                info!("Cron inbox: job {job_id} ({job_name}) -> @{agent_id}");

                if incoming_tx.send(msg).await.is_err() {
                    error!("Failed to inject cron message for job {job_id}");
                }
            }
        }
    }
}
