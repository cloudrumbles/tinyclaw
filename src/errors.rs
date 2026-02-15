use thiserror::Error;

#[derive(Error, Debug)]
pub enum InvokeError {
    #[error("command failed: {0}")]
    CommandFailed(String),

    #[error("command not found: {0}")]
    CommandNotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Error, Debug)]
pub enum PairingError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
