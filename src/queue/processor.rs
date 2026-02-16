use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use regex::Regex;
use tokio::sync::{mpsc, Semaphore};
use tracing::{info, error};

use crate::config;
use crate::invoke::invoke_agent;
use crate::queue::channels::{IncomingMessage, OutgoingMessage};
use crate::routing::{
    agent_reset_flag, extract_teammate_mentions, find_team_for_agent, parse_agent_routing,
};
use crate::types::{AgentConfig, ChainStep, TeamConfig};

/// Run the queue processor task. Reads from `incoming_rx`, writes to `outgoing_tx`.
/// `incoming_tx` allows re-injecting messages for async teammate dispatch.
pub async fn run_queue_processor(
    tinyclaw_home: PathBuf,
    skills_source: Option<PathBuf>,
    mut incoming_rx: mpsc::Receiver<IncomingMessage>,
    outgoing_tx: mpsc::Sender<OutgoingMessage>,
    incoming_tx: mpsc::Sender<IncomingMessage>,
) {
    info!("Queue processor started");

    // Per-agent semaphores: ensures messages to the same agent are processed sequentially
    let agent_locks: Arc<DashMap<String, Arc<Semaphore>>> = Arc::new(DashMap::new());

    while let Some(msg) = incoming_rx.recv().await {
        let home = tinyclaw_home.clone();
        let tx = outgoing_tx.clone();
        let inject_tx = incoming_tx.clone();
        let locks = agent_locks.clone();
        let skills = skills_source.clone();

        tokio::spawn(async move {
            let t0 = std::time::Instant::now();

            // Peek at the target agent to determine which lock to acquire
            let settings = config::get_settings(&home);
            let agents = config::get_agents(&settings);
            let teams = config::get_teams(&settings);

            let target_agent = peek_agent_id(&msg, &agents, &teams);
            info!("[timing] config+peek: {:?}", t0.elapsed());

            // Acquire per-agent semaphore (sequential processing per agent)
            let sem = locks
                .entry(target_agent.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone();
            let t1 = std::time::Instant::now();
            let _permit = sem.acquire().await.unwrap();
            info!("[timing] semaphore wait: {:?}", t1.elapsed());

            // Send typing start
            if let Some(chat_id) = msg.chat_id {
                let _ = tx
                    .send(OutgoingMessage::TypingStart {
                        message_id: msg.message_id.clone(),
                        chat_id,
                    })
                    .await;
            }

            let t2 = std::time::Instant::now();
            let result = process_message(&home, skills.as_deref(), &msg, &tx, &inject_tx).await;
            info!("[timing] process_message: {:?}", t2.elapsed());

            // Send typing stop
            let _ = tx
                .send(OutgoingMessage::TypingStop {
                    message_id: msg.message_id.clone(),
                })
                .await;

            match result {
                Ok((response, agent_id, files, miniapp)) => {
                    let _ = tx
                        .send(OutgoingMessage::Response {
                            channel: msg.channel.clone(),
                            sender: msg.sender.clone(),
                            message: response,
                            original_message: msg.message.clone(),
                            timestamp: now_millis(),
                            message_id: msg.message_id.clone(),
                            agent: Some(agent_id),
                            files,
                            chat_id: msg.chat_id,
                            reply_to_message_id: msg.reply_to_message_id,
                            miniapp,
                        })
                        .await;
                }
                Err(e) => {
                    error!("Processing error: {e}");
                    let _ = tx
                        .send(OutgoingMessage::Response {
                            channel: msg.channel.clone(),
                            sender: msg.sender.clone(),
                            message: "Sorry, I encountered an error processing your request."
                                .into(),
                            original_message: msg.message.clone(),
                            timestamp: now_millis(),
                            message_id: msg.message_id.clone(),
                            agent: None,
                            files: vec![],
                            chat_id: msg.chat_id,
                            reply_to_message_id: msg.reply_to_message_id,
                            miniapp: None,
                        })
                        .await;
                }
            }
        });
    }

    info!("Queue processor shutting down");
}

/// Peek at a message to determine target agent without processing it.
fn peek_agent_id(
    msg: &IncomingMessage,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
) -> String {
    if let Some(ref pre_routed) = msg.agent {
        if agents.contains_key(pre_routed) {
            return pre_routed.clone();
        }
    }
    let routing = parse_agent_routing(&msg.message, agents, teams);
    routing.agent_id
}

/// Process a single message through agent invocation (and async team dispatch if applicable).
/// Returns (response_text, agent_id, files, miniapp).
async fn process_message(
    tinyclaw_home: &Path,
    skills_source: Option<&Path>,
    msg: &IncomingMessage,
    outgoing_tx: &mpsc::Sender<OutgoingMessage>,
    incoming_tx: &mpsc::Sender<IncomingMessage>,
) -> anyhow::Result<(String, String, Vec<String>, Option<(String, String)>)> {
    let settings = config::get_settings(tinyclaw_home);
    let workspace = config::workspace_path(&settings);
    let agents = config::get_agents(&settings);
    let teams = config::get_teams(&settings);

    // Set up status update channel for streaming tool status to Telegram.
    // A spinning braille character animates every 10s to show the bot is alive,
    // plus elapsed time so the user can see progress.
    let (status_tx, mut status_rx) = mpsc::channel::<String>(16);
    if let Some(chat_id) = msg.chat_id {
        let fwd_tx = outgoing_tx.clone();
        let fwd_message_id = msg.message_id.clone();
        tokio::spawn(async move {
            const SPINNER: &[char] = &['◐', '◑'];
            let started = std::time::Instant::now();
            let mut last_status = String::new();
            let mut frame: usize = 0;
            loop {
                tokio::select! {
                    status = status_rx.recv() => {
                        match status {
                            Some(s) => {
                                last_status = s;
                                frame = frame.wrapping_add(1);
                                let spinner = SPINNER[frame % SPINNER.len()];
                                let elapsed = started.elapsed().as_secs();
                                let display = format!("{spinner} {last_status} ({elapsed}s)");
                                let _ = fwd_tx
                                    .send(OutgoingMessage::StatusUpdate {
                                        message_id: fwd_message_id.clone(),
                                        chat_id,
                                        status: display,
                                    })
                                    .await;
                            }
                            None => break,
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                        if !last_status.is_empty() {
                            frame = frame.wrapping_add(1);
                            let spinner = SPINNER[frame % SPINNER.len()];
                            let elapsed = started.elapsed().as_secs();
                            let display = format!("{spinner} {last_status} ({elapsed}s)");
                            let _ = fwd_tx
                                .send(OutgoingMessage::StatusUpdate {
                                    message_id: fwd_message_id.clone(),
                                    chat_id,
                                    status: display,
                                })
                                .await;
                        }
                    }
                }
            }
        });
    }

    let raw_message = &msg.message;

    // Route message
    let (mut agent_id, routed_message, is_team_routed) = if let Some(ref pre) = msg.agent {
        if agents.contains_key(pre) {
            (pre.clone(), raw_message.clone(), false)
        } else {
            let r = parse_agent_routing(raw_message, &agents, &teams);
            (r.agent_id, r.message, r.is_team)
        }
    } else {
        let r = parse_agent_routing(raw_message, &agents, &teams);
        (r.agent_id, r.message, r.is_team)
    };

    // Prepend timestamp in SGT (UTC+8) so the agent knows when the message was sent
    let sgt = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let ts = chrono::Utc::now().with_timezone(&sgt);
    let message = format!("[{}]\n{}", ts.format("%-d %b %Y, %-I:%M %p SGT"), routed_message);

    // Easter egg: multiple agents across teams
    if agent_id == "error" {
        return Ok((message, "error".into(), vec![], None));
    }

    // Fallback to default, then first available
    if !agents.contains_key(&agent_id) {
        agent_id = "default".into();
    }
    if !agents.contains_key(&agent_id) {
        agent_id = agents.keys().next().cloned().unwrap_or("default".into());
    }

    let agent = agents
        .get(&agent_id)
        .ok_or_else(|| anyhow::anyhow!("No agents configured"))?;

    info!(
        "Routing to agent: {} ({agent_id}) [{}/{}]",
        agent.name, agent.provider, agent.model
    );

    // Determine team context
    let team_context = if is_team_routed {
        teams
            .iter()
            .find(|(_, t)| t.leader_agent == agent_id && t.agents.contains(&agent_id))
            .map(|(id, t)| (id.clone(), t.clone()))
    } else {
        find_team_for_agent(&agent_id, &teams).map(|(id, t): (&String, &TeamConfig)| (id.clone(), t.clone()))
    };

    // Check reset flags
    let global_reset = config::reset_flag(tinyclaw_home);
    let agent_reset = agent_reset_flag(&agent_id, &workspace);
    let should_reset = global_reset.exists() || agent_reset.exists();

    if should_reset {
        if global_reset.exists() {
            std::fs::remove_file(&global_reset).ok();
        }
        if agent_reset.exists() {
            std::fs::remove_file(&agent_reset).ok();
        }
    }

    let mut all_files: Vec<String> = Vec::new();
    let file_ref_re = Regex::new(r"\[send_file:\s*([^\]]+)\]").unwrap();

    let final_response = if team_context.is_none() {
        // Single agent invocation
        match invoke_agent(
            agent,
            &agent_id,
            &message,
            &workspace,
            should_reset,
            &agents,
            &teams,
            skills_source,
            Some(status_tx.clone()),
        )
        .await
        {
            Ok(resp) => resp,
            Err(crate::errors::InvokeError::Timeout(secs, _)) => {
                error!("Agent timeout ({agent_id}): {secs}s");
                "Sorry, the request timed out. Please try again.".into()
            }
            Err(e) => {
                error!("Agent error ({agent_id}): {e}");
                "Sorry, I encountered an error processing your request. Please check the logs."
                    .into()
            }
        }
    } else {
        // Async team dispatch: invoke the leader, then inject teammate
        // messages back into the queue for independent processing.
        let (team_id, team) = team_context.unwrap();
        info!("Team context: {} (@{team_id})", team.name);

        let leader_response = match invoke_agent(
            agent,
            &agent_id,
            &message,
            &workspace,
            should_reset,
            &agents,
            &teams,
            skills_source,
            Some(status_tx.clone()),
        )
        .await
        {
            Ok(resp) => resp,
            Err(crate::errors::InvokeError::Timeout(secs, _)) => {
                error!("Leader timeout ({agent_id}): {secs}s");
                "Sorry, the request timed out. Please try again.".into()
            }
            Err(e) => {
                error!("Leader error ({agent_id}): {e}");
                "Sorry, I encountered an error processing your request.".into()
            }
        };

        // Collect files from leader response
        for caps in file_ref_re.captures_iter(&leader_response) {
            let file_path = caps[1].trim();
            if Path::new(file_path).exists() {
                all_files.push(file_path.to_string());
            }
        }

        // Extract teammate mentions and dispatch them async via the incoming queue
        let mentions = extract_teammate_mentions(
            &leader_response,
            &agent_id,
            &team_id,
            &teams,
            &agents,
        );

        for mention in &mentions {
            let teammate_msg = IncomingMessage {
                channel: msg.channel.clone(),
                sender: msg.sender.clone(),
                sender_id: msg.sender_id.clone(),
                message: format!(
                    "[Message from @{agent_id}]:\n{}",
                    mention.message
                ),
                timestamp: now_millis(),
                message_id: format!("team_{}_{:08x}", now_millis(), rand::random::<u32>()),
                agent: Some(mention.teammate_id.clone()),
                files: vec![],
                chat_id: msg.chat_id,
                reply_to_message_id: None,
            };

            info!("Async dispatch: @{agent_id} → @{}", mention.teammate_id);
            if incoming_tx.send(teammate_msg).await.is_err() {
                error!("Failed to inject teammate message for @{}", mention.teammate_id);
            }
        }

        if !mentions.is_empty() {
            info!(
                "Dispatched {} teammate(s) async from @{agent_id}",
                mentions.len()
            );
        }

        // Strip [@teammate: ...] tags from the leader's response
        let mention_re = Regex::new(r"\[@\w+:\s*[^\]]*\]").unwrap();
        let cleaned = mention_re.replace_all(&leader_response, "").trim().to_string();

        // Save history
        save_chain_history(
            tinyclaw_home,
            &team_id,
            &team,
            &[ChainStep {
                agent_id: agent_id.clone(),
                response: leader_response,
            }],
            raw_message,
            msg,
        );

        cleaned
    };

    // Extract file references from final response
    let mut final_text = final_response.trim().to_string();
    for caps in file_ref_re.captures_iter(&final_text.clone()) {
        let file_path = caps[1].trim();
        if Path::new(file_path).exists() && !all_files.contains(&file_path.to_string()) {
            all_files.push(file_path.to_string());
        }
    }

    // Remove [send_file: ...] tags
    if !all_files.is_empty() {
        final_text = file_ref_re.replace_all(&final_text, "").trim().to_string();
    }

    // Extract [miniapp: name: button_text] tag
    let miniapp_re = Regex::new(r"\[miniapp:\s*([^:\]]+):\s*([^\]]+)\]").unwrap();
    let miniapp = miniapp_re.captures(&final_text)
        .map(|c| (c[1].trim().to_string(), c[2].trim().to_string()));
    final_text = miniapp_re.replace_all(&final_text, "").trim().to_string();

    // Truncate if too long
    if final_text.len() > 4000 {
        final_text.truncate(3900);
        final_text.push_str("\n\n[Response truncated...]");
    }

    Ok((final_text, agent_id, all_files, miniapp))
}

fn save_chain_history(
    tinyclaw_home: &Path,
    team_id: &str,
    team: &TeamConfig,
    chain_steps: &[ChainStep],
    raw_message: &str,
    msg: &IncomingMessage,
) {
    let chats_dir = config::chats_dir(tinyclaw_home).join(team_id);
    if std::fs::create_dir_all(&chats_dir).is_err() {
        return;
    }

    let now = chrono::Utc::now();
    let mut lines = Vec::new();
    lines.push(format!(
        "# Team Chain: {} (@{team_id})",
        team.name
    ));
    lines.push(format!("**Date:** {}", now.to_rfc3339()));
    lines.push(format!(
        "**Channel:** {} | **Sender:** {}",
        msg.channel, msg.sender
    ));
    lines.push(format!("**Steps:** {}", chain_steps.len()));
    lines.push(String::new());
    lines.push("---".into());
    lines.push(String::new());
    lines.push("## User Message".into());
    lines.push(String::new());
    lines.push(raw_message.to_string());
    lines.push(String::new());

    for (i, step) in chain_steps.iter().enumerate() {
        lines.push("---".into());
        lines.push(String::new());
        lines.push(format!("## Step {}: @{}", i + 1, step.agent_id));
        lines.push(String::new());
        lines.push(step.response.clone());
        lines.push(String::new());
    }

    let filename = now
        .format("%Y-%m-%dT%H-%M-%S")
        .to_string()
        + ".md";
    let _ = std::fs::write(chats_dir.join(filename), lines.join("\n"));
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
