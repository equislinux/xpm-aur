//! Build orchestration errors.

use thiserror::Error;

use xpm_aur_core::process::ProcessError;

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("source packages must be built as a normal user, not root")]
    RunningAsRoot,

    #[error("could not create build directory {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Process(#[from] ProcessError),

    #[error("makepkg produced no package artifacts in {0}")]
    NoArtifacts(String),

    #[error("invalid build result at {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

pub type BuildResult<T> = Result<T, BuildError>;
