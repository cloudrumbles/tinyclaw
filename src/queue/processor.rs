use std::path::Path;
use std::sync::Arc;

use regex::Regex;
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{info, error};

use crate::config;
use crate::invoke::invoke_bot;
use crate::queue::channels::{IncomingMessage, OutgoingMessage};

/// Shared handle to the current task's cancellation token.
/// Bot.rs cancels it on /stop; processor.rs sets a new one per message.
pub type CancelHandle = Arc<Mutex<CancellationToken>>;

/// Run the queue processor task. Reads from `incoming_rx`, writes to `outgoing_tx`.
pub async fn run_queue_processor(
    tinyclaw_home: std::path::PathBuf,
    skills_source: Option<std::path::PathBuf>,
    mut incoming_rx: mpsc::Receiver<IncomingMessage>,
    outgoing_tx: mpsc::Sender<OutgoingMessage>,
    cancel_handle: CancelHandle,
) {
    info!("Queue processor started");

    // Single semaphore: messages are processed sequentially
    let lock = Arc::new(Semaphore::new(1));

    while let Some(msg) = incoming_rx.recv().await {
        let home = tinyclaw_home.clone();
        let tx = outgoing_tx.clone();
        let skills = skills_source.clone();
        let sem = lock.clone();
        let cancel = cancel_handle.clone();

        tokio::spawn(async move {
            let t0 = std::time::Instant::now();

            // Acquire semaphore (sequential processing)
            let _permit = sem.acquire().await.unwrap();
            info!("[timing] semaphore wait: {:?}", t0.elapsed());

            // Create a fresh cancel token for this message and store it
            let token = CancellationToken::new();
            *cancel.lock().await = token.clone();

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
            let result = process_message(&home, skills.as_deref(), &msg, &tx, token).await;
            info!("[timing] process_message: {:?}", t2.elapsed());

            // Send typing stop
            let _ = tx
                .send(OutgoingMessage::TypingStop {
                    message_id: msg.message_id.clone(),
                })
                .await;

            match result {
                Ok((response, files, miniapp, menubutton)) => {
                    let _ = tx
                        .send(OutgoingMessage::Response {
                            sender: msg.sender.clone(),
                            message: response,
                            original_message: msg.message.clone(),
                            timestamp: now_millis(),
                            message_id: msg.message_id.clone(),
                            files,
                            chat_id: msg.chat_id,
                            reply_to_message_id: msg.reply_to_message_id,
                            miniapp,
                            menubutton,
                        })
                        .await;
                }
                Err(e) => {
                    error!("Processing error: {e}");
                    let _ = tx
                        .send(OutgoingMessage::Response {
                            sender: msg.sender.clone(),
                            message: "Sorry, I encountered an error processing your request."
                                .into(),
                            original_message: msg.message.clone(),
                            timestamp: now_millis(),
                            message_id: msg.message_id.clone(),
                            files: vec![],
                            chat_id: msg.chat_id,
                            reply_to_message_id: msg.reply_to_message_id,
                            miniapp: None,
                            menubutton: None,
                        })
                        .await;
                }
            }
        });
    }

    info!("Queue processor shutting down");
}

/// Process a single message through bot invocation.
/// Returns (response_text, files, miniapp).
async fn process_message(
    tinyclaw_home: &Path,
    skills_source: Option<&Path>,
    msg: &IncomingMessage,
    outgoing_tx: &mpsc::Sender<OutgoingMessage>,
    cancel: CancellationToken,
) -> anyhow::Result<(String, Vec<String>, Option<(String, String)>, Option<(String, String)>)> {
    let settings = config::get_settings(tinyclaw_home);
    let bot_config = config::get_bot_config(&settings);

    // Set up status update channel for streaming tool status to Telegram.
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

    // Prepend timestamp in SGT (UTC+8) so the bot knows when the message was sent
    let sgt = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let ts = chrono::Utc::now().with_timezone(&sgt);
    let message = format!("[{}]\n{}", ts.format("%-d %b %Y, %-I:%M %p SGT"), msg.message);

    info!(
        "Invoking bot: {} ({}) [{}/{}]",
        bot_config.name, bot_config.bot_id, bot_config.provider, bot_config.model
    );

    // Check reset flag
    let workspace_dir = config::bot_workspace(&bot_config.bot_id);
    let reset_path = config::reset_flag(&workspace_dir);
    let should_reset = reset_path.exists();
    if should_reset {
        std::fs::remove_file(&reset_path).ok();
    }

    let file_ref_re = Regex::new(r"\[send_file:\s*([^\]]+)\]").unwrap();

    let response = match invoke_bot(
        &bot_config,
        &message,
        tinyclaw_home,
        should_reset,
        skills_source,
        Some(status_tx.clone()),
        cancel,
    )
    .await
    {
        Ok(resp) => resp,
        Err(crate::errors::InvokeError::Cancelled) => {
            info!("Task cancelled by user");
            "Cancelled.".into()
        }
        Err(crate::errors::InvokeError::Timeout(secs, _)) => {
            error!("Bot timeout: {secs}s");
            "Sorry, the request timed out. Please try again.".into()
        }
        Err(e) => {
            error!("Bot error: {e}");
            "Sorry, I encountered an error processing your request. Please check the logs."
                .into()
        }
    };

    // Extract file references
    let mut final_text = response.trim().to_string();
    let mut all_files: Vec<String> = Vec::new();
    for caps in file_ref_re.captures_iter(&final_text.clone()) {
        let file_path = caps[1].trim();
        if Path::new(file_path).exists() {
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

    // Extract [menubutton: name: button_text] tag — pins miniapp as chat menu button
    let menubutton_re = Regex::new(r"\[menubutton:\s*([^:\]]+):\s*([^\]]+)\]").unwrap();
    let menubutton = menubutton_re.captures(&final_text)
        .map(|c| (c[1].trim().to_string(), c[2].trim().to_string()));
    final_text = menubutton_re.replace_all(&final_text, "").trim().to_string();

    // Truncate if too long
    if final_text.len() > 4000 {
        final_text.truncate(3900);
        final_text.push_str("\n\n[Response truncated...]");
    }

    Ok((final_text, all_files, miniapp, menubutton))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
