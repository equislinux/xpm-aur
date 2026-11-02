//! Port for running external commands.
//!
//! All crates that need `git`, `makepkg` or `xpm` depend on this trait instead
//! of `std::process::Command`, which keeps them testable and makes privilege
//! escalation explicit at the adapter boundary.

use std::ffi::OsString;
use std::path::PathBuf;

use thiserror::Error;

/// How a child process should use the terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdioMode {
    /// Forward the child's output to the user's terminal.
    Inherit,
    /// Capture stdout and stderr for parsing.
    Capture,
}

/// A declarative description of one external command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub stdio: StdioMode,
}

impl CommandSpec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            stdio: StdioMode::Capture,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn stdio(mut self, stdio: StdioMode) -> Self {
        self.stdio = stdio;
        self
    }
}

/// Captured result of a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Failure to start a command (a non-zero exit is reported in the output).
#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to run {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("command {program} failed with status {code:?}: {stderr}")]
    Failed {
        program: String,
        code: Option<i32>,
        stderr: String,
    },
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, ProcessError>;
}

/// Convenience for callers that treat a non-zero exit as an error.
pub fn run_checked(
    runner: &dyn CommandRunner,
    spec: &CommandSpec,
) -> Result<CommandOutput, ProcessError> {
    let output = runner.run(spec)?;
    if output.success {
        Ok(output)
    } else {
        Err(ProcessError::Failed {
            program: spec.program.to_string_lossy().into_owned(),
            code: output.code,
            stderr: output.stderr.trim().to_string(),
        })
    }
}
