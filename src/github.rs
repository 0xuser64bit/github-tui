//! Minimal, read-only GitHub REST client built on a blocking [`ureq`] agent.
//!
//! The client is `Clone` (the underlying agent is reference-counted) so it can
//! be cheaply handed to worker threads that perform fetches off the UI thread.
//! Errors are returned as user-facing strings: the TUI's job is to *show* them,
//! not to programmatically branch on them.

use std::collections::HashSet;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::config::RedirectAuthHeaders;
use ureq::http::{HeaderMap, Uri};

use crate::models::{Issue, PullRequest, Repo, SearchedPullRequest, User};

const API_ROOT: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = concat!("github-tui/", env!("CARGO_PKG_VERSION"));
const PER_PAGE: usize = 100;
const MAX_RESPONSE_BYTES: u64 = 10 * 1024 * 1024;

/// One page of results; warnings mean GitHub cannot guarantee completeness.
#[derive(Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
    pub warning: Option<String>,
}

/// A read-only GitHub API client bound to a single token.
#[derive(Clone)]
pub struct Github {
    agent: ureq::Agent,
    token: String,
    cooldown: Arc<Mutex<Option<(Instant, Duration)>>>,
    #[cfg(test)]
    test_api_root: Option<String>,
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
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(20)))
            // GitHub returns 301 for renamed repos; follow redirects and keep
            // the bearer token across the same-host redirect.
            .redirect_auth_headers(RedirectAuthHeaders::SameHost)
            .build()
            .new_agent();
        Self {
            agent,
            token,
            cooldown: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_api_root: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_client(root: &str) -> Self {
        let uri: Uri = root.parse().expect("fixture URL");
        assert_eq!(uri.scheme_str(), Some("http"));
        assert_eq!(uri.host(), Some("127.0.0.1"));
        Self {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(2)))
                .redirect_auth_headers(RedirectAuthHeaders::SameHost)
                .proxy(None)
                .build()
                .new_agent(),
            token: "fixture-token".to_string(),
            cooldown: Arc::new(Mutex::new(None)),
            test_api_root: Some(root.to_string()),
        }
    }

    fn api_root(&self) -> &str {
        #[cfg(test)]
        if let Some(root) = &self.test_api_root {
            return root;
        }
        API_ROOT
    }

    /// Remaining API cooldown, shared by all clones of this client.
    pub fn retry_after(&self) -> Option<Duration> {
        let cooldown = self.cooldown.lock().ok()?;
        let (started, delay) = cooldown.as_ref()?;
        let remaining = delay.saturating_sub(started.elapsed());
        (!remaining.is_zero()).then_some(remaining)
    }

    fn set_retry_after(&self, delay: Duration) {
        if let Ok(mut cooldown) = self.cooldown.lock() {
            let remaining = cooldown
                .as_ref()
                .map(|(started, delay)| delay.saturating_sub(started.elapsed()))
                .unwrap_or_default();
            *cooldown = Some((Instant::now(), delay.max(remaining)));
        }
    }

    /// The authenticated user.
    pub fn current_user(&self) -> Result<User, String> {
        self.get_json("/user", None).map(|(user, _)| user)
    }

    /// Repositories the token can see, most-recently-updated first.
    pub fn list_repos(&self) -> Result<Vec<Repo>, String> {
        collect_pages(|next| self.repos_page(next))
    }

    /// Issues for a repo, with pull requests filtered out (the issues endpoint
    /// returns both).
    pub fn list_issues(&self, owner: &str, repo: &str) -> Result<Vec<Issue>, String> {
        collect_pages(|next| self.issues_page(owner, repo, next))
    }

    /// Pull requests for a repo.
    pub fn list_pulls(&self, owner: &str, repo: &str) -> Result<Vec<PullRequest>, String> {
        collect_pages(|next| self.pulls_page(owner, repo, next))
    }

    /// Pull requests authored by the authenticated user across all repos.
    ///
    /// Returns an error if Search reports incomplete or capped results.
    pub fn my_pulls(&self, login: &str) -> Result<Vec<SearchedPullRequest>, String> {
        collect_pages(|next| self.my_pulls_page(login, next))
    }

    /// Repositories the token can see, most-recently-updated first.
    pub fn repos_page(&self, next: Option<&str>) -> Result<Page<Repo>, String> {
        self.list_page(
            &format!("/user/repos?per_page={PER_PAGE}&sort=updated&affiliation=owner,collaborator,organization_member"),
            next,
        )
    }

    /// One issue page; a page containing only PRs can be empty with a next link.
    pub fn issues_page(
        &self,
        owner: &str,
        repo: &str,
        next: Option<&str>,
    ) -> Result<Page<Issue>, String> {
        let mut page: Page<Issue> = self.list_page(
            &format!("/repos/{owner}/{repo}/issues?state=all&per_page={PER_PAGE}"),
            next,
        )?;
        page.items.retain(|issue| !issue.is_pull_request());
        Ok(page)
    }

    pub fn pulls_page(
        &self,
        owner: &str,
        repo: &str,
        next: Option<&str>,
    ) -> Result<Page<PullRequest>, String> {
        self.list_page(
            &format!("/repos/{owner}/{repo}/pulls?state=all&per_page={PER_PAGE}"),
            next,
        )
    }

    /// All PR states, ordered by update time; Search exposes at most 1,000 hits.
    pub fn my_pulls_page(
        &self,
        login: &str,
        next: Option<&str>,
    ) -> Result<Page<SearchedPullRequest>, String> {
        #[derive(Deserialize)]
        struct SearchEnvelope<T> {
            total_count: u64,
            incomplete_results: bool,
            items: Vec<T>,
        }

        // Omitting a state qualifier includes both open and closed PRs.
        let query = urlencode(&format!("type:pr author:{login}"));
        let path = format!("/search/issues?q={query}&per_page={PER_PAGE}&sort=updated&order=desc");
        let (envelope, next): (SearchEnvelope<SearchedPullRequest>, _) =
            self.get_json(&path, next)?;
        let warning = match (envelope.incomplete_results, envelope.total_count > 1_000) {
            (true, true) => Some(
                "GitHub Search timed out and limits results to 1,000; this list is incomplete."
                    .to_string(),
            ),
            (true, false) => Some(
                "GitHub Search timed out; this list may be incomplete. Refresh to retry."
                    .to_string(),
            ),
            (false, true) => Some(format!(
                "GitHub Search exposes only the first 1,000 of {} matching pull requests.",
                envelope.total_count
            )),
            (false, false) => None,
        };
        Ok(Page {
            items: envelope.items,
            next,
            warning,
        })
    }

    fn list_page<T: DeserializeOwned>(
        &self,
        path: &str,
        next: Option<&str>,
    ) -> Result<Page<T>, String> {
        let (items, next) = self.get_json(path, next)?;
        Ok(Page {
            items,
            next,
            warning: None,
        })
    }

    fn validate_url(&self, url: &str) -> Result<(), String> {
        let uri: Uri = url
            .parse()
            .map_err(|_| "Invalid GitHub pagination URL.".to_string())?;
        let root: Uri = self.api_root().parse().expect("fixed API origin");
        if uri.scheme() != root.scheme() || uri.authority() != root.authority() {
            return Err(
                "Refusing a GitHub pagination URL outside the HTTPS GitHub API.".to_string(),
            );
        }
        Ok(())
    }

    fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        next: Option<&str>,
    ) -> Result<(T, Option<String>), String> {
        let initial = format!("{}{path}", self.api_root());
        let url = next.unwrap_or(&initial);
        self.validate_url(url)?;
        if let Some(delay) = self.retry_after() {
            return Err(rate_limit_message(delay));
        }
        let mut resp = self
            .agent
            .get(url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| format!("Network error: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let headers = resp.headers().clone();
            let server_message = read_json::<serde_json::Value>(resp.body_mut(), 16 * 1024)
                .ok()
                .and_then(|body| {
                    body.get("message")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                });
            let (message, delay) = response_error(status, &headers, server_message.as_deref());
            if let Some(delay) = delay {
                self.set_retry_after(delay);
            }
            return Err(message);
        }
        let next = resp
            .headers()
            .get_all("link")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(parse_next_link);
        if let Some(next) = &next {
            self.validate_url(next)?;
            if next == url {
                return Err("GitHub returned a repeated pagination URL.".to_string());
            }
        }
        let items = read_json(resp.body_mut(), MAX_RESPONSE_BYTES)?;
        Ok((items, next))
    }
}

fn read_json<T: DeserializeOwned>(body: &mut ureq::Body, limit: u64) -> Result<T, String> {
    let mut bytes = Vec::new();
    // ureq's limit applies before decompression; also bound the decoded stream.
    body.with_config()
        .limit(limit)
        .reader()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not parse GitHub response: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "Could not parse GitHub response: decoded body exceeds {limit} bytes."
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not parse GitHub response: {error}"))
}

fn collect_pages<T>(
    mut fetch: impl FnMut(Option<&str>) -> Result<Page<T>, String>,
) -> Result<Vec<T>, String> {
    let mut all = Vec::new();
    let mut next = None;
    let mut seen = HashSet::new();
    loop {
        let page = fetch(next.as_deref())?;
        if let Some(warning) = page.warning {
            return Err(warning);
        }
        all.extend(page.items);
        match page.next {
            Some(url) if seen.insert(url.clone()) => next = Some(url),
            Some(_) => return Err("GitHub returned a repeated pagination URL.".to_string()),
            None => return Ok(all),
        }
    }
}

fn response_error(
    status: u16,
    headers: &HeaderMap,
    server_message: Option<&str>,
) -> (String, Option<Duration>) {
    let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
    let remaining = header("x-ratelimit-remaining");
    if status == 429
        || (status == 403
            && (remaining == Some("0")
                || header("retry-after").is_some()
                || server_message
                    .is_some_and(|message| message.to_ascii_lowercase().contains("rate limit"))))
    {
        let retry = header("retry-after").and_then(|value| value.parse::<u64>().ok());
        let reset = (remaining == Some("0"))
            .then(|| header("x-ratelimit-reset").and_then(|value| value.parse::<u64>().ok()))
            .flatten();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let delay = retry.or_else(|| reset.map(|reset| reset.saturating_sub(now)));
        let delay = Duration::from_secs(delay.unwrap_or(60));
        return (rate_limit_message(delay), Some(delay));
    }
    (describe_status(status, remaining), None)
}

fn rate_limit_message(delay: Duration) -> String {
    let seconds = delay
        .as_secs()
        .saturating_add(u64::from(delay.subsec_nanos() > 0));
    format!("GitHub API rate limit exceeded. Retry after {seconds} seconds.")
}

/// Encode a complete query value so caller input cannot introduce parameters.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Extract the URL for `rel="next"` from a GitHub `Link` header value.
///
/// GitHub's format: `<https://…?page=2>; rel="next", <https://…?page=5>; rel="last"`
fn parse_next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|segment| {
        let (url, parameters) = segment.trim().strip_prefix('<')?.split_once('>')?;
        parameters
            .split(';')
            .any(|parameter| {
                parameter
                    .trim()
                    .strip_prefix("rel=")
                    .is_some_and(|relations| {
                        relations
                            .trim_matches('"')
                            .split_whitespace()
                            .any(|rel| rel == "next")
                    })
            })
            .then(|| url.to_string())
    })
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
        422 => "GitHub could not process this query or the token cannot access its repositories."
            .to_string(),
        429 => {
            "GitHub API rate limit exceeded. Wait at least a minute before retrying.".to_string()
        }
        s @ 500..=599 => format!("GitHub is having problems (HTTP {s}). Try again shortly."),
        s => format!("Unexpected GitHub API response (HTTP {s})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    fn reply(status: u16, headers: &str, body: impl AsRef<[u8]>) -> Vec<u8> {
        let body = body.as_ref();
        let mut response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn fixture(responses: Vec<Vec<u8>>) -> (Github, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let client = Github::test_client(&root);
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "fixture request timed out"
                            );
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0, "incomplete fixture request");
                    request.extend_from_slice(&buffer[..count]);
                }
                requests.push(String::from_utf8(request).unwrap());
                let split = response
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .unwrap()
                    + 4;
                let headers = String::from_utf8_lossy(&response[..split]).replace("$ROOT", &root);
                stream.write_all(headers.as_bytes()).unwrap();
                stream.write_all(&response[split..]).unwrap();
            }
            requests
        });
        (client, worker)
    }

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
        assert_eq!(urlencode("a&b=c+#/%é"), "a%26b%3Dc%2B%23%2F%25%C3%A9");
    }

    #[test]
    fn pages_follow_links_and_send_github_headers() {
        let (client, worker) = fixture(vec![
            reply(
                200,
                "Link: <$ROOT/user/repos?page=2>; rel=\"next\"\r\n",
                r#"[{"name":"first","full_name":"me/first"}]"#,
            ),
            reply(200, "", r#"[{"name":"second","full_name":"me/second"}]"#),
        ]);
        let repos = client.list_repos().unwrap();
        assert_eq!(
            repos
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        let requests = worker.join().unwrap();
        assert!(requests[0].starts_with("GET /user/repos?per_page=100"));
        assert!(requests[1].starts_with("GET /user/repos?page=2 "));
        for request in requests {
            let request = request.to_ascii_lowercase();
            assert!(request.contains("authorization: bearer fixture-token\r\n"));
            assert!(request.contains("accept: application/vnd.github+json\r\n"));
            assert!(request.contains("x-github-api-version: 2022-11-28\r\n"));
            assert!(request.contains("user-agent: github-tui/"));
        }
    }

    #[test]
    fn pr_only_issue_page_keeps_its_continuation() {
        let (client, worker) = fixture(vec![
            reply(
                200,
                "Link: <$ROOT/repos/me/repo/issues?page=2>; rel=\"next\"\r\n",
                r#"[{"number":1,"title":"PR","state":"open","pull_request":{}}]"#,
            ),
            reply(200, "", r#"[{"number":2,"title":"Issue","state":"open"}]"#),
        ]);
        let first = client.issues_page("me", "repo", None).unwrap();
        assert!(first.items.is_empty());
        let second = client
            .issues_page("me", "repo", first.next.as_deref())
            .unwrap();
        assert_eq!(second.items[0].number, 2);
        assert!(second.next.is_none());
        worker.join().unwrap();
    }

    #[test]
    fn search_queries_all_states_with_explicit_sort_and_warns() {
        for (count, incomplete, expected) in [
            (1, false, None),
            (1, true, Some("timed out")),
            (1_001, false, Some("1,000")),
            (1_001, true, Some("incomplete")),
        ] {
            let body = format!(
                r#"{{"total_count":{count},"incomplete_results":{incomplete},"items":[{{"number":1,"title":"Closed PR","state":"closed"}}]}}"#
            );
            let (client, worker) = fixture(vec![reply(200, "", &body)]);
            let page = client.my_pulls_page("octocat", None).unwrap();
            assert_eq!(page.items[0].state, "closed");
            match expected {
                Some(expected) => assert!(page.warning.unwrap().contains(expected)),
                None => assert!(page.warning.is_none()),
            }
            let requests = worker.join().unwrap();
            assert!(requests[0].starts_with("GET /search/issues?q=type%3Apr%20author%3Aoctocat&per_page=100&sort=updated&order=desc "));
            assert!(!requests[0].contains("state"));
        }
    }

    #[test]
    fn complete_search_api_does_not_claim_partial_success() {
        let (client, worker) = fixture(vec![reply(
            200,
            "",
            r#"{"total_count":3,"incomplete_results":true,"items":[]}"#,
        )]);
        assert!(
            client
                .my_pulls("octocat")
                .unwrap_err()
                .contains("incomplete")
        );
        worker.join().unwrap();
    }

    #[test]
    fn later_page_failure_does_not_return_a_complete_list() {
        let (client, worker) = fixture(vec![
            reply(
                200,
                "Link: <$ROOT/user/repos?page=2>; rel=\"next\"\r\n",
                r#"[{"name":"first","full_name":"me/first"}]"#,
            ),
            reply(503, "", "{}"),
        ]);
        assert!(client.list_repos().unwrap_err().contains("HTTP 503"));
        assert_eq!(worker.join().unwrap().len(), 2);
    }

    #[test]
    fn unsafe_continuations_are_rejected_before_requesting() {
        let client = Github::new("fixture-token".to_string());
        assert!(
            client
                .validate_url("https://api.github.com/user/repos?page=2")
                .is_ok()
        );
        for url in [
            "http://api.github.com/user/repos?page=2",
            "https://example.com/",
            "https://api.github.com.example.com/",
            "https://api.github.com@evil.example/",
            "https://api.github.com:444/",
            "/user/repos?page=2",
            ">bad<",
        ] {
            assert!(client.repos_page(Some(url)).is_err(), "accepted {url}");
        }
        let (client, worker) = fixture(vec![reply(
            200,
            "Link: <https://example.com/page=2>; rel=\"next\"\r\n",
            "[]",
        )]);
        assert!(client.repos_page(None).unwrap_err().contains("Refusing"));
        assert_eq!(worker.join().unwrap().len(), 1);
    }

    #[test]
    fn redirect_to_another_host_does_not_forward_authorization() {
        let (destination, destination_worker) =
            fixture(vec![reply(200, "", r#"{"login":"octocat"}"#)]);
        let target = destination.api_root().replace("127.0.0.1", "localhost");
        let (client, worker) = fixture(vec![reply(
            301,
            &format!("Location: {target}/user/renamed\r\n"),
            "",
        )]);
        assert_eq!(client.current_user().unwrap().login, "octocat");
        let requests = worker.join().unwrap();
        assert!(requests[0].to_ascii_lowercase().contains("authorization:"));
        let redirected = destination_worker.join().unwrap();
        assert!(
            !redirected[0]
                .to_ascii_lowercase()
                .contains("authorization:")
        );
    }

    #[test]
    fn response_errors_explain_rate_limits_and_retry_delays() {
        for (status, headers, expected) in [
            (401, "", "token"),
            (
                403,
                "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 0\r\n",
                "Retry after 0 seconds",
            ),
            (403, "Retry-After: 12\r\n", "Retry after 12 seconds"),
            (429, "Retry-After: 30\r\n", "Retry after 30 seconds"),
            (429, "", "rate limit"),
            (403, "X-RateLimit-Remaining: 10\r\n", "scopes"),
            (422, "", "query"),
        ] {
            let (client, worker) = fixture(vec![reply(status, headers, "{}")]);
            assert!(client.current_user().unwrap_err().contains(expected));
            worker.join().unwrap();
        }
    }

    #[test]
    fn malformed_or_oversized_json_returns_an_error() {
        for body in [
            "not json".to_string(),
            format!(
                "{{\"login\":\"{}\"}}",
                "x".repeat(MAX_RESPONSE_BYTES as usize)
            ),
        ] {
            let (client, worker) = fixture(vec![reply(200, "", &body)]);
            assert!(
                client
                    .current_user()
                    .unwrap_err()
                    .contains("Could not parse")
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn compressed_json_is_limited_after_decompression() {
        // A 35-byte gzip payload containing a 268-byte JSON document.
        let gzip = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\xab\x56\xca\xc9\x4f\xcf\xcc\x53\xb2\x52\xaa\x18\xe1\x40\xa9\x16\x00\x62\xb5\x53\x2c\x0c\x01\x00\x00";
        let (client, worker) = fixture(vec![reply(200, "Content-Encoding: gzip\r\n", gzip)]);
        let mut response = client
            .agent
            .get(format!("{}/user", client.api_root()))
            .call()
            .unwrap();
        let error = read_json::<User>(response.body_mut(), 128).unwrap_err();
        assert!(error.contains("decoded body exceeds 128 bytes"));
        worker.join().unwrap();
    }

    #[test]
    fn repeated_pagination_links_stop_collection() {
        let (client, worker) = fixture(vec![
            reply(
                200,
                "Link: <$ROOT/user/repos?page=2>; rel=\"next\"\r\n",
                "[]",
            ),
            reply(
                200,
                "Link: <$ROOT/user/repos?page=2>; rel=\"next\"\r\n",
                "[]",
            ),
        ]);
        assert!(client.list_repos().unwrap_err().contains("repeated"));
        worker.join().unwrap();
    }

    #[test]
    fn rate_limit_cooldown_is_shared_and_expires() {
        let (client, worker) = fixture(vec![
            reply(429, "Retry-After: 60\r\n", "{}"),
            reply(200, "", r#"{"login":"octocat"}"#),
        ]);
        assert!(client.current_user().unwrap_err().contains("60 seconds"));
        let clone = client.clone();
        assert!(clone.retry_after().is_some());
        assert!(clone.current_user().unwrap_err().contains("rate limit"));
        *client.cooldown.lock().unwrap() = Some((Instant::now(), Duration::ZERO));
        assert!(clone.retry_after().is_none());
        assert_eq!(clone.current_user().unwrap().login, "octocat");
        assert_eq!(worker.join().unwrap().len(), 2);

        *client.cooldown.lock().unwrap() = Some((Instant::now(), Duration::MAX));
        assert!(client.retry_after().is_some());
        assert!(rate_limit_message(Duration::MAX).contains("Retry after"));
    }

    #[test]
    fn a_later_rate_limit_response_cannot_shorten_the_cooldown() {
        let client = Github::new("fixture-token".to_string());
        client.set_retry_after(Duration::from_secs(600));
        client.clone().set_retry_after(Duration::from_secs(60));
        assert!(client.retry_after().unwrap() > Duration::from_secs(590));
    }

    #[test]
    fn secondary_rate_limit_without_retry_header_uses_a_cooldown() {
        let (client, worker) = fixture(vec![reply(
            403,
            "X-RateLimit-Remaining: 10\r\nX-RateLimit-Reset: 9999999999\r\n",
            r#"{"message":"You have exceeded a secondary rate limit."}"#,
        )]);
        assert!(
            client
                .current_user()
                .unwrap_err()
                .contains("Retry after 60 seconds")
        );
        assert!(client.retry_after().is_some());
        worker.join().unwrap();
    }
}
