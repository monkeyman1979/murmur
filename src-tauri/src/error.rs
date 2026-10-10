use std::path::PathBuf;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("streaming is not supported by this engine")]
    StreamingUnsupported,

    #[error("model not found at {0}")]
    ModelNotFound(PathBuf),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("backend error: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, Error>;
