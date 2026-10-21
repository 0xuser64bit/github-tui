# github-tui

A fast, read-only terminal UI for browsing your GitHub **repositories**,
**issues**, and **pull requests** — without leaving the keyboard. Built with
[Ratatui].

> Status: early MVP. Read-only by design (see [Scope](#scope)).

## Features

- Browse repositories you own, collaborate on, or share via an org
- Drill into a repository's **Issues** and **Pull Requests** in tabs
- View all **your pull requests** across every repository (`m`)
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
3. the [GitHub CLI] (`gh auth token`), if you're logged in

The simplest path is the GitHub CLI:

```sh
gh auth login
```

Or use a [personal access token] with read access to the repositories you care
about (a classic token's `repo` scope, or a fine-grained token with
read-only **Contents**, **Issues**, and **Pull requests**):

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
| `o` | Open current selection in browser |
| `r` | Refresh the current view |
| `?` | Toggle help |
| `q` / `Ctrl-C` | Quit |

## Scope

This is intentionally **read-only**. There is no creating, editing, commenting,
or merging — those add token-scope, validation, and confirmation-flow surface
that the first version deliberately avoids. The browser integration (`o`) is the
escape hatch for actions that need the full GitHub UI.

The global "my pull requests" view uses GitHub's Search API, which has a lower
rate limit than the core REST endpoints (30 requests per minute per user on the
free plan). The app fetches open and closed PRs separately, so opening that view
consumes two search requests.

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

The UI thread blocks on the channel and never performs I/O itself, so a slow
request can never block rendering or keystrokes.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test                              # unit tests (no network)
cargo test --test live -- --ignored --nocapture   # live API smoke test
```

The library/binary split means logic lives in a testable library crate
(`github_tui`) with a thin binary on top.

## License

MIT — see [LICENSE](./LICENSE).

[Ratatui]: https://ratatui.rs
[GitHub CLI]: https://cli.github.com
[personal access token]: https://github.com/settings/tokens
