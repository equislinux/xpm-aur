//! Errors surfaced by use cases.

use thiserror::Error;

use xpm_aur_aur::AurError;
use xpm_aur_build::BuildError;
use xpm_aur_core::error::CoreError;
use xpm_aur_system::{DbError, StateError, XpmError};

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] CoreError),

    #[error(transparent)]
    Aur(#[from] AurError),

    #[error(transparent)]
    Build(#[from] BuildError),

    #[error(transparent)]
    Db(#[from] DbError),

    #[error(transparent)]
    State(#[from] StateError),

    #[error(transparent)]
    Xpm(#[from] XpmError),

    #[error("install cancelled")]
    Cancelled,

    #[error("AUR package base {base} produced no package matching the plan")]
    MissingArtifact { base: String },

    #[error("no AUR updates available")]
    NoUpdates,

    #[error("no official sync database found; run `xpm-aur sync` first")]
    NoSyncDatabase,

    #[error("I/O error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub type AppResult<T> = Result<T, AppError>;
