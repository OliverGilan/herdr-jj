use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

pub fn checked_output(command: &mut Command, label: &str) -> Result<String> {
    output_text(run(command, label)?, label).map(|value| value.trim().to_owned())
}

pub fn checked_output_raw(command: &mut Command, label: &str) -> Result<String> {
    output_text(run(command, label)?, label)
}

pub fn checked_status(command: &mut Command, label: &str) -> Result<()> {
    let output = run(command, label)?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(command_error(label, &output))
    }
}

fn run(command: &mut Command, label: &str) -> Result<Output> {
    command
        .output()
        .with_context(|| format!("{label} failed to start"))
}

fn output_text(output: Output, label: &str) -> Result<String> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!(command_error(label, &output))
    }
}

fn command_error(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or("no command output");
    format!(
        "{label} failed (exit {}): {detail}",
        output.status.code().unwrap_or(-1)
    )
}
