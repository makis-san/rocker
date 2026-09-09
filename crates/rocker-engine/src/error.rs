use thiserror::Error;

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("docker daemon unreachable: {0}")]
    Unreachable(String),

    #[error("engine API version {found} is below the supported minimum {minimum}")]
    ApiTooOld { found: String, minimum: String },

    #[error("operation not supported by this daemon: {0}")]
    Unsupported(String),

    #[error("docker: {0}")]
    Docker(String),
}
