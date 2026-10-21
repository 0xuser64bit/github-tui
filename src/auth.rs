//! Token resolution.
//!
//! Resolution order, first match wins:
//!   1. `GITHUB_TOKEN` environment variable
//!   2. `GH_TOKEN` environment variable
//!   3. the GitHub CLI (`gh auth token --hostname github.com`), if logged in
//!
//! The token is never logged or rendered anywhere in the UI.

use std::process::{Command, Output};

/// Resolve a GitHub token, or `None` if the user hasn't provided one.
pub fn resolve_token() -> Option<String> {
    resolve_token_with(
        |var| std::env::var(var).ok(),
        || gh_token_command().output(),
    )
}

fn resolve_token_with(
    mut env: impl FnMut(&str) -> Option<String>,
    gh_output: impl FnOnce() -> std::io::Result<Output>,
) -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Some(token) = env(var).and_then(non_empty) {
            return Some(token);
        }
    }
    let output = gh_output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().and_then(non_empty)
}

fn gh_token_command() -> Command {
    let mut command = Command::new("gh");
    command.args(["auth", "token", "--hostname", "github.com"]);
    command
}

/// Trim surrounding whitespace and drop the value if nothing remains.
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn cli_output(success: bool, stdout: &[u8]) -> std::io::Result<Output> {
        Ok(Output {
            status: ExitStatus::from_raw(if success { 0 } else { 1 }),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        })
    }

    #[test]
    fn non_empty_trims_and_filters() {
        assert_eq!(non_empty("  abc \n".to_string()), Some("abc".to_string()));
        assert_eq!(non_empty("   ".to_string()), None);
        assert_eq!(non_empty(String::new()), None);
    }

    #[test]
    fn github_token_takes_precedence_without_calling_cli() {
        let token = resolve_token_with(
            |var| match var {
                "GITHUB_TOKEN" => Some(" github-env-test \n".to_string()),
                _ => panic!("lower-priority environment variable was read"),
            },
            || panic!("CLI must not run when an environment token exists"),
        );
        assert_eq!(token.as_deref(), Some("github-env-test"));
    }

    #[test]
    fn gh_token_follows_missing_or_blank_github_token() {
        for github_token in [None, Some("   \n")] {
            let token = resolve_token_with(
                |var| match var {
                    "GITHUB_TOKEN" => github_token.map(str::to_string),
                    "GH_TOKEN" => Some(" gh-env-test ".to_string()),
                    _ => unreachable!(),
                },
                || panic!("CLI must not run when an environment token exists"),
            );
            assert_eq!(token.as_deref(), Some("gh-env-test"));
        }
    }

    #[test]
    fn cli_is_used_after_both_environment_tokens_are_empty() {
        let token = resolve_token_with(
            |_| Some(" \n".to_string()),
            || cli_output(true, b" cli-test \n"),
        );
        assert_eq!(token.as_deref(), Some("cli-test"));
    }

    #[test]
    fn cli_failures_and_invalid_output_do_not_resolve_a_token() {
        for output in [
            cli_output(false, b"cli-test"),
            cli_output(true, b" \n"),
            cli_output(true, &[0xff]),
            Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        ] {
            assert!(resolve_token_with(|_| None, || output).is_none());
        }
    }

    #[test]
    fn cli_token_command_targets_github_com() {
        let command = gh_token_command();
        assert_eq!(command.get_program(), "gh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["auth", "token", "--hostname", "github.com"]
        );
    }
}
