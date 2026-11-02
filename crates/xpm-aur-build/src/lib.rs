//! Build orchestration for xpm-aur.
//!
//! xpm-aur no longer invokes makepkg directly. It asks `xpm build` to do it,
//! which keeps the build environment, artifact discovery and future sandboxing
//! in one place: xpm.

pub mod error;
pub mod makepkg;

pub use error::{BuildError, BuildResult};
pub use makepkg::{BuildArtifacts, BuildConfig, BuiltPackage, XpmBuilder};
