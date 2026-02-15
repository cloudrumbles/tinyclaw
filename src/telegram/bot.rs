use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::Path as AxumPath;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{
    ChatAction, FileMeta, InputFile, MediaKind, MessageKind, ReplyParameters,
};
use teloxide::error_handlers::LoggingErrorHandler;
use teloxide::update_listeners::webhooks;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config;
use crate::pairing::ensure_sender_paired;
use crate::queue::{IncomingMessage, OutgoingMessage};
use crate::telegram::files::{
    build_unique_file_path, ensure_file_extension, ext_from_mime, split_message,
};

/// Pending message info for matching responses to original messages.
struct PendingMessage {
    chat_id: ChatId,
    message_id: teloxide::types::MessageId,
    typing_token: CancellationToken,
    created_at: std::time::Instant,
}

/// Run the Telegram bot task. Handles both incoming messages and outgoing responses.
pub async fn run_telegram(
    tinyclaw_home: PathBuf,
    bot_token: String,
    webhook_url: Option<String>,
    incoming_tx: mpsc::Sender<IncomingMessage>,
    mut outgoing_rx: mpsc::Receiver<OutgoingMessage>,
) {
    let bot = Bot::new(&bot_token);

    // Verify bot connection
    match bot.get_me().await {
        Ok(me) => info!("Telegram bot connected as @{}", me.username()),
        Err(e) => {
            error!("Failed to connect to Telegram: {e}");
            return;
        }
    }

    let files_dir = config::files_dir(&tinyclaw_home);
    std::fs::create_dir_all(&files_dir).ok();

    let pairing_file = config::pairing_file(&tinyclaw_home);
    let settings_file = config::settings_file(&tinyclaw_home);

    // Shared state for pending messages
    let pending: Arc<Mutex<HashMap<String, PendingMessage>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Spawn outgoing message consumer
    let bot_out = bot.clone();
    let pending_out = pending.clone();
    let outgoing_handle = tokio::spawn(async move {
        handle_outgoing(bot_out, pending_out, &mut outgoing_rx).await;
    });

    // Spawn pending message cleanup task (10-minute timeout)
    let pending_cleanup = pending.clone();
    let cleanup_handle = tokio::spawn(async move {
        let timeout = std::time::Duration::from_secs(600);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            let mut map = pending_cleanup.lock().await;
            let before = map.len();
            map.retain(|id, p| {
                let stale = p.created_at.elapsed() > timeout;
                if stale {
                    p.typing_token.cancel();
                    warn!("Cleaned up stale pending message: {id}");
                }
                !stale
            });
            let removed = before - map.len();
            if removed > 0 {
                info!("Pending message cleanup: removed {removed} stale entries");
            }
        }
    });

    // Set up incoming message handler using teloxide dispatcher
    let incoming_tx = Arc::new(incoming_tx);
    let pending_in = pending.clone();
    let files_dir = Arc::new(files_dir);
    let pairing_file = Arc::new(pairing_file);
    let settings_file = Arc::new(settings_file);
    let tinyclaw_home = Arc::new(tinyclaw_home);
    let cron_home = tinyclaw_home.clone();
    let bot_clone = bot.clone();

    let handler = Update::filter_message().endpoint(
        move |bot: Bot, msg: Message| {
            let tx = incoming_tx.clone();
            let pending = pending_in.clone();
            let files_dir = files_dir.clone();
            let pairing_file = pairing_file.clone();
            let settings_file = settings_file.clone();
            let tinyclaw_home = tinyclaw_home.clone();
            async move {
                handle_incoming_message(
                    &bot,
                    msg,
                    &tx,
                    &pending,
                    &files_dir,
                    &pairing_file,
                    &settings_file,
                    &tinyclaw_home,
                )
                .await;
                respond(())
            }
        },
    );

    let mut dispatcher = Dispatcher::builder(bot_clone, handler)
        .enable_ctrlc_handler()
        .build();

    if let Some(ref url) = webhook_url {
        let port: u16 = std::env::var("WEBHOOK_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(3000);
        let addr = ([0, 0, 0, 0], port).into();
        let url: reqwest::Url = url.parse().expect("invalid WEBHOOK_URL");
        let (listener, stop_flag, tg_router) =
            webhooks::axum_to_router(bot.clone(), webhooks::Options::new(addr, url))
                .await
                .expect("Failed to set up webhook");

        // Add /cron/{job_id} route for cron-job.org triggers
        let app = tg_router.route(
            "/cron/{job_id}",
            axum::routing::get({
                let home = cron_home;
                move |AxumPath(job_id): AxumPath<String>| {
                    let home = home.clone();
                    async move { handle_cron_trigger(&home, &job_id) }
                }
            }),
        );

        // Start HTTP server with graceful shutdown
        let tcp = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Failed to bind webhook listener");
        tokio::spawn(async move {
            axum::serve(tcp, app)
                .with_graceful_shutdown(stop_flag)
                .await
                .ok();
        });

        info!("Webhook listener started on 0.0.0.0:{}", port);
        dispatcher
            .dispatch_with_listener(
                listener,
                LoggingErrorHandler::with_custom_text("Webhook error"),
            )
            .await;
    } else {
        info!("No WEBHOOK_URL set, using long polling");
        dispatcher.dispatch().await;
    }

    outgoing_handle.abort();
    cleanup_handle.abort();
}

async fn handle_incoming_message(
    bot: &Bot,
    msg: Message,
    incoming_tx: &mpsc::Sender<IncomingMessage>,
    pending: &Arc<Mutex<HashMap<String, PendingMessage>>>,
    files_dir: &Path,
    pairing_file: &Path,
    settings_file: &Path,
    tinyclaw_home: &Path,
) {
    // Only handle private chats
    if !msg.chat.is_private() {
        return;
    }

    let chat_id = msg.chat.id;
    let msg_id = msg.id;

    // Extract sender info
    let sender = msg
        .from
        .as_ref()
        .map(|u| {
            let mut name = u.first_name.clone();
            if let Some(ref last) = u.last_name {
                name.push(' ');
                name.push_str(last);
            }
            name
        })
        .unwrap_or_else(|| "Unknown".into());
    let sender_id = chat_id.0.to_string();

    // Extract text + download files
    let (message_text, downloaded_files) =
        extract_message_content(bot, &msg, files_dir).await;

    // Skip if no text and no media
    if message_text.trim().is_empty() && downloaded_files.is_empty() {
        return;
    }

    info!(
        "Message from {sender}: {}{}",
        &message_text.chars().take(50).collect::<String>(),
        if !downloaded_files.is_empty() {
            format!(" [+{} file(s)]", downloaded_files.len())
        } else {
            String::new()
        }
    );

    // Check pairing
    let pairing = {
        let pf = pairing_file.to_path_buf();
        let ch = "telegram".to_string();
        let sid = sender_id.clone();
        let sname = sender.clone();
        tokio::task::spawn_blocking(move || ensure_sender_paired(&pf, &ch, &sid, &sname))
            .await
            .unwrap()
    };

    if !pairing.approved {
        if let Some(code) = &pairing.code {
            if pairing.is_new_pending {
                info!("Blocked unpaired sender {sender} ({sender_id}) with code {code}");
                let text = format!(
                    "This sender is not paired yet.\nYour pairing code: {code}\n\
                     Ask the TinyClaw owner to approve you with:\ntinyclaw pairing approve {code}"
                );
                let _ = bot
                    .send_message(chat_id, text)
                    .reply_parameters(ReplyParameters::new(msg_id))
                    .await;
            } else {
                info!("Blocked pending sender {sender} ({sender_id})");
            }
        }
        return;
    }

    // Handle /agent command
    if message_text.trim().eq_ignore_ascii_case("/agent")
        || message_text.trim().eq_ignore_ascii_case("!agent")
    {
        let text = get_agent_list_text(settings_file);
        let _ = bot
            .send_message(chat_id, text)
            .reply_parameters(ReplyParameters::new(msg_id))
            .await;
        return;
    }

    // Handle /team command
    if message_text.trim().eq_ignore_ascii_case("/team")
        || message_text.trim().eq_ignore_ascii_case("!team")
    {
        let text = get_team_list_text(settings_file);
        let _ = bot
            .send_message(chat_id, text)
            .reply_parameters(ReplyParameters::new(msg_id))
            .await;
        return;
    }

    // Handle /reset command
    if message_text.trim().eq_ignore_ascii_case("/reset")
        || message_text.trim().eq_ignore_ascii_case("/new")
        || message_text.trim().eq_ignore_ascii_case("!reset")
    {
        let reset_path = config::reset_flag(tinyclaw_home);
        std::fs::write(&reset_path, "reset").ok();
        let _ = bot
            .send_message(
                chat_id,
                "Conversation reset! Next message will start a fresh conversation.",
            )
            .reply_parameters(ReplyParameters::new(msg_id))
            .await;
        return;
    }

    // Send typing indicator
    let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;

    // Build full message with file references
    let mut full_message = message_text.clone();
    if !downloaded_files.is_empty() {
        let file_refs: Vec<String> = downloaded_files
            .iter()
            .map(|f| format!("[file: {f}]"))
            .collect();
        let refs_str = file_refs.join("\n");
        if full_message.is_empty() {
            full_message = refs_str;
        } else {
            full_message = format!("{full_message}\n\n{refs_str}");
        }
    }

    let queue_message_id = format!("{}_{:08x}", now_millis(), rand::random::<u32>());

    // Create typing cancellation token
    let typing_token = CancellationToken::new();
    let typing_bot = bot.clone();
    let typing_chat_id = chat_id;
    let typing_token_clone = typing_token.clone();

    // Spawn typing indicator refresh task
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = typing_token_clone.cancelled() => break,
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(4)) => {
                    let _ = typing_bot.send_chat_action(typing_chat_id, ChatAction::Typing).await;
                }
            }
        }
    });

    // Store pending message
    {
        let mut map = pending.lock().await;
        map.insert(
            queue_message_id.clone(),
            PendingMessage {
                chat_id,
                message_id: msg_id,
                typing_token: typing_token.clone(),
                created_at: std::time::Instant::now(),
            },
        );
    }

    // Send to queue processor
    let incoming = IncomingMessage {
        channel: "telegram".into(),
        sender,
        sender_id,
        message: full_message,
        timestamp: now_millis(),
        message_id: queue_message_id,
        agent: None,
        files: downloaded_files,
        chat_id: Some(chat_id.0),
        reply_to_message_id: Some(msg_id.0),
    };

    if incoming_tx.send(incoming).await.is_err() {
        error!("Failed to send message to queue processor");
        typing_token.cancel();
    }
}

/// Extract text and download any attached files from a Telegram message.
async fn extract_message_content(
    bot: &Bot,
    msg: &Message,
    files_dir: &Path,
) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut files = Vec::new();

    let msg_id_str = msg.id.0.to_string();

    // Extract text from the message
    if let Some(t) = msg.text() {
        text = t.to_string();
    } else if let Some(c) = msg.caption() {
        text = c.to_string();
    }

    // Handle different media types
    if let MessageKind::Common(ref common) = msg.kind {
        match &common.media_kind {
            MediaKind::Photo(photo) => {
                if let Some(largest) = photo.photo.last() {
                    if let Some(path) = download_telegram_file(
                        bot,
                        &largest.file,
                        ".jpg",
                        &msg_id_str,
                        Some(&format!("photo_{}.jpg", msg.id.0)),
                        files_dir,
                    )
                    .await
                    {
                        files.push(path);
                    }
                }
            }
            MediaKind::Document(doc) => {
                let ext = doc
                    .document
                    .file_name
                    .as_deref()
                    .and_then(|n| Path::new(n).extension())
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{e}"))
                    .or_else(|| {
                        doc.document
                            .mime_type
                            .as_ref()
                            .map(|m| ext_from_mime(m.as_ref()).to_string())
                    })
                    .unwrap_or_default();
                if let Some(path) = download_telegram_file(
                    bot,
                    &doc.document.file,
                    &ext,
                    &msg_id_str,
                    doc.document.file_name.as_deref(),
                    files_dir,
                )
                .await
                {
                    files.push(path);
                }
            }
            MediaKind::Audio(audio) => {
                let ext = audio
                    .audio
                    .mime_type
                    .as_ref()
                    .map(|m| ext_from_mime(m.as_ref()).to_string())
                    .unwrap_or_else(|| ".mp3".into());
                let name = audio.audio.file_name.as_deref();
                if let Some(path) = download_telegram_file(
                    bot,
                    &audio.audio.file,
                    &ext,
                    &msg_id_str,
                    name,
                    files_dir,
                )
                .await
                {
                    files.push(path);
                }
            }
            MediaKind::Voice(voice) => {
                if let Some(path) = download_telegram_file(
                    bot,
                    &voice.voice.file,
                    ".ogg",
                    &msg_id_str,
                    Some(&format!("voice_{}.ogg", msg.id.0)),
                    files_dir,
                )
                .await
                {
                    files.push(path);
                }
            }
            MediaKind::Video(video) => {
                let ext = video
                    .video
                    .mime_type
                    .as_ref()
                    .map(|m| ext_from_mime(m.as_ref()).to_string())
                    .unwrap_or_else(|| ".mp4".into());
                let name = video.video.file_name.as_deref();
                if let Some(path) = download_telegram_file(
                    bot,
                    &video.video.file,
                    &ext,
                    &msg_id_str,
                    name,
                    files_dir,
                )
                .await
                {
                    files.push(path);
                }
            }
            MediaKind::VideoNote(vn) => {
                if let Some(path) = download_telegram_file(
                    bot,
                    &vn.video_note.file,
                    ".mp4",
                    &msg_id_str,
                    Some(&format!("video_note_{}.mp4", msg.id.0)),
                    files_dir,
                )
                .await
                {
                    files.push(path);
                }
            }
            MediaKind::Sticker(sticker) => {
                let ext = if sticker.sticker.is_animated() {
                    ".tgs"
                } else if sticker.sticker.is_video() {
                    ".webm"
                } else {
                    ".webp"
                };
                if let Some(path) = download_telegram_file(
                    bot,
                    &sticker.sticker.file,
                    ext,
                    &msg_id_str,
                    Some(&format!("sticker_{}{}", msg.id.0, ext)),
                    files_dir,
                )
                .await
                {
                    files.push(path);
                }
                if text.is_empty() {
                    text = format!(
                        "[Sticker: {}]",
                        sticker.sticker.emoji.as_deref().unwrap_or("sticker")
                    );
                }
            }
            _ => {}
        }
    }

    (text, files)
}

/// Download a Telegram file to the local files directory.
async fn download_telegram_file(
    bot: &Bot,
    file_meta: &FileMeta,
    ext: &str,
    msg_id: &str,
    original_name: Option<&str>,
    files_dir: &Path,
) -> Option<String> {
    let file = bot.get_file(file_meta.id.clone()).await.ok()?;
    let default_name = format!("file_{}{}", now_millis(), ext);
    let source_name = original_name.unwrap_or(&default_name);
    let with_ext = ensure_file_extension(source_name, if ext.is_empty() { ".bin" } else { ext });
    let filename = format!("telegram_{msg_id}_{with_ext}");
    let local_path = build_unique_file_path(files_dir, &filename);

    // Download using teloxide
    let mut dst = tokio::fs::File::create(&local_path).await.ok()?;
    bot.download_file(&file.path, &mut dst).await.ok()?;

    let path_str = local_path.to_string_lossy().to_string();
    info!(
        "Downloaded file: {}",
        local_path.file_name()?.to_string_lossy()
    );
    Some(path_str)
}

/// Handle outgoing messages (responses from queue processor → Telegram).
async fn handle_outgoing(
    bot: Bot,
    pending: Arc<Mutex<HashMap<String, PendingMessage>>>,
    outgoing_rx: &mut mpsc::Receiver<OutgoingMessage>,
) {
    while let Some(msg) = outgoing_rx.recv().await {
        match msg {
            OutgoingMessage::TypingStart {
                message_id: _,
                chat_id,
            } => {
                let _ = bot
                    .send_chat_action(ChatId(chat_id), ChatAction::Typing)
                    .await;
            }
            OutgoingMessage::TypingStop { ref message_id } => {
                let map = pending.lock().await;
                if let Some(p) = map.get(message_id) {
                    p.typing_token.cancel();
                }
            }
            OutgoingMessage::Response {
                ref channel,
                ref sender,
                ref message,
                original_message: _,
                timestamp: _,
                ref message_id,
                ref agent,
                ref files,
                chat_id,
                reply_to_message_id,
            } => {
                // Heartbeat responses don't go to telegram
                if channel == "heartbeat" {
                    info!(
                        "Heartbeat response from @{}: {} chars",
                        agent.as_deref().unwrap_or("?"),
                        message.len()
                    );
                    continue;
                }

                let mut map = pending.lock().await;
                let entry = map.remove(message_id);
                drop(map);

                let (target_chat_id, target_reply_id) = if let Some(ref p) = entry {
                    p.typing_token.cancel();
                    (p.chat_id, Some(p.message_id))
                } else if let Some(cid) = chat_id {
                    (
                        ChatId(cid),
                        reply_to_message_id.map(teloxide::types::MessageId),
                    )
                } else {
                    warn!("No pending message for {message_id}, skipping");
                    continue;
                };

                // Send files first
                for file_path in files {
                    if let Err(e) = send_file(&bot, target_chat_id, file_path).await {
                        error!("Failed to send file {file_path}: {e}");
                    }
                }

                // Send text response
                if !message.is_empty() {
                    let chunks = split_message(message, 4096);
                    for (i, chunk) in chunks.iter().enumerate() {
                        let mut req = bot.send_message(target_chat_id, chunk);
                        if i == 0 {
                            if let Some(reply_id) = target_reply_id {
                                req = req.reply_parameters(ReplyParameters::new(reply_id));
                            }
                        }
                        if let Err(e) = req.await {
                            error!("Failed to send message chunk {i}: {e}");
                        }
                    }
                }

                info!(
                    "Sent response to {sender} ({} chars{})",
                    message.len(),
                    if !files.is_empty() {
                        format!(", {} file(s)", files.len())
                    } else {
                        String::new()
                    }
                );
            }
        }
    }
}

/// Send a file to a Telegram chat, choosing the appropriate method by extension.
async fn send_file(
    bot: &Bot,
    chat_id: ChatId,
    file_path: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Ok(());
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let input_file = InputFile::file(path);

    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" => {
            bot.send_photo(chat_id, input_file).await?;
        }
        "mp3" | "ogg" | "wav" | "m4a" => {
            bot.send_audio(chat_id, input_file).await?;
        }
        "mp4" | "avi" | "mov" | "webm" => {
            bot.send_video(chat_id, input_file).await?;
        }
        _ => {
            bot.send_document(chat_id, input_file).await?;
        }
    }

    info!(
        "Sent file to Telegram: {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    Ok(())
}

/// Get formatted agent list for /agent command.
fn get_agent_list_text(settings_file: &Path) -> String {
    let Ok(data) = std::fs::read_to_string(settings_file) else {
        return "Could not load agent configuration.".into();
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&data) else {
        return "Could not load agent configuration.".into();
    };

    let agents = settings.get("agents").and_then(|a| a.as_object());
    let Some(agents) = agents else {
        return "No agents configured. Using default single-agent mode.\n\n\
                Configure agents in .tinyclaw/settings.json or run: tinyclaw agent add"
            .into();
    };

    if agents.is_empty() {
        return "No agents configured. Using default single-agent mode.\n\n\
                Configure agents in .tinyclaw/settings.json or run: tinyclaw agent add"
            .into();
    }

    let mut text = String::from("Available Agents:\n");
    for (id, agent) in agents {
        let name = agent.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let provider = agent
            .get("provider")
            .and_then(|p| p.as_str())
            .unwrap_or("?");
        let model = agent.get("model").and_then(|m| m.as_str()).unwrap_or("?");
        let wd = agent
            .get("working_directory")
            .and_then(|w| w.as_str())
            .unwrap_or("?");
        text.push_str(&format!("\n@{id} - {name}"));
        text.push_str(&format!("\n  Provider: {provider}/{model}"));
        text.push_str(&format!("\n  Directory: {wd}"));
    }
    text.push_str("\n\nUsage: Start your message with @agent_id to route to a specific agent.");
    text
}

/// Get formatted team list for /team command.
fn get_team_list_text(settings_file: &Path) -> String {
    let Ok(data) = std::fs::read_to_string(settings_file) else {
        return "Could not load team configuration.".into();
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&data) else {
        return "Could not load team configuration.".into();
    };

    let teams = settings.get("teams").and_then(|t| t.as_object());
    let Some(teams) = teams else {
        return "No teams configured.\n\nCreate a team with: tinyclaw team add".into();
    };

    if teams.is_empty() {
        return "No teams configured.\n\nCreate a team with: tinyclaw team add".into();
    }

    let mut text = String::from("Available Teams:\n");
    for (id, team) in teams {
        let name = team.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let agents = team
            .get("agents")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let leader = team
            .get("leader_agent")
            .and_then(|l| l.as_str())
            .unwrap_or("?");
        text.push_str(&format!("\n@{id} - {name}"));
        text.push_str(&format!("\n  Agents: {agents}"));
        text.push_str(&format!("\n  Leader: @{leader}"));
    }
    text.push_str("\n\nUsage: Start your message with @team_id to route to a team.");
    text
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Handle GET /cron/:job_id — triggered by cron-job.org.
/// Reads job config from cron-jobs.json, writes a trigger file to cron-inbox/.
fn handle_cron_trigger(
    tinyclaw_home: &Path,
    job_id: &str,
) -> (axum::http::StatusCode, &'static str) {
    let jobs_file = tinyclaw_home.join("cron-jobs.json");

    let jobs: serde_json::Value = match std::fs::read_to_string(&jobs_file) {
        Ok(data) => match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "Bad jobs file"),
        },
        Err(_) => return (axum::http::StatusCode::NOT_FOUND, "No jobs configured"),
    };

    let job = match jobs.get("jobs").and_then(|j| j.get(job_id)) {
        Some(j) => j,
        None => return (axum::http::StatusCode::NOT_FOUND, "Job not found"),
    };

    let name = job.get("name").and_then(|n| n.as_str()).unwrap_or("Cron Job");
    let agent_id = job.get("agent_id").and_then(|a| a.as_str()).unwrap_or("sultana");
    let prompt = job.get("prompt").and_then(|p| p.as_str()).unwrap_or("");

    // chat_id may be stored as string or number
    let chat_id: Option<i64> = job
        .get("chat_id")
        .and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.parse().ok())));

    let mut trigger = serde_json::json!({
        "name": name,
        "agent_id": agent_id,
        "job_id": job_id,
        "prompt": prompt,
    });
    if let Some(cid) = chat_id {
        trigger["chat_id"] = serde_json::json!(cid);
    }

    // Write trigger file atomically to cron-inbox
    let inbox_dir = tinyclaw_home.join("cron-inbox");
    let trigger_file = inbox_dir.join(format!("{job_id}.json"));
    let tmp = inbox_dir.join(format!("{job_id}.json.tmp"));

    let Ok(data) = serde_json::to_string_pretty(&trigger) else {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "Serialize failed");
    };

    if std::fs::write(&tmp, &data).is_err() {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "Write failed");
    }
    std::fs::rename(&tmp, &trigger_file).ok();

    info!("Cron trigger: {job_id} ({name}) -> @{agent_id}");
    (axum::http::StatusCode::OK, "OK")
}
