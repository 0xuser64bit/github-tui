//! Minimal, read-only GitHub REST client built on a blocking [`ureq`] agent.
//!
//! The client is `Clone` (the underlying agent is reference-counted) so it can
//! be cheaply handed to worker threads that perform fetches off the UI thread.
//! Errors are returned as user-facing strings: the TUI's job is to *show* them,
//! not to programmatically branch on them.

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::config::RedirectAuthHeaders;

use crate::models::{Issue, PullRequest, Repo, SearchedPullRequest, User};

const API_ROOT: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = concat!("github-tui/", env!("CARGO_PKG_VERSION"));
/// GitHub caps per_page at 100; we paginate via `Link` headers to fetch all.
const PER_PAGE: usize = 100;

/// A read-only GitHub API client bound to a single token.
#[derive(Clone)]
pub struct Github {
    agent: ureq::Agent,
    token: String,
}

// Manual Debug so the token can never leak through `{:?}` / panic reports.
impl std::fmt::Debug for Github {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Github")
            .field("token", &"<redacted>")
            .finish()
    }
}

impl Github {
    /// Build a client for the given personal-access / OAuth token.
    pub fn new(token: String) -> Self {
        let agent = ureq::Agent::config_builder()
            // Surface 4xx/5xx as normal responses so we can read status and
            // rate-limit headers instead of an opaque transport error.
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            // GitHub returns 301 for renamed repos; follow redirects and keep
            // the bearer token across the same-host redirect.
            .redirect_auth_headers(RedirectAuthHeaders::SameHost)
            .build()
            .new_agent();
        Self { agent, token }
    }

    /// The authenticated user.
    pub fn current_user(&self) -> Result<User, String> {
        self.get_json("/user")
    }

    /// Repositories the token can see, most-recently-updated first.
    pub fn list_repos(&self) -> Result<Vec<Repo>, String> {
        self.get_json_paginated(&format!(
            "/user/repos?per_page={PER_PAGE}&sort=updated&affiliation=owner,collaborator,organization_member"
        ))
    }

    /// Issues for a repo, with pull requests filtered out (the issues endpoint
    /// returns both).
    pub fn list_issues(&self, owner: &str, repo: &str) -> Result<Vec<Issue>, String> {
        let issues: Vec<Issue> = self.get_json_paginated(&format!(
            "/repos/{owner}/{repo}/issues?state=all&per_page={PER_PAGE}"
        ))?;
        Ok(issues
            .into_iter()
            .filter(|i| !i.is_pull_request())
            .collect())
    }

    /// Pull requests for a repo.
    pub fn list_pulls(&self, owner: &str, repo: &str) -> Result<Vec<PullRequest>, String> {
        self.get_json_paginated(&format!(
            "/repos/{owner}/{repo}/pulls?state=all&per_page={PER_PAGE}"
        ))
    }

    /// Pull requests authored by the authenticated user across all repos.
    ///
    /// GitHub's Search API does not allow `state:all` (it returns zero results),
    /// so we fetch `open` and `closed` separately and concatenate them. Closed
    /// PRs are returned most-recently-updated first, which keeps the combined
    /// list useful without a second client-side sort.
    pub fn my_pulls(&self, login: &str) -> Result<Vec<SearchedPullRequest>, String> {
        let mut all = Vec::new();
        for state in ["open", "closed"] {
            let query = format!("type:pr author:{login} state:{state}");
            let encoded = urlencode(&query);
            let path = format!("/search/issues?q={encoded}&per_page={PER_PAGE}");
            all.extend(self.get_json_paginated_search::<SearchedPullRequest>(&path)?);
        }
        Ok(all)
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let url = format!("{API_ROOT}{path}");
        let mut resp = self
            .agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| format!("Network error: {e}"))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let remaining = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok());
            return Err(describe_status(status, remaining));
        }

        resp.body_mut()
            .read_json::<T>()
            .map_err(|e| format!("Could not parse GitHub response: {e}"))
    }

    /// Fetch a paginated list endpoint, following `Link: rel="next"` headers
    /// until every page has been collected.
    fn get_json_paginated<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, String> {
        let mut all: Vec<T> = Vec::new();
        let mut url = format!("{API_ROOT}{path}");

        loop {
            let mut resp = self
                .agent
                .get(&url)
                .header("Authorization", &format!("Bearer {}", self.token))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", API_VERSION)
                .header("User-Agent", USER_AGENT)
                .call()
                .map_err(|e| format!("Network error: {e}"))?;

            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let remaining = resp
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|v| v.to_str().ok());
                return Err(describe_status(status, remaining));
            }

            let next = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_next_link);

            let page: Vec<T> = resp
                .body_mut()
                .read_json()
                .map_err(|e| format!("Could not parse GitHub response: {e}"))?;

            all.extend(page);

            match next {
                Some(next_url) => url = next_url,
                None => break,
            }
        }

        Ok(all)
    }

    /// Fetch a paginated Search API endpoint, which wraps results in a
    /// `{ total_count, incomplete_results, items }` envelope instead of a bare
    /// array.
    fn get_json_paginated_search<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, String> {
        #[derive(Deserialize)]
        struct SearchEnvelope<T> {
            items: Vec<T>,
        }

        let mut all: Vec<T> = Vec::new();
        let mut url = format!("{API_ROOT}{path}");

        loop {
            let mut resp = self
                .agent
                .get(&url)
                .header("Authorization", &format!("Bearer {}", self.token))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", API_VERSION)
                .header("User-Agent", USER_AGENT)
                .call()
                .map_err(|e| format!("Network error: {e}"))?;

            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let remaining = resp
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|v| v.to_str().ok());
                return Err(describe_status(status, remaining));
            }

            let next = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_next_link);

            let envelope: SearchEnvelope<T> = resp
                .body_mut()
                .read_json()
                .map_err(|e| format!("Could not parse GitHub response: {e}"))?;

            all.extend(envelope.items);

            match next {
                Some(next_url) => url = next_url,
                None => break,
            }
        }

        Ok(all)
    }
}

/// Percent-encode the characters GitHub's Search API treats specially.
///
/// A full form-encoder would be overkill for our one query shape; we only need
/// spaces and colons escaped so `type:pr author:octocat state:open` survives
/// the trip through a query string.
fn urlencode(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace(':', "%3A")
}

/// Extract the URL for `rel="next"` from a GitHub `Link` header value.
///
/// GitHub's format: `<https://…?page=2>; rel="next", <https://…?page=5>; rel="last"`
fn parse_next_link(header: &str) -> Option<String> {
    for segment in header.split(',') {
        let segment = segment.trim();
        if segment.contains("rel=\"next\"") {
            // Extract the URL between < and >
            let start = segment.find('<')? + 1;
            let end = segment.find('>')?;
            return Some(segment[start..end].to_string());
        }
    }
    None
}

/// Translate an HTTP status into a message a human can act on.
///
/// Pure and side-effect free so it can be unit tested without a network.
pub fn describe_status(status: u16, rate_limit_remaining: Option<&str>) -> String {
    match status {
        401 => "Authentication failed — your token is invalid or expired.".to_string(),
        403 if rate_limit_remaining == Some("0") => {
            "GitHub API rate limit exceeded. Try again in a little while.".to_string()
        }
        403 => "Access forbidden — your token may be missing the required scopes.".to_string(),
        404 => "Not found — the repository may be private or renamed.".to_string(),
        s @ 500..=599 => format!("GitHub is having problems (HTTP {s}). Try again shortly."),
        s => format!("Unexpected GitHub API response (HTTP {s})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_messages_are_actionable() {
        assert!(describe_status(401, None).contains("token"));
        assert!(describe_status(403, Some("0")).contains("rate limit"));
        assert!(describe_status(403, Some("42")).contains("scopes"));
        assert!(describe_status(404, None).contains("Not found"));
        assert!(describe_status(503, None).contains("HTTP 503"));
        assert!(describe_status(418, None).contains("HTTP 418"));
    }

    #[test]
    fn debug_redacts_token() {
        let client = Github::new("super-secret-token".to_string());
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("super-secret-token"));
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn parse_next_link_extracts_next_url() {
        let header = r#"<https://api.github.com/user/repos?page=2&per_page=100>; rel="next", <https://api.github.com/user/repos?page=5&per_page=100>; rel="last""#;
        assert_eq!(
            parse_next_link(header),
            Some("https://api.github.com/user/repos?page=2&per_page=100".to_string())
        );
    }

    #[test]
    fn parse_next_link_returns_none_on_last_page() {
        let header = r#"<https://api.github.com/user/repos?page=1&per_page=100>; rel="prev", <https://api.github.com/user/repos?page=5&per_page=100>; rel="last""#;
        assert_eq!(parse_next_link(header), None);
    }

    #[test]
    fn parse_next_link_returns_none_on_empty() {
        assert_eq!(parse_next_link(""), None);
    }

    #[test]
    fn urlencode_escapes_search_query() {
        assert_eq!(
            urlencode("type:pr author:octocat state:open"),
            "type%3Apr%20author%3Aoctocat%20state%3Aopen"
        );
    }
}
