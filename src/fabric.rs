//! Fabric pattern integration for LLM-powered features.
//!
//! Shells out to the `fabric` CLI binary for pattern-based text processing
//! (extract_wisdom, summarize, etc.). Used by the intel module for daily
//! digests and weekly reviews.

use eyre::{Context, Result};
use std::process::Command;

/// Run a Fabric pattern against input text.
/// Returns the pattern output or an error if fabric is not available.
pub fn run_pattern(pattern: &str, input: &str) -> Result<String> {
    let output = Command::new("fabric")
        .args(["--pattern", pattern])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(input.as_bytes())?;
            }
            child.wait_with_output()
        })
        .context("failed to run fabric - is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("fabric pattern '{}' failed: {}", pattern, stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check if fabric is available on the system.
pub fn is_available() -> bool {
    Command::new("fabric")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available_returns_bool() {
        // Just verify it doesn't panic - result depends on whether fabric is installed
        let _ = is_available();
    }
}
