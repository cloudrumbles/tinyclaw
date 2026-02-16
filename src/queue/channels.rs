/// A message arriving from a channel (telegram, cron) to the queue processor.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IncomingMessage {
    pub channel: String,
    pub sender: String,
    pub sender_id: String,
    pub message: String,
    pub timestamp: u64,
    pub message_id: String,
    pub files: Vec<String>,
    pub chat_id: Option<i64>,
    pub reply_to_message_id: Option<i32>,
}

/// A message from the queue processor back to a channel.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum OutgoingMessage {
    Response {
        sender: String,
        message: String,
        original_message: String,
        timestamp: u64,
        message_id: String,
        files: Vec<String>,
        chat_id: Option<i64>,
        reply_to_message_id: Option<i32>,
        /// If set, attach an InlineKeyboardButton::WebApp to the response.
        /// (app_name, button_text) — URL is derived from the webhook base URL.
        miniapp: Option<(String, String)>,
        /// If set, pin the miniapp as the chat's persistent menu button.
        /// (app_name, button_text) — URL is derived from the webhook base URL.
        menubutton: Option<(String, String)>,
    },
    TypingStart {
        message_id: String,
        chat_id: i64,
    },
    TypingStop {
        message_id: String,
    },
    StatusUpdate {
        message_id: String,
        chat_id: i64,
        status: String,
    },
}
