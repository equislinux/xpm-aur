//! Production implementation of the command runner port.

use std::process::{Command, Stdio};

use xpm_aur_core::process::{CommandOutput, CommandRunner, CommandSpec, ProcessError, StdioMode};

/// Runs commands on the host, optionally inheriting the terminal.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, ProcessError> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        match spec.stdio {
            StdioMode::Inherit => {
                command.stdin(Stdio::inherit());
                command.stdout(Stdio::inherit());
                command.stderr(Stdio::inherit());
                let status = command.status().map_err(|source| ProcessError::Spawn {
                    program: spec.program.to_string_lossy().into_owned(),
                    source,
                })?;
                Ok(CommandOutput {
                    success: status.success(),
                    code: status.code(),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
            StdioMode::Capture => {
                command.stdin(Stdio::null());
                let output = command.output().map_err(|source| ProcessError::Spawn {
                    program: spec.program.to_string_lossy().into_owned(),
                    source,
                })?;
                Ok(CommandOutput {
                    success: output.status.success(),
                    code: output.status.code(),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            }
        }
    }
}
