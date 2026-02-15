mod agent_setup;
mod config;
mod cron_inbox;
mod errors;
mod heartbeat;
mod invoke;
mod logging;
mod pairing;
mod queue;
mod routing;
mod telegram;
mod types;

use std::path::PathBuf;

use tokio::sync::mpsc;
use tracing::{info, error};

#[tokio::main]
async fn main() {
    // Load .env file (if present)
    dotenvy::dotenv().ok();

    let tinyclaw_home = config::resolve_tinyclaw_home();
    std::fs::create_dir_all(&tinyclaw_home).ok();

    // Initialize logging (must hold guard for entire program lifetime)
    let _log_guard = logging::init_logging(&tinyclaw_home);

    info!("TinyClaw starting (Rust)");
    info!("TINYCLAW_HOME: {}", tinyclaw_home.display());

    // Ensure required directories exist
    let logs_dir = tinyclaw_home.join("logs");
    let files_dir = tinyclaw_home.join("files");
    std::fs::create_dir_all(&logs_dir).ok();
    std::fs::create_dir_all(&files_dir).ok();

    // Load settings to get bot token
    let settings = config::get_settings(&tinyclaw_home);

    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .or_else(|_| {
            settings
                .channels
                .as_ref()
                .and_then(|c| c.telegram.as_ref())
                .and_then(|t| t.bot_token.clone())
                .ok_or(std::env::VarError::NotPresent)
        })
        .unwrap_or_else(|_| {
            error!("TELEGRAM_BOT_TOKEN not set in environment or settings.json");
            std::process::exit(1);
        });

    let webhook_url = std::env::var("WEBHOOK_URL").unwrap_or_else(|_| {
        error!("WEBHOOK_URL not set in environment");
        std::process::exit(1);
    });

    // Log agent/team configuration
    let agents = config::get_agents(&settings);
    let teams = config::get_teams(&settings);

    info!("Loaded {} agent(s):", agents.len());
    for (id, agent) in &agents {
        info!(
            "  {id}: {} [{}/{}] cwd={}",
            agent.name, agent.provider, agent.model, agent.working_directory
        );
    }
    if !teams.is_empty() {
        info!("Loaded {} team(s):", teams.len());
        for (id, team) in &teams {
            info!(
                "  {id}: {} [agents: {}] leader={}",
                team.name,
                team.agents.join(", "),
                team.leader_agent
            );
        }
    }

    // Resolve skills source directory
    let skills_source = find_skills_source();

    // Create channels
    // incoming: telegram + cron inbox → queue processor
    // outgoing: queue processor → telegram
    let (incoming_tx, incoming_rx) = mpsc::channel::<queue::IncomingMessage>(256);
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<queue::OutgoingMessage>(256);

    // Spawn queue processor task
    let qp_home = tinyclaw_home.clone();
    let qp_skills = skills_source.clone();
    let queue_handle = tokio::spawn(async move {
        queue::run_queue_processor(qp_home, qp_skills, incoming_rx, outgoing_tx).await;
    });

    // Spawn cron inbox watcher (handles heartbeat + scheduled jobs via cron-job.org)
    let ci_home = tinyclaw_home.clone();
    let ci_tx = incoming_tx.clone();
    let cron_inbox_handle = tokio::spawn(async move {
        cron_inbox::run_cron_inbox(ci_home, ci_tx).await;
    });

    // Run telegram (blocks — it runs the teloxide dispatcher)
    let tg_home = tinyclaw_home.clone();
    telegram::run_telegram(tg_home, bot_token, webhook_url, incoming_tx, outgoing_rx).await;

    // If telegram exits, shut down everything
    info!("Telegram task exited, shutting down...");
    queue_handle.abort();
    cron_inbox_handle.abort();
}

/// Find the .agents/skills directory relative to the binary or current dir.
fn find_skills_source() -> Option<PathBuf> {
    // Check next to the binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let skills = exe_dir.join(".agents").join("skills");
            if skills.exists() {
                return Some(skills);
            }
        }
    }

    // Check current working directory
    let cwd_skills = PathBuf::from(".agents/skills");
    if cwd_skills.exists() {
        return Some(std::fs::canonicalize(cwd_skills).ok()?);
    }

    None
}
