//! Errors from AUR RPC, git and SRCINFO parsing.

use thiserror::Error;

use xpm_aur_core::process::ProcessError;

#[derive(Debug, Error)]
pub enum AurError {
    #[error("AUR request failed: {0}")]
    Network(String),

    #[error("AUR returned an error: {0}")]
    Api(String),

    #[error("could not decode AUR response: {0}")]
    Json(#[from] serde_json::Error),

    #[error("AUR package not found: {0}")]
    NotFound(String),

    #[error("AUR git error: {0}")]
    Git(#[from] ProcessError),

    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid SRCINFO: {0}")]
    SrcInfo(String),
}

pub type AurResult<T> = Result<T, AurError>;
