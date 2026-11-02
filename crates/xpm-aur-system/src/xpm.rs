//! Adapter for all package transactions through xpm.
//!
//! xpm-aur never calls pacman to install or remove packages. It may use
//! `makepkg` to build AUR sources, but every resulting transaction is handed to
//! `xpm`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use xpm_aur_core::process::{CommandRunner, CommandSpec, ProcessError, StdioMode};

use crate::process::SystemRunner;

#[derive(Debug, Error)]
pub enum XpmError {
    #[error(transparent)]
    Process(#[from] ProcessError),

    #[error("{command} failed with status {code:?}")]
    Failed { command: String, code: Option<i32> },
}

/// Thin, explicit wrapper around the xpm CLI.
pub struct XpmClient {
    runner: Arc<dyn CommandRunner>,
    program: String,
    sudo: Option<String>,
    no_confirm: bool,
}

impl XpmClient {
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        program: impl Into<String>,
        sudo: Option<String>,
        no_confirm: bool,
    ) -> Self {
        Self {
            runner,
            program: program.into(),
            sudo,
            no_confirm,
        }
    }

    /// Convenience constructor used by the CLI.
    pub fn system(no_confirm: bool) -> Self {
        Self::new(
            Arc::new(SystemRunner),
            "xpm",
            Some("sudo".into()),
            no_confirm,
        )
    }

    pub fn sync(&self) -> Result<(), XpmError> {
        self.run(["sync"], &[], &[])?;
        Ok(())
    }

    pub fn upgrade(&self) -> Result<(), XpmError> {
        self.run(["upgrade"], &[], &[])?;
        Ok(())
    }

    pub fn install_official(
        &self,
        names: &[String],
        asdeps: bool,
        needed: bool,
    ) -> Result<(), XpmError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut options = Vec::new();
        if asdeps {
            options.push("--as-deps");
        }
        if needed {
            options.push("--needed");
        }
        self.run(["install"], &options, names)?;
        Ok(())
    }

    pub fn install_files(&self, paths: &[PathBuf], asdeps: bool) -> Result<(), XpmError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut options = Vec::new();
        if asdeps {
            options.push("--as-deps");
        }
        let names: Vec<OsString> = paths
            .iter()
            .map(|path| path.as_os_str().to_os_string())
            .collect();
        self.run_os(["install"], &options, &names)?;
        Ok(())
    }

    pub fn remove(&self, names: &[String], recursive: bool, nosave: bool) -> Result<(), XpmError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut options = Vec::new();
        if recursive {
            options.push("--recursive");
        }
        if nosave {
            options.push("--nosave");
        }
        self.run(["remove"], &options, names)?;
        Ok(())
    }

    fn run(
        &self,
        command: impl IntoIterator<Item = &'static str>,
        options: &[&str],
        names: &[String],
    ) -> Result<(), XpmError> {
        let names: Vec<OsString> = names.iter().map(OsString::from).collect();
        self.run_os(command, options, &names)
    }

    fn run_os(
        &self,
        command: impl IntoIterator<Item = &'static str>,
        options: &[&str],
        names: &[OsString],
    ) -> Result<(), XpmError> {
        let own_root = is_effective_root();
        let mut args: Vec<OsString> = Vec::new();
        if self.no_confirm {
            args.push("--no-confirm".into());
        }
        args.extend(command.into_iter().map(OsString::from));
        args.extend(options.iter().map(OsString::from));
        if !names.is_empty() {
            args.push("--".into());
            args.extend(names.iter().cloned());
        }

        let (program, full_args) = if !own_root {
            if let Some(sudo) = &self.sudo {
                let mut full = Vec::with_capacity(args.len() + 1);
                full.push(OsString::from(&self.program));
                full.extend(args);
                (sudo.clone(), full)
            } else {
                (self.program.clone(), args)
            }
        } else {
            (self.program.clone(), args)
        };

        let spec = CommandSpec::new(program)
            .args(full_args.clone())
            .stdio(StdioMode::Inherit);
        let output = self.runner.run(&spec)?;
        if output.success {
            Ok(())
        } else {
            Err(XpmError::Failed {
                command: format!(
                    "{} {}",
                    spec.program.to_string_lossy(),
                    full_args
                        .iter()
                        .map(|arg| arg.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                code: output.code,
            })
        }
    }
}

fn is_effective_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid() == 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use std::path::Path;
    use xpm_aur_core::process::{CommandOutput, ProcessError};

    #[derive(Default)]
    struct RecordingRunner {
        calls: Mutex<Vec<CommandSpec>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, ProcessError> {
            self.calls.lock().unwrap().push(spec.clone());
            Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn sends_install_to_xpm_with_global_options_first() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", None, true);
        client
            .install_official(&["foo".into(), "bar".into()], true, true)
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let args: Vec<String> = calls[0]
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--no-confirm",
                "install",
                "--as-deps",
                "--needed",
                "--",
                "foo",
                "bar"
            ]
        );
    }

    #[test]
    fn sends_local_files_to_xpm() {
        let runner = Arc::new(RecordingRunner::default());
        let client = XpmClient::new(runner.clone(), "xpm", None, true);
        client
            .install_files(&[Path::new("/tmp/demo.pkg.tar.zst").to_path_buf()], false)
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert!(
            calls[0]
                .args
                .iter()
                .any(|arg| arg.to_string_lossy() == "/tmp/demo.pkg.tar.zst")
        );
    }
}
