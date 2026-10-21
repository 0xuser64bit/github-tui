//! Live smoke test against the real GitHub API.
//!
//! Ignored by default (CI has no token); run locally with:
//!   cargo test --test live -- --ignored --nocapture
//!
//! Verifies the full data path: token resolution → auth → HTTP → JSON parsing →
//! pull-request filtering for the issues endpoint, and global pull-request search.

use github_tui::auth;
use github_tui::github::Github;

#[test]
#[ignore = "hits the live GitHub API; requires a resolvable token"]
fn live_data_path() {
    let token = auth::resolve_token().expect(
        "manual live smoke test requires GITHUB_TOKEN, GH_TOKEN, or gh auth login --hostname github.com",
    );

    let client = Github::new(token);

    let user = client.current_user().expect("fetch current user");
    println!("authenticated as @{}", user.login);

    let repos = client.list_repos().expect("list repositories");
    println!("fetched {} repositories", repos.len());

    let my_pulls = client
        .my_pulls(&user.login)
        .expect("fetch global my pull requests; incomplete search results must fail this test");
    println!("fetched {} global pull requests", my_pulls.len());

    if let Some(repo) = repos.first() {
        let (owner, name) = repo.owner_repo();
        let issues = client.list_issues(owner, name).expect("list issues");
        let pulls = client.list_pulls(owner, name).expect("list pulls");
        println!(
            "{owner}/{name}: {} issues, {} pull requests",
            issues.len(),
            pulls.len()
        );
        // The issues endpoint also returns PRs; our client must filter them.
        assert!(issues.iter().all(|issue| !issue.is_pull_request()));
    } else {
        eprintln!(
            "no repositories available; repository issue/pull endpoint checks were not exercised"
        );
    }
}
