//! Fabric pattern integration for LLM-powered features.
//!
//! Shells out to the `fabric` CLI binary for pattern-based text processing
//! (extract_wisdom, summarize, etc.). Used by the intel module for daily
//! digests and weekly reviews.

use eyre::{Context, Result};
use std::process::Command;
use std::time::Duration;

/// Run a Fabric pattern against input text with a timeout.
/// Returns the pattern output or an error if fabric is not available or times out.
pub fn run_pattern(pattern: &str, input: &str, timeout_secs: u64) -> Result<String> {
    let mut child = Command::new("fabric")
        .args(["--pattern", pattern])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run fabric - is it installed?")?;

    // Write input to stdin
    {
        use std::io::Write;
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(input.as_bytes())?;
        }
        // Drop stdin to signal EOF
        child.stdin.take();
    }

    // Wait with timeout
    let timeout = Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output()?;
                if !status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(eyre::eyre!("fabric pattern '{}' failed: {}", pattern, stderr));
                }
                return Ok(String::from_utf8_lossy(&output.stdout).to_string());
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return Err(eyre::eyre!(
                        "fabric pattern '{}' timed out after {}s",
                        pattern,
                        timeout_secs
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(eyre::eyre!("failed to check fabric process status: {}", e)),
        }
    }
}

/// Check if fabric is available on the system.
pub fn is_available() -> bool {
    // Just check if the binary exists on PATH - don't invoke it, some subcommands hang
    which::which("fabric").is_ok()
}

/// Truncate input text to approximately max_tokens (estimated at ~4 chars per token).
pub fn truncate_input(input: &str, max_tokens: usize) -> &str {
    let max_chars = max_tokens * 4;
    if input.len() <= max_chars {
        input
    } else {
        // Find a char boundary near the limit
        let end = input.floor_char_boundary(max_chars);
        &input[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available_returns_bool() {
        // Just verify it doesn't panic - result depends on whether fabric is installed
        let _ = is_available();
    }

    #[test]
    fn test_truncate_input_short() {
        let input = "hello world";
        assert_eq!(truncate_input(input, 50000), "hello world");
    }

    #[test]
    fn test_truncate_input_long() {
        let input = "a".repeat(300_000);
        let result = truncate_input(&input, 50000);
        assert!(result.len() <= 200_000);
    }
}
