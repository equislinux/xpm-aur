//! xpm-aur command line interface.

mod compat;
mod config;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use xpm_aur_app::{App, AppPaths, InstallOptions, RemoveOptions};
use xpm_aur_aur::{AurGit, AurPackage, AurRpc, SearchBy};
use xpm_aur_build::{BuildConfig, XpmBuilder};
use xpm_aur_core::process::CommandRunner;
use xpm_aur_system::{PacmanDatabase, StateStore, SystemCatalog, SystemRunner, XpmClient};

use crate::config::{Config, expand_path};

#[derive(Debug, Parser)]
#[command(
    name = "xpm-aur",
    version,
    about = "AUR helper for X Linux; AUR queries and builds, transactions through xpm",
    after_help = "Pacman/yay-style forms are accepted: -S, -Syu, -Ss, -Si, -Rns, -Qm, -Sc."
)]
struct Cli {
    /// Increase verbosity (-v, -vv).
    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Do not ask for confirmation.
    #[arg(long = "noconfirm", alias = "no-confirm", global = true)]
    no_confirm: bool,

    /// Pass --needed to xpm for official packages.
    #[arg(long, global = true)]
    needed: bool,

    /// Mark the requested packages as dependencies.
    #[arg(long, global = true)]
    asdeps: bool,

    /// Rebuild the selected AUR packages even when the version is unchanged.
    #[arg(long, global = true)]
    rebuild: bool,

    /// Only use AUR candidates.
    #[arg(long, global = true, conflicts_with = "repo")]
    aur: bool,

    /// Only use official candidates.
    #[arg(long, global = true)]
    repo: bool,

    /// Check VCS package HEADs during upgrade.
    #[arg(long, global = true)]
    devel: bool,

    /// Print the transaction plan and exit before building/installing.
    #[arg(long, global = true)]
    print: bool,

    /// Remove conflicting/replaced packages automatically.
    #[arg(long, global = true)]
    remove_conflicts: bool,

    /// Skip source package checks (makepkg --nocheck).
    #[arg(long, global = true)]
    nocheck: bool,

    /// Configuration file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override the build directory.
    #[arg(long = "build-dir", global = true)]
    build_dir: Option<PathBuf>,

    /// Override the cache root (build, packages and sources).
    #[arg(long = "cache-dir", global = true)]
    cache_dir: Option<PathBuf>,

    /// Override the state file.
    #[arg(long, global = true)]
    state: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Search the AUR.
    Search {
        query: String,
        #[arg(long, default_value = "name-desc", value_parser = parse_by)]
        by: SearchBy,
    },
    /// Show package information (official and AUR unless restricted).
    Info { packages: Vec<String> },
    /// Build and install AUR packages (and their official/AUR dependencies).
    Install { packages: Vec<String> },
    /// Remove packages through xpm.
    Remove {
        packages: Vec<String>,
        #[arg(short = 's', long)]
        recursive: bool,
        #[arg(short = 'n', long)]
        nosave: bool,
    },
    /// Upgrade official packages with xpm and AUR packages with xpm-aur.
    Upgrade,
    /// List AUR packages tracked by xpm-aur, or all foreign packages.
    Query {
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        foreign: bool,
    },
    /// Synchronize official databases through xpm.
    Sync,
    /// Adopt AUR packages installed by another helper (e.g. yay).
    Adopt,
    /// Clone/update AUR PKGBUILDs for inspection (yay -G).
    #[command(name = "getpkgbuild", visible_alias = "get-pkgbuild")]
    GetPkgbuild {
        packages: Vec<String>,
        #[arg(short = 'p', long)]
        print: bool,
    },
    /// Remove cached build trees and artifacts.
    Clean {
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 14)]
        keep_days: u64,
    },
}

fn main() -> ExitCode {
    let argv = compat::rewrite_args(std::env::args_os().collect());
    let cli = Cli::parse_from(argv);
    init_tracing(cli.verbose);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xpm-aur: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let config = Config::load(cli.config.as_deref())?;
    let paths = resolve_paths(&config, &cli);
    let state_path = cli
        .state
        .clone()
        .or(config.state.clone())
        .unwrap_or_else(StateStore::default_path);

    let runner: Arc<dyn CommandRunner> = Arc::new(SystemRunner);
    let database = PacmanDatabase::new(Path::new("/"));
    let rpc = AurRpc::new(config.rpc_url.clone());
    let catalog = SystemCatalog::new(database, rpc);
    let xpm = XpmClient::new(
        runner.clone(),
        config.xpm.clone(),
        config.sudo.clone(),
        cli.no_confirm,
    );
    let git = AurGit::new(config.git_url.clone(), runner.clone());

    let run_checks = config.run_checks && !cli.nocheck;
    let mut build_config = BuildConfig::new(paths.package_root.clone());
    build_config.program = config.xpm.clone();
    build_config.srcdest = Some(paths.sources_root.clone());
    build_config.run_checks = run_checks;
    build_config.extra_args = config.extra_args.clone();
    let builder = XpmBuilder::new(runner, build_config);
    let state = StateStore::at(state_path);

    let app = App::new(catalog, xpm, git, builder, state, paths, cli.no_confirm);

    let mut install_options = InstallOptions {
        needed: cli.needed,
        force_aur: cli.aur,
        force_repo: cli.repo,
        rebuild: cli.rebuild,
        remove_conflicts: cli.remove_conflicts,
        include_check_depends: run_checks,
        print_only: cli.print,
        target_asdeps: cli.asdeps,
    };

    match cli.command {
        Command::Search { query, by } => {
            let mut found = false;
            if !cli.aur {
                let official = app.search_official(&query)?;
                if !official.is_empty() {
                    print_official_search(&official);
                    found = true;
                }
            }
            if !cli.repo {
                let results = app.search(&query, by)?;
                if !results.is_empty() {
                    if found {
                        println!();
                    }
                    print_search(&results);
                    found = true;
                }
            }
            if !found {
                println!("No results.");
            }
        }
        Command::Info { packages } => {
            if packages.is_empty() {
                anyhow::bail!("info needs at least one package");
            }
            let mut found = false;
            if !cli.aur {
                for package in &packages {
                    if let Some(candidate) = app.info_official(package)? {
                        if found {
                            println!();
                        }
                        print_official_info(&candidate);
                        found = true;
                    }
                }
            }
            if !cli.repo {
                let results = app.info(&packages)?;
                if !results.is_empty() {
                    if found {
                        println!();
                    }
                    print_info(&results);
                    found = true;
                }
            }
            if !found {
                anyhow::bail!("no package found");
            }
        }
        Command::Install { packages } => {
            if packages.is_empty() {
                anyhow::bail!("install needs at least one package");
            }
            let outcome = app.install(&packages, &install_options)?;
            if !cli.print {
                println!(
                    "Done: {} official, {} AUR base(s), {} AUR package(s), {} removal(s).",
                    outcome.official, outcome.aur_bases, outcome.aur_packages, outcome.removals
                );
            }
        }
        Command::Remove {
            packages,
            recursive,
            nosave,
        } => {
            if packages.is_empty() {
                anyhow::bail!("remove needs at least one package");
            }
            app.remove(&packages, &RemoveOptions { recursive, nosave })?;
            println!("Removed {}.", packages.join(", "));
        }
        Command::Upgrade => {
            install_options.print_only = false;
            install_options.target_asdeps = false;
            let outcome = app.upgrade(cli.devel, cli.aur, cli.repo, &install_options)?;
            if outcome.aur_updates.is_empty() {
                if outcome.official_upgraded {
                    println!("Official packages are up to date; no AUR updates.");
                } else {
                    println!("No AUR updates.");
                }
            } else {
                println!("AUR updated: {}", outcome.aur_updates.join(", "));
            }
        }
        Command::Query { quiet, foreign } => {
            if foreign {
                let packages = app.query_foreign()?;
                if packages.is_empty() {
                    if !quiet {
                        println!("No foreign packages installed.");
                    }
                } else {
                    for package in packages {
                        if quiet {
                            println!("{}", package.meta.name);
                        } else {
                            let reason = match package.reason {
                                Some(xpm_aur_core::catalog::InstallReason::Dependency) => {
                                    "dependency"
                                }
                                _ => "explicit",
                            };
                            println!("{} {} ({reason})", package.meta.name, package.meta.version);
                        }
                    }
                }
                return Ok(());
            }
            let records = app.query()?;
            if records.is_empty() {
                if !quiet {
                    println!("No AUR packages tracked by xpm-aur.");
                }
            } else {
                for record in records {
                    if quiet {
                        for package in &record.packages {
                            println!("{package}");
                        }
                    } else {
                        println!(
                            "{}/{} {}{} [{}]",
                            record.base,
                            record.version,
                            if record.explicit {
                                " explicit"
                            } else {
                                " dependency"
                            },
                            if record.commit.is_empty() {
                                String::new()
                            } else {
                                format!(" @{}", &record.commit[..record.commit.len().min(10)])
                            },
                            record.packages.join(", ")
                        );
                    }
                }
            }
        }
        Command::Sync => {
            app.sync()?;
            println!("Official databases synchronized.");
        }
        Command::Adopt => {
            let count = app.adopt()?;
            println!("Adopted {count} AUR package(s).");
        }
        Command::GetPkgbuild { packages, print } => {
            let paths = app.getpkgbuild(&packages)?;
            for path in paths {
                if print {
                    let text = std::fs::read_to_string(&path)
                        .with_context(|| format!("could not read {}", path.display()))?;
                    print!("{text}");
                } else {
                    println!("{}", path.display());
                }
            }
        }
        Command::Clean { all, keep_days } => {
            let removed = app.clean(all, keep_days)?;
            println!("Removed {removed} cache entr(y/ies).");
        }
    }
    Ok(())
}

fn resolve_paths(config: &Config, cli: &Cli) -> AppPaths {
    let mut paths = AppPaths::default();
    if let Some(cache) = &config.paths.cache {
        let cache = expand_path(cache);
        paths.build_root = cache.join("build");
        paths.package_root = cache.join("packages");
        paths.sources_root = cache.join("sources");
        paths.cache_root = cache;
    }
    if let Some(build) = &config.paths.build {
        paths.build_root = expand_path(build);
    }
    if let Some(packages) = &config.paths.packages {
        paths.package_root = expand_path(packages);
    }
    if let Some(sources) = &config.paths.sources {
        paths.sources_root = expand_path(sources);
    }
    if let Some(cache) = &cli.cache_dir {
        paths.build_root = cache.join("build");
        paths.package_root = cache.join("packages");
        paths.sources_root = cache.join("sources");
        paths.cache_root = cache.clone();
    }
    if let Some(build) = &cli.build_dir {
        paths.build_root = build.clone();
    }
    paths
}

fn parse_by(value: &str) -> Result<SearchBy, String> {
    match value {
        "name-desc" => Ok(SearchBy::NameDesc),
        "name" => Ok(SearchBy::Name),
        "provides" => Ok(SearchBy::Provides),
        "depends" => Ok(SearchBy::Depends),
        "makedepends" => Ok(SearchBy::MakeDepends),
        "checkdepends" => Ok(SearchBy::CheckDepends),
        "optdepends" => Ok(SearchBy::OptDepends),
        "conflicts" => Ok(SearchBy::Conflicts),
        "replaces" => Ok(SearchBy::Replaces),
        "groups" => Ok(SearchBy::Groups),
        "keywords" => Ok(SearchBy::Keywords),
        "maintainer" => Ok(SearchBy::Maintainer),
        "comaintainers" => Ok(SearchBy::Comaintainers),
        other => Err(format!("unknown search field: {other}")),
    }
}

fn print_official_search(results: &[xpm_aur_core::catalog::Candidate]) {
    for package in results {
        let repo = match &package.source {
            xpm_aur_core::catalog::CandidateSource::Official { repo, .. } => repo.as_str(),
            _ => "?",
        };
        println!("{}/{} {}", repo, package.meta.name, package.meta.version);
        if let Some(description) = &package.meta.description {
            println!("    {description}");
        }
    }
}

fn print_official_info(package: &xpm_aur_core::catalog::Candidate) {
    let repo = match &package.source {
        xpm_aur_core::catalog::CandidateSource::Official { repo, .. } => repo.as_str(),
        _ => "?",
    };
    println!("Repository      : {repo}");
    println!("Name            : {}", package.meta.name);
    println!("Version         : {}", package.meta.version);
    if let Some(description) = &package.meta.description {
        println!("Description     : {description}");
    }
    if let Some(url) = &package.meta.url {
        println!("URL             : {url}");
    }
    if !package.meta.depends.is_empty() {
        let deps = package
            .meta
            .depends
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        println!("Depends         : {}", deps.join(" "));
    }
    if !package.meta.provides.is_empty() {
        let provides = package
            .meta
            .provides
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        println!("Provides        : {}", provides.join(" "));
    }
}

fn print_search(results: &[AurPackage]) {
    if results.is_empty() {
        println!("No AUR results.");
        return;
    }
    for package in results {
        println!(
            "aur/{} {} ({}, {:.2})",
            package.name, package.version, package.votes, package.popularity
        );
        if let Some(description) = &package.description {
            println!("    {description}");
        }
    }
}

fn print_info(results: &[AurPackage]) {
    for (index, package) in results.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("Repository      : aur");
        println!("Name            : {}", package.name);
        println!("Package base    : {}", package.package_base);
        println!("Version         : {}", package.version);
        if let Some(description) = &package.description {
            println!("Description     : {description}");
        }
        if let Some(url) = &package.url {
            println!("URL             : {url}");
        }
        println!(
            "Votes           : {} ({:.2})",
            package.votes, package.popularity
        );
        println!(
            "Maintainer      : {}",
            package.maintainer.as_deref().unwrap_or("orphan")
        );
        println!(
            "Out of date     : {}",
            package.out_of_date.map(|_| "yes").unwrap_or("no")
        );
        if !package.depends.is_empty() {
            println!("Depends         : {}", package.depends.join(" "));
        }
        if !package.make_depends.is_empty() {
            println!("Make depends    : {}", package.make_depends.join(" "));
        }
        if !package.check_depends.is_empty() {
            println!("Check depends   : {}", package.check_depends.join(" "));
        }
        if !package.opt_depends.is_empty() {
            println!("Optional deps   : {}", package.opt_depends.join(" "));
        }
        if !package.provides.is_empty() {
            println!("Provides        : {}", package.provides.join(" "));
        }
        if !package.conflicts.is_empty() {
            println!("Conflicts       : {}", package.conflicts.join(" "));
        }
        if !package.replaces.is_empty() {
            println!("Replaces        : {}", package.replaces.join(" "));
        }
    }
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// Keeps `Context` available for future fallible setup steps and documents the
/// expected anyhow usage in this binary.
#[allow(dead_code)]
fn _context<T>(value: anyhow::Result<T>) -> anyhow::Result<T> {
    value.context("xpm-aur setup failed")
}
