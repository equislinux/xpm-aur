//! Application services and their dependencies.

use std::path::PathBuf;

use xpm_aur_aur::AurGit;
use xpm_aur_build::MakepkgBuilder;
use xpm_aur_system::{StateStore, SystemCatalog, XpmClient};

use crate::error::AppResult;

/// Runtime dependencies for the use cases.
pub struct App {
    pub(crate) catalog: SystemCatalog,
    pub(crate) xpm: XpmClient,
    pub(crate) git: AurGit,
    pub(crate) builder: MakepkgBuilder,
    pub(crate) state: StateStore,
    pub(crate) paths: AppPaths,
    pub(crate) no_confirm: bool,
}

/// Cache/build locations. Keeping them explicit makes dry runs and tests
/// predictable and prevents accidental writes outside the user's cache.
#[derive(Clone, Debug)]
pub struct AppPaths {
    pub build_root: PathBuf,
    pub package_root: PathBuf,
    pub sources_root: PathBuf,
    pub cache_root: PathBuf,
}

impl Default for AppPaths {
    fn default() -> Self {
        let cache = dirs_cache();
        Self {
            build_root: cache.join("build"),
            package_root: cache.join("packages"),
            sources_root: cache.join("sources"),
            cache_root: cache,
        }
    }
}

fn dirs_cache() -> PathBuf {
    if let Ok(path) = std::env::var("XPM_AUR_CACHE") {
        return PathBuf::from(path);
    }
    dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("xpm-aur")
}

impl App {
    pub fn new(
        catalog: SystemCatalog,
        xpm: XpmClient,
        git: AurGit,
        builder: MakepkgBuilder,
        state: StateStore,
        paths: AppPaths,
        no_confirm: bool,
    ) -> Self {
        Self {
            catalog,
            xpm,
            git,
            builder,
            state,
            paths,
            no_confirm,
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn state(&self) -> &StateStore {
        &self.state
    }
}

/// Dependencies are passed explicitly by the CLI. This exists so tests and
/// future frontends can assemble the app without hidden globals.
pub struct AppDependencies {
    pub catalog: SystemCatalog,
    pub xpm: XpmClient,
    pub git: AurGit,
    pub builder: MakepkgBuilder,
    pub state: StateStore,
    pub paths: AppPaths,
    pub no_confirm: bool,
}

pub fn build_app(deps: AppDependencies) -> App {
    App::new(
        deps.catalog,
        deps.xpm,
        deps.git,
        deps.builder,
        deps.state,
        deps.paths,
        deps.no_confirm,
    )
}

/// Confirm a destructive plan unless `--noconfirm` was passed.
pub(crate) fn confirm(prompt: &str, no_confirm: bool) -> AppResult<()> {
    if no_confirm {
        return Ok(());
    }
    use std::io::{self, Write};
    print!("{prompt} [Y/n] ");
    io::stdout().flush().map_err(|source| crate::AppError::Io {
        path: "stdout".into(),
        source,
    })?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|source| crate::AppError::Io {
            path: "stdin".into(),
            source,
        })?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer.is_empty() || answer == "y" || answer == "yes" {
        Ok(())
    } else {
        Err(crate::AppError::Cancelled)
    }
}
