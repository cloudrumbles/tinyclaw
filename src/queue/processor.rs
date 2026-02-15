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
pub async fn run_queue_processor(
    tinyclaw_home: PathBuf,
    skills_source: Option<PathBuf>,
    mut incoming_rx: mpsc::Receiver<IncomingMessage>,
    outgoing_tx: mpsc::Sender<OutgoingMessage>,
) {
    info!("Queue processor started");

    // Per-agent semaphores: ensures messages to the same agent are processed sequentially
    let agent_locks: Arc<DashMap<String, Arc<Semaphore>>> = Arc::new(DashMap::new());

    while let Some(msg) = incoming_rx.recv().await {
        let home = tinyclaw_home.clone();
        let tx = outgoing_tx.clone();
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
            let result = process_message(&home, skills.as_deref(), &msg).await;
            info!("[timing] process_message: {:?}", t2.elapsed());

            // Send typing stop
            let _ = tx
                .send(OutgoingMessage::TypingStop {
                    message_id: msg.message_id.clone(),
                })
                .await;

            match result {
                Ok((response, agent_id, files)) => {
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

/// Process a single message through agent invocation (and team chain if applicable).
/// Returns (response_text, agent_id, files).
async fn process_message(
    tinyclaw_home: &Path,
    skills_source: Option<&Path>,
    msg: &IncomingMessage,
) -> anyhow::Result<(String, String, Vec<String>)> {
    let settings = config::get_settings(tinyclaw_home);
    let workspace = config::workspace_path(&settings);
    let agents = config::get_agents(&settings);
    let teams = config::get_teams(&settings);

    let raw_message = &msg.message;

    // Route message
    let (mut agent_id, message, is_team_routed) = if let Some(ref pre) = msg.agent {
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

    // Easter egg: multiple agents across teams
    if agent_id == "error" {
        return Ok((message, "error".into(), vec![]));
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
        // Team chain execution
        let (team_id, team) = team_context.unwrap();
        info!("Team context: {} (@{team_id})", team.name);

        let mut chain_steps: Vec<ChainStep> = Vec::new();
        let mut current_agent_id = agent_id.clone();
        let mut current_message = message.clone();

        loop {
            let Some(current_agent) = agents.get(&current_agent_id) else {
                error!("Agent {current_agent_id} not found during chain execution");
                break;
            };

            info!(
                "Chain step {}: invoking @{current_agent_id}",
                chain_steps.len() + 1
            );

            // Determine reset for this step
            let step_reset_flag = agent_reset_flag(&current_agent_id, &workspace);
            let step_should_reset = if chain_steps.is_empty() {
                should_reset
            } else {
                step_reset_flag.exists()
            };
            if step_should_reset && step_reset_flag.exists() {
                std::fs::remove_file(&step_reset_flag).ok();
            }

            let step_response = match invoke_agent(
                current_agent,
                &current_agent_id,
                &current_message,
                &workspace,
                step_should_reset,
                &agents,
                &teams,
                skills_source,
            )
            .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    error!("Chain error (agent: {current_agent_id}): {e}");
                    "Sorry, I encountered an error processing this request.".into()
                }
            };

            chain_steps.push(ChainStep {
                agent_id: current_agent_id.clone(),
                response: step_response.clone(),
            });

            // Collect files
            for caps in file_ref_re.captures_iter(&step_response) {
                let file_path = caps[1].trim();
                if Path::new(file_path).exists() {
                    all_files.push(file_path.to_string());
                }
            }

            // Check for teammate mentions
            let mentions = extract_teammate_mentions(
                &step_response,
                &current_agent_id,
                &team_id,
                &teams,
                &agents,
            );

            if mentions.is_empty() {
                info!(
                    "Chain ended after {} step(s) — no teammate mentioned",
                    chain_steps.len()
                );
                break;
            }

            if mentions.len() == 1 {
                // Sequential handoff
                let mention = &mentions[0];
                info!(
                    "@{current_agent_id} mentioned @{} — continuing chain",
                    mention.teammate_id
                );
                let from_agent = current_agent_id.clone();
                current_agent_id = mention.teammate_id.clone();
                current_message = format!(
                    "[Message from teammate @{from_agent}]:\n{}",
                    mention.message
                );
            } else {
                // Fan-out: invoke multiple teammates in parallel
                info!(
                    "@{current_agent_id} mentioned {} teammates — fan-out",
                    mentions.len()
                );

                let mut handles = Vec::new();
                for mention in &mentions {
                    let m_agent = agents.get(&mention.teammate_id).cloned();
                    let m_id = mention.teammate_id.clone();
                    let m_message = format!(
                        "[Message from teammate @{current_agent_id}]:\n{}",
                        mention.message
                    );
                    let ws = workspace.clone();
                    let ag = agents.clone();
                    let tm = teams.clone();
                    let ss = skills_source.map(PathBuf::from);

                    handles.push(tokio::spawn(async move {
                        let Some(agent) = m_agent else {
                            return ChainStep {
                                agent_id: m_id.clone(),
                                response: format!("Error: agent {m_id} not found"),
                            };
                        };

                        let reset_flag = agent_reset_flag(&m_id, &ws);
                        let m_should_reset = reset_flag.exists();
                        if m_should_reset {
                            std::fs::remove_file(&reset_flag).ok();
                        }

                        let resp = match invoke_agent(
                            &agent,
                            &m_id,
                            &m_message,
                            &ws,
                            m_should_reset,
                            &ag,
                            &tm,
                            ss.as_deref(),
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                error!("Fan-out error (agent: {m_id}): {e}");
                                "Sorry, I encountered an error processing this request.".into()
                            }
                        };

                        ChainStep {
                            agent_id: m_id,
                            response: resp,
                        }
                    }));
                }

                let results = futures::future::join_all(handles).await;
                for result in results {
                    if let Ok(step) = result {
                        // Collect files from fan-out
                        for caps in file_ref_re.captures_iter(&step.response) {
                            let file_path = caps[1].trim();
                            if Path::new(file_path).exists() {
                                all_files.push(file_path.to_string());
                            }
                        }
                        chain_steps.push(step);
                    }
                }

                info!("Fan-out complete");
                break;
            }
        }

        // Aggregate responses
        let aggregated = if chain_steps.len() == 1 {
            chain_steps[0].response.clone()
        } else {
            chain_steps
                .iter()
                .map(|s| format!("@{}: {}", s.agent_id, s.response))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n")
        };

        // Save chain chat history
        save_chain_history(tinyclaw_home, &team_id, &team, &chain_steps, raw_message, msg);

        aggregated
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

    // Truncate if too long
    if final_text.len() > 4000 {
        final_text.truncate(3900);
        final_text.push_str("\n\n[Response truncated...]");
    }

    Ok((final_text, agent_id, all_files))
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
