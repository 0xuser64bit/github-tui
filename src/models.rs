//! Typed GitHub REST API resources.
//!
//! Only the fields the TUI actually renders are modeled. Every optional field
//! uses `#[serde(default)]` so a missing or null value never aborts a fetch —
//! the GitHub payload is large and we deliberately ignore most of it.

use serde::Deserialize;

/// A GitHub account (only the parts we display).
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
}

/// A repository, as returned by `GET /user/repos`.
#[derive(Debug, Clone, Deserialize)]
pub struct Repo {
    pub name: String,
    /// `owner/name`, used both for display and to address sub-resources.
    pub full_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub stargazers_count: u64,
    #[serde(default)]
    pub open_issues_count: u64,
    #[serde(default)]
    pub fork: bool,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub archived: bool,
}

impl Repo {
    /// Split `full_name` into `(owner, repo)` for addressing sub-resources.
    ///
    /// Falls back to `("", name)` if `full_name` is malformed, so callers never
    /// panic on unexpected payloads.
    pub fn owner_repo(&self) -> (&str, &str) {
        match self.full_name.split_once('/') {
            Some((owner, repo)) => (owner, repo),
            None => ("", self.name.as_str()),
        }
    }
}

/// An issue, as returned by `GET /repos/{owner}/{repo}/issues`.
///
/// Note: that endpoint also returns pull requests. Items where
/// [`Issue::is_pull_request`] is true must be filtered out for an issues view.
#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    /// `"open"` or `"closed"`.
    pub state: String,
    #[serde(default)]
    pub comments: u64,
    #[serde(default)]
    pub user: Option<User>,
    /// Present only when this "issue" is actually a pull request.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

impl Issue {
    /// True when GitHub returned a pull request through the issues endpoint.
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }

    /// Author login, or `"unknown"` when GitHub omits the user (e.g. ghost).
    pub fn author(&self) -> &str {
        self.user.as_ref().map_or("unknown", |u| u.login.as_str())
    }
}

/// A pull request, as returned by `GET /repos/{owner}/{repo}/pulls`.
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    /// `"open"` or `"closed"`.
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub user: Option<User>,
}

impl PullRequest {
    /// Author login, or `"unknown"` when GitHub omits the user.
    pub fn author(&self) -> &str {
        self.user.as_ref().map_or("unknown", |u| u.login.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_splits_owner_and_name() {
        let repo: Repo =
            serde_json::from_str(r#"{"name":"github-tui","full_name":"user-64bit/github-tui"}"#)
                .unwrap();
        assert_eq!(repo.owner_repo(), ("user-64bit", "github-tui"));
        // Optional fields default rather than failing.
        assert_eq!(repo.stargazers_count, 0);
        assert!(repo.description.is_none());
    }

    #[test]
    fn repo_owner_repo_tolerates_missing_slash() {
        let repo: Repo = serde_json::from_str(r#"{"name":"solo","full_name":"solo"}"#).unwrap();
        assert_eq!(repo.owner_repo(), ("", "solo"));
    }

    #[test]
    fn issue_detects_pull_requests() {
        let plain: Issue =
            serde_json::from_str(r#"{"number":1,"title":"bug","state":"open"}"#).unwrap();
        assert!(!plain.is_pull_request());
        assert_eq!(plain.author(), "unknown");

        let pr: Issue = serde_json::from_str(
            r#"{"number":2,"title":"feat","state":"open","user":{"login":"octocat"},
                "pull_request":{"url":"https://api.github.com/..."}}"#,
        )
        .unwrap();
        assert!(pr.is_pull_request());
        assert_eq!(pr.author(), "octocat");
    }
}
