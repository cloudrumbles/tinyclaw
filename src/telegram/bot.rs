use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::Path as AxumPath;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{
    ChatAction, FileMeta, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, MediaKind,
    MenuButton, MessageKind, ReplyParameters, WebAppInfo,
};
use teloxide::error_handlers::LoggingErrorHandler;
use teloxide::update_listeners::webhooks;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config;
use crate::queue::{CancelHandle, IncomingMessage, OutgoingMessage};
use crate::telegram::files::{
    build_unique_file_path, ensure_file_extension, ext_from_mime, split_message,
};
use crate::telegram::markdown::markdown_to_telegram_html;

/// Pending message info for matching responses to original messages.
struct PendingMessage {
    chat_id: ChatId,
    message_id: teloxide::types::MessageId,
    typing_token: CancellationToken,
    created_at: std::time::Instant,
    status_message_id: Option<teloxide::types::MessageId>,
}

/// Run the Telegram bot task. Handles both incoming messages and outgoing responses.
pub async fn run_telegram(
    tinyclaw_home: PathBuf,
    bot_token: String,
    webhook_url: Option<String>,
    incoming_tx: mpsc::Sender<IncomingMessage>,
    mut outgoing_rx: mpsc::Receiver<OutgoingMessage>,
    cancel_handle: CancelHandle,
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

    // Resolve the bot's workspace for files, miniapps, cron
    let settings = config::get_settings(&tinyclaw_home);
    let bot_config = config::get_bot_config(&settings);
    let default_workspace = config::bot_workspace(&bot_config.bot_id);

    let files_dir = config::files_dir(&default_workspace);
    std::fs::create_dir_all(&files_dir).ok();

    // Shared state for pending messages
    let pending: Arc<Mutex<HashMap<String, PendingMessage>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Compute base URL for mini apps (strip /webhook suffix)
    let base_url = webhook_url
        .as_deref()
        .map(|u| u.trim_end_matches("/webhook").to_string())
        .unwrap_or_default();

    // Spawn outgoing message consumer
    let bot_out = bot.clone();
    let pending_out = pending.clone();
    let base_url_out = base_url.clone();
    let outgoing_handle = tokio::spawn(async move {
        handle_outgoing(bot_out, pending_out, &mut outgoing_rx, &base_url_out).await;
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
    let default_workspace = Arc::new(default_workspace);
    let cancel_handle = Arc::new(cancel_handle);
    let cron_home = default_workspace.clone();
    let miniapps_home = default_workspace.clone();
    let cron_tx = incoming_tx.clone();
    let bot_clone = bot.clone();

    let handler = Update::filter_message().endpoint(
        move |bot: Bot, msg: Message| {
            let tx = incoming_tx.clone();
            let pending = pending_in.clone();
            let files_dir = files_dir.clone();
            let default_workspace = default_workspace.clone();
            let cancel = cancel_handle.clone();
            async move {
                handle_incoming_message(
                    &bot,
                    msg,
                    &tx,
                    &pending,
                    &files_dir,
                    &default_workspace,
                    &cancel,
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

        // Serve mini apps from the default agent's workspace
        let miniapps_dir = miniapps_home.join("miniapps");
        std::fs::create_dir_all(&miniapps_dir).ok();

        // Add /cron/{job_id} route for cron-job.org triggers
        // Add /apps/ route for serving mini app static files
        let app = tg_router
            .route(
                "/cron/{job_id}",
                axum::routing::get({
                    let home = cron_home;
                    move |AxumPath(job_id): AxumPath<String>| {
                        let home = home.clone();
                        let tx = cron_tx.clone();
                        async move { handle_cron_trigger(&home, &job_id, &*tx).await }
                    }
                }),
            )
            .nest_service("/apps", tower_http::services::ServeDir::new(&miniapps_dir));

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
    default_workspace: &Path,
    cancel_handle: &Arc<CancelHandle>,
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

    // Handle /stop command — cancel current task
    if message_text.trim().eq_ignore_ascii_case("/stop")
        || message_text.trim().eq_ignore_ascii_case("!stop")
    {
        info!("Stop requested by {sender}");
        let token = cancel_handle.lock().await;
        token.cancel();
        let _ = bot
            .send_message(chat_id, "Stopping...")
            .reply_parameters(ReplyParameters::new(msg_id))
            .await;
        return;
    }

    // Handle /reset command
    if message_text.trim().eq_ignore_ascii_case("/reset")
        || message_text.trim().eq_ignore_ascii_case("!reset")
    {
        let reset_path = config::reset_flag(default_workspace);
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

    // Prepend timestamp so the agent knows when the message was sent
    let now_sgt = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
    let timestamp_prefix = format!("[{}]", now_sgt.format("%Y-%m-%d %H:%M:%S SGT"));

    // Build full message with file references
    let mut full_message = format!("{timestamp_prefix} {message_text}");
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
                status_message_id: None,
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
    base_url: &str,
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
            OutgoingMessage::StatusUpdate {
                ref message_id,
                chat_id,
                ref status,
            } => {
                let mut map = pending.lock().await;
                if let Some(p) = map.get_mut(message_id) {
                    let formatted = format!("<i>{status}</i>");
                    if let Some(status_msg_id) = p.status_message_id {
                        let _ = bot
                            .edit_message_text(ChatId(chat_id), status_msg_id, &formatted)
                            .parse_mode(teloxide::types::ParseMode::Html)
                            .await;
                    } else {
                        match bot
                            .send_message(ChatId(chat_id), &formatted)
                            .parse_mode(teloxide::types::ParseMode::Html)
                            .await
                        {
                            Ok(sent) => {
                                p.status_message_id = Some(sent.id);
                            }
                            Err(e) => {
                                error!("Failed to send status message: {e}");
                            }
                        }
                    }
                }
            }
            OutgoingMessage::Response {
                ref sender,
                ref message,
                ref message_id,
                ref files,
                chat_id,
                reply_to_message_id,
                ref miniapp,
                ref menubutton,
                ..
            } => {


                let mut map = pending.lock().await;
                let entry = map.remove(message_id);
                drop(map);

                let (target_chat_id, target_reply_id, status_msg_id) = if let Some(ref p) = entry {
                    p.typing_token.cancel();
                    (p.chat_id, Some(p.message_id), p.status_message_id)
                } else if let Some(cid) = chat_id {
                    (
                        ChatId(cid),
                        reply_to_message_id.map(teloxide::types::MessageId),
                        None,
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

                // Send text response — edit the status message for the first
                // chunk if one exists, send the rest as new messages.
                // Split mixed content into plain text and <html>...</html> segments.
                // Plain text segments get markdown→HTML conversion so everything
                // is sent with ParseMode::Html.
                if !message.is_empty() {
                    let segments: Vec<(String, bool)> = split_html_segments(message)
                        .into_iter()
                        .map(|(text, is_html)| {
                            if is_html {
                                (text, true)
                            } else {
                                (markdown_to_telegram_html(&text), true)
                            }
                        })
                        .collect();

                    // Build InlineKeyboardButton for mini app if present
                    let miniapp_keyboard = miniapp.as_ref().and_then(|(app_name, button_text)| {
                        if base_url.is_empty() { return None; }
                        let app_url = format!("{base_url}/apps/{app_name}/index.html");
                        let url: reqwest::Url = app_url.parse().ok()?;
                        Some(InlineKeyboardMarkup::new(vec![vec![
                            InlineKeyboardButton::web_app(
                                button_text,
                                WebAppInfo { url },
                            ),
                        ]]))
                    });

                    let mut first_handled = false;
                    let total_segments = segments.len();

                    for (seg_idx, (seg_text, use_html)) in segments.iter().enumerate() {
                        let chunks = split_message(seg_text, 4096);
                        let is_last_segment = seg_idx == total_segments - 1;

                        // Try to edit the status message for the very first chunk
                        let start = if !first_handled && status_msg_id.is_some() {
                            let sid = status_msg_id.unwrap();
                            let mut req = bot.edit_message_text(target_chat_id, sid, &chunks[0]);
                            if *use_html {
                                req = req.parse_mode(teloxide::types::ParseMode::Html);
                            }
                            match req.await {
                                Ok(_) => {
                                    first_handled = true;
                                    1
                                }
                                Err(e) => {
                                    warn!("Failed to edit status message, sending new: {e}");
                                    0
                                }
                            }
                        } else {
                            0
                        };

                        for (i, chunk) in chunks[start..].iter().enumerate() {
                            let mut req = bot.send_message(target_chat_id, chunk);
                            if *use_html {
                                req = req.parse_mode(teloxide::types::ParseMode::Html);
                            }
                            if !first_handled && i == 0 {
                                first_handled = true;
                                if let Some(reply_id) = target_reply_id {
                                    req = req.reply_parameters(ReplyParameters::new(reply_id));
                                }
                            }
                            // Attach mini app button to the last chunk of the last segment
                            if is_last_segment && i == chunks[start..].len() - 1 {
                                if let Some(ref kb) = miniapp_keyboard {
                                    req = req.reply_markup(kb.clone());
                                }
                            }
                            if let Err(e) = req.await {
                                error!("Failed to send message chunk: {e}");
                            }
                        }
                    }
                }

                // Set chat menu button if requested
                if let Some((app_name, button_text)) = menubutton {
                    let mb = if app_name == "commands" {
                        Some(MenuButton::Commands)
                    } else if !base_url.is_empty() {
                        let app_url = format!("{base_url}/apps/{app_name}/index.html");
                        app_url.parse::<reqwest::Url>().ok().map(|url| {
                            MenuButton::WebApp {
                                text: button_text.clone(),
                                web_app: WebAppInfo { url },
                            }
                        })
                    } else {
                        None
                    };
                    if let Some(mb) = mb {
                        match bot
                            .set_chat_menu_button()
                            .chat_id(target_chat_id)
                            .menu_button(mb)
                            .await
                        {
                            Ok(_) => info!("Set menu button to '{button_text}' for chat {target_chat_id}"),
                            Err(e) => error!("Failed to set menu button: {e}"),
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

/// Split a message into segments of (text, is_html).
/// Plain text outside <html>...</html> tags becomes (text, false).
/// Content inside <html>...</html> tags becomes (content, true).
fn split_html_segments(message: &str) -> Vec<(String, bool)> {
    let mut segments = Vec::new();
    let mut remaining = message;

    while let Some(start) = remaining.find("<html>") {
        // Plain text before the <html> tag
        let before = remaining[..start].trim();
        if !before.is_empty() {
            segments.push((before.to_string(), false));
        }
        remaining = &remaining[start + 6..];

        if let Some(end) = remaining.find("</html>") {
            let html = remaining[..end].trim();
            if !html.is_empty() {
                segments.push((html.to_string(), true));
            }
            remaining = &remaining[end + 7..];
        } else {
            // No closing tag — treat rest as HTML
            let html = remaining.trim();
            if !html.is_empty() {
                segments.push((html.to_string(), true));
            }
            remaining = "";
        }
    }

    // Remaining plain text after last </html>
    let after = remaining.trim();
    if !after.is_empty() {
        segments.push((after.to_string(), false));
    }

    // If no segments found, return the whole message as plain text
    if segments.is_empty() {
        segments.push((message.to_string(), false));
    }

    segments
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Handle GET /cron/:job_id — triggered by cron-job.org.
/// Reads job config from cron-jobs.json and injects directly into the message queue.
async fn handle_cron_trigger(
    tinyclaw_home: &Path,
    job_id: &str,
    incoming_tx: &mpsc::Sender<IncomingMessage>,
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
    let prompt = job.get("prompt").and_then(|p| p.as_str()).unwrap_or("");

    // chat_id may be stored as string or number
    let chat_id: Option<i64> = job
        .get("chat_id")
        .and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.parse().ok())));

    let now_millis = now_millis();
    let rand_val: u32 = rand::random();

    let now_sgt = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
    let ts = now_sgt.format("%Y-%m-%d %H:%M:%S SGT");
    let message = format!("[{ts}] [CRON JOB: {name}]\n{prompt}");
    let message_id = format!("cron_{job_id}_{now_millis}_{rand_val:08x}");

    let msg = IncomingMessage {
        channel: "telegram".into(),
        sender: "Cron".into(),
        sender_id: chat_id.map(|c| c.to_string()).unwrap_or_else(|| format!("cron_{job_id}")),
        message,
        timestamp: now_millis,
        message_id: message_id.clone(),
        files: vec![],
        chat_id,
        reply_to_message_id: None,
    };

    info!("Cron trigger: {job_id} ({name})");
    if incoming_tx.send(msg).await.is_err() {
        error!("Failed to inject cron message for job {job_id}");
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "Queue send failed");
    }

    // Auto-delete one-shot jobs after firing
    let recurring = job.get("recurring").and_then(|r| r.as_bool()).unwrap_or(false);
    if !recurring {
        info!("One-shot job {job_id}: removing from cron-jobs.json");
        if let Ok(mut jobs_obj) = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(&jobs_file).unwrap_or_default(),
        ) {
            if let Some(jobs_map) = jobs_obj.get_mut("jobs").and_then(|j| j.as_object_mut()) {
                jobs_map.remove(job_id);
                if let Ok(updated) = serde_json::to_string_pretty(&jobs_obj) {
                    std::fs::write(&jobs_file, updated).ok();
                }
            }
        }
    }

    (axum::http::StatusCode::OK, "OK")
}
