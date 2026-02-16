use thiserror::Error;

#[derive(Error, Debug)]
pub enum InvokeError {
    #[error("command failed: {0}")]
    CommandFailed(String),

    #[error("command not found: {0}")]
    CommandNotFound(String),

    #[error("command timed out after {0}s: {1}")]
    Timeout(u64, String),

    #[error("cancelled by user")]
    Cancelled,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
