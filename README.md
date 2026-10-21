# github-tui

A fast, read-only terminal UI for browsing your GitHub **repositories**,
**issues**, and **pull requests** — without leaving the keyboard. Built with
[Ratatui].

> Status: early MVP. Read-only by design (see [Scope](#scope)).

## Features

- Browse repositories you own, collaborate on, or share via an org
- Drill into a repository's **Issues** and **Pull Requests** in tabs
- View **your pull requests** across accessible repositories (`m`), or open PRs
  requesting your review (`v`)
- Load results one page at a time (`n`); keep loaded rows if a later request fails
- Search loaded items by title, number, or author (`/`), and cycle state filters (`s`)
- Preview issue and PR descriptions without leaving the terminal (`p`)
- Open the current repo / issue / PR in your browser (`o` or `Enter`)
- Non-blocking UI: network requests never freeze rendering or input
- Loading spinners, empty states, and actionable error messages
  (auth failures, rate limits, network errors)
- Vim-style and arrow-key navigation, with an in-app help overlay (`?`)

## Install

Requires Rust 1.88+ (edition 2024).

```sh
git clone https://github.com/user-64bit/github-tui
cd github-tui
cargo run --release
```

## Authentication

github-tui never stores a token; it resolves one at startup, first match wins:

1. `GITHUB_TOKEN` environment variable
2. `GH_TOKEN` environment variable
3. the [GitHub CLI] (`gh auth token --hostname github.com`), if you're logged in

Only **github.com** is supported. Enterprise credentials are not selected by the
CLI fallback, even when `GH_HOST` points at an Enterprise server.

The simplest path is the GitHub CLI:

```sh
gh auth login --hostname github.com
```

Or use a [personal access token] with read access to the repositories you care
about. Prefer a fine-grained token limited to those repositories with read-only
**Issues** and **Pull requests** permissions. A classic token's `repo` scope
also works, but grants broad write permissions even though this app only reads:

```sh
export GITHUB_TOKEN=ghp_xxx
cargo run --release
```

If no token is found, the app starts and explains how to provide one rather
than failing.

## Keybindings

| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | Move selection |
| `g` / `Home`, `G` / `End` | Jump to first / last |
| `Enter` / `l` / `→` | Open repository (or issue/PR in browser) |
| `Esc` / `h` / `←` | Back to repository list |
| `Tab` | Switch Issues / Pull Requests |
| `m` | View your pull requests |
| `v` | View open pull requests requesting your review |
| `/` | Search loaded rows; `Esc` clears, `Enter` opens the highlighted match |
| `s` | Cycle all / open / closed / merged (issues omit merged) |
| `n` | Load the next page of the current list |
| `p` | Preview the selected issue or PR; `Esc` closes |
| `o` | Open current selection in browser |
| `r` | Refresh the current view |
| `?` | Toggle help |
| `q` / `Ctrl-C` | Quit |

Previews show plain-text descriptions. Use `j`/`k` or arrows to scroll, `g` to
return to the top, `r` to retry loading, and `o` to open the previewed item in
your browser. Browser shortcuts and `Enter` still work directly from lists.

Search and state filters apply to **loaded rows**, not unseen pages. Load more
pages with `n` after leaving search. State filters reset when switching screens
or repository tabs. Closed and merged PRs are separate filters. My PRs and the
review queue retain loaded results while you visit repositories; switching
between those two queues starts a new query. Refresh replaces the current
list only after a successful response and retains useful rows on failure.

## Scope

This is intentionally **read-only**. There is no creating, editing, commenting,
or merging — those add token-scope, validation, and confirmation-flow surface
that the first version deliberately avoids. The browser integration (`o`) is the
escape hatch for actions that need the full GitHub UI.

The personal PR queues use GitHub's Search API, normally limited to 30
authenticated requests per minute. Opening a queue costs one request for its
first page, and each `n` costs another request. Results are ordered by recent
updates. Repeated refreshes are coalesced while loading, and API rate-limit
cooldowns are shared across the app's requests.

GitHub Search exposes at most 1,000 results per query and searches at most
4,000 matching repositories. The app shows warnings for reported timeouts or
result caps; it does not promise exhaustive history. Pages reflect live data,
so changes while paging can move results. Repository issue/PR lists can be
continued through their available history. See [GitHub Search limits].

## Architecture

```
main → auth (resolve token) → App.run(terminal, events)
                                     │
        ┌────────────────────────────┴───────────────────────────┐
        │                 mpsc::channel<Event>                     │
   input thread        tick thread          worker threads (per fetch)
 (terminal events)   (spinner ticks)     (github::Github → REST → JSON)
```

- `models` — typed GitHub resources (`serde`); only rendered fields are modeled
- `github` — a cloneable, blocking REST client (`ureq` + rustls; no OpenSSL)
- `auth` — token resolution
- `event` — the single channel multiplexing input, ticks, and fetch results
- `app` — state machine, key handling, and fetch dispatch
- `ui` — pure rendering of `App` state

Network requests run off the UI thread. Request generations reject responses
from superseded views; repeated requests for a loading resource are coalesced.
Browser launchers are reaped by worker threads. Idle animation ticks do not
redraw loaded lists, and input failures return through normal terminal cleanup.

## Development

```sh
cargo fmt --all
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo test --locked --all-features --all-targets  # unit tests + local HTTP fixtures; no GitHub requests
cargo +1.88.0 test --locked --all-features --all-targets  # minimum compiler
cargo test --test live -- --ignored --nocapture   # live API smoke test
```

The library/binary split means logic lives in a testable library crate
(`github_tui`) with a thin binary on top.

The ignored live smoke test requires credentials and fails clearly when they
are absent. It checks the account, repositories, issue/PR lists, and authored
PR search; capped or incomplete Search results fail its completeness check.
Regular tests use synthetic credentials and local fixtures. CI checks Rust
1.88, current stable on Linux/macOS/Windows, formatting, Clippy, and documentation.

## License

MIT — see [LICENSE](./LICENSE).

[Ratatui]: https://ratatui.rs
[GitHub CLI]: https://cli.github.com
[personal access token]: https://github.com/settings/tokens
[GitHub Search limits]: https://docs.github.com/en/rest/search/search#about-search
