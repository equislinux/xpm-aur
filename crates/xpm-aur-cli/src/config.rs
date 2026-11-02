//! Configuration loading.
//!
//! Defaults work on a stock X/Arch system. Every path can be overridden by the
//! TOML file or by a command-line flag; the CLI layer resolves precedence.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub aur: AurConfig,
    pub build: BuildSettings,
    pub commands: CommandSettings,
    pub paths: PathSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AurConfig {
    pub rpc_url: Option<String>,
    pub git_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct BuildSettings {
    pub run_checks: bool,
    pub extra_args: Vec<String>,
}

impl Default for BuildSettings {
    fn default() -> Self {
        Self {
            run_checks: true,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct CommandSettings {
    pub xpm: String,
    pub sudo: String,
}

impl Default for CommandSettings {
    fn default() -> Self {
        Self {
            xpm: "xpm".into(),
            sudo: "sudo".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PathSettings {
    pub build: Option<String>,
    pub packages: Option<String>,
    pub sources: Option<String>,
    pub cache: Option<String>,
    pub state: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub rpc_url: Option<String>,
    pub git_url: String,
    pub xpm: String,
    pub sudo: Option<String>,
    pub run_checks: bool,
    pub extra_args: Vec<String>,
    pub paths: PathSettings,
    pub state: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rpc_url: None,
            git_url: "https://aur.archlinux.org".into(),
            xpm: "xpm".into(),
            sudo: Some("sudo".into()),
            run_checks: true,
            extra_args: Vec::new(),
            paths: PathSettings::default(),
            state: None,
        }
    }
}

impl Config {
    pub fn load(path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        let path = match path {
            Some(path) => Some(path.to_path_buf()),
            None => default_config_path(),
        };
        let Some(path) = path else {
            return Ok(Self::default());
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|err| anyhow::anyhow!("could not read {}: {err}", path.display()))?;
        let file: FileConfig = toml::from_str(&text)
            .map_err(|err| anyhow::anyhow!("could not parse {}: {err}", path.display()))?;

        Ok(Self {
            rpc_url: file.aur.rpc_url,
            git_url: file
                .aur
                .git_url
                .unwrap_or_else(|| "https://aur.archlinux.org".into()),
            xpm: file.commands.xpm,
            sudo: (!file.commands.sudo.is_empty()).then_some(file.commands.sudo),
            run_checks: file.build.run_checks,
            extra_args: file.build.extra_args,
            paths: file.paths.clone(),
            state: file.paths.state.as_deref().map(expand_path),
        })
    }
}

fn default_config_path() -> Option<PathBuf> {
    std::env::var("XPM_AUR_CONFIG")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|base| base.join("xpm-aur/config.toml")))
}

pub fn expand_path(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}
