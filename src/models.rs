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
    /// Present (non-null) only for merged pull requests.
    #[serde(default)]
    pub merged_at: Option<String>,
}

impl PullRequest {
    /// Author login, or `"unknown"` when GitHub omits the user.
    pub fn author(&self) -> &str {
        self.user.as_ref().map_or("unknown", |u| u.login.as_str())
    }

    /// Whether this pull request has been merged.
    pub fn is_merged(&self) -> bool {
        self.merged_at.is_some()
    }

    /// Normalized state: `"open"`, `"merged"`, or `"closed"`.
    pub fn effective_state(&self) -> &str {
        if self.state == "closed" && self.is_merged() {
            "merged"
        } else {
            &self.state
        }
    }
}

/// A pull request result from the GitHub Search API.
///
/// This is intentionally a separate type from [`PullRequest`]: the search
/// endpoint wraps pull requests in an issue-shaped payload and identifies the
/// repository via `repository_url` rather than an owner/name pair.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchedPullRequest {
    pub number: u64,
    pub title: String,
    /// `"open"` or `"closed"`.
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub repository_url: Option<String>,
    #[serde(default)]
    pub pull_request: Option<PullRequestMetadata>,
}

impl SearchedPullRequest {
    /// Author login, or `"unknown"` when GitHub omits the user.
    pub fn author(&self) -> &str {
        self.user.as_ref().map_or("unknown", |u| u.login.as_str())
    }

    /// Whether this pull request has been merged.
    pub fn is_merged(&self) -> bool {
        self.pull_request
            .as_ref()
            .is_some_and(|pr| pr.merged_at.is_some())
    }

    /// Normalized state: `"open"`, `"merged"`, or `"closed"`.
    pub fn effective_state(&self) -> &str {
        if self.state == "closed" && self.is_merged() {
            "merged"
        } else {
            &self.state
        }
    }

    /// The repository name (`owner/name`) this PR belongs to, or `"unknown"`.
    pub fn repo(&self) -> String {
        let Some(url) = self.repository_url.as_deref() else {
            return "unknown".to_string();
        };
        let mut parts = url.trim_end_matches('/').rsplit('/');
        let name = parts.next().unwrap_or("unknown");
        let owner = parts.next().unwrap_or("unknown");
        format!("{owner}/{name}")
    }
}

/// The `pull_request` object embedded in search API results.
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestMetadata {
    #[serde(default)]
    pub merged_at: Option<String>,
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

    #[test]
    fn pull_request_effective_state_distinguishes_merged() {
        let open: PullRequest =
            serde_json::from_str(r#"{"number":1,"title":"t","state":"open","merged_at":null}"#)
                .unwrap();
        assert_eq!(open.effective_state(), "open");
        assert!(!open.is_merged());

        let merged: PullRequest = serde_json::from_str(
            r#"{"number":2,"title":"t","state":"closed","merged_at":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(merged.effective_state(), "merged");
        assert!(merged.is_merged());

        let closed: PullRequest =
            serde_json::from_str(r#"{"number":3,"title":"t","state":"closed","merged_at":null}"#)
                .unwrap();
        assert_eq!(closed.effective_state(), "closed");
        assert!(!closed.is_merged());
    }

    #[test]
    fn searched_pull_request_parses_repo_from_url() {
        let pr: SearchedPullRequest = serde_json::from_str(
            r#"{
                "number": 7,
                "title": "add tests",
                "state": "closed",
                "repository_url": "https://api.github.com/repos/octocat/Hello-World",
                "pull_request": {"merged_at": "2024-01-01T00:00:00Z"}
            }"#,
        )
        .unwrap();
        assert_eq!(pr.repo(), "octocat/Hello-World");
        assert_eq!(pr.effective_state(), "merged");
    }
}
