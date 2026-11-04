//! Application layer for xpm-aur.
//!
//! The modules here compose the pure resolver, AUR adapters, makepkg builder
//! and xpm transaction adapter. They contain policy (order of operations,
//! confirmation, state updates), not protocol or filesystem details.

pub mod app;
pub mod error;
pub mod install;
pub mod operations;
pub mod progress;
mod scheduler;

pub use app::{App, AppDependencies, AppPaths, build_app};
pub use error::{AppError, AppResult};
pub use install::{InstallOptions, InstallOutcome, default_jobs};
pub use operations::{RemoveOptions, UpgradeOutcome};
