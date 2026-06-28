//! Token resolution.
//!
//! Resolution order, first match wins:
//!   1. `GITHUB_TOKEN` environment variable
//!   2. `GH_TOKEN` environment variable
//!   3. the GitHub CLI (`gh auth token`), if installed and logged in
//!
//! The token is never logged or rendered anywhere in the UI.

use std::process::Command;

/// Resolve a GitHub token, or `None` if the user hasn't provided one.
pub fn resolve_token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Some(token) = std::env::var(var).ok().and_then(non_empty) {
            return Some(token);
        }
    }
    token_from_gh_cli()
}

/// Ask the `gh` CLI for its stored token. Silently yields `None` when `gh` is
/// absent, not authenticated, or returns nothing.
fn token_from_gh_cli() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    non_empty(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Trim surrounding whitespace and drop the value if nothing remains.
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::non_empty;

    #[test]
    fn non_empty_trims_and_filters() {
        assert_eq!(non_empty("  abc \n".to_string()), Some("abc".to_string()));
        assert_eq!(non_empty("   ".to_string()), None);
        assert_eq!(non_empty(String::new()), None);
    }
}
