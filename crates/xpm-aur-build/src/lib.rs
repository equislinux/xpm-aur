//! Build orchestration for xpm-aur.
//!
//! The crate only knows how to run `makepkg` and find the resulting package
//! files. It does not resolve dependencies or install anything.

pub mod error;
pub mod makepkg;

pub use error::{BuildError, BuildResult};
pub use makepkg::{
    BuildArtifacts, BuildConfig, BuiltPackage, MakepkgBuilder, package_name_from_filename,
};
