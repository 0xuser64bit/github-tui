# Contributing to github-tui

Thanks for taking the time to improve `github-tui`. The project is pre-1.0, so small, well-tested changes are more useful than large speculative ones.

This guide covers local setup, the checks CI runs, code conventions, tests, and the pull request process.

## Before you start

- Read the [user guide](docs/usage.md) and [architecture](docs/architecture.md) to understand the intended behavior.
- `github-tui` is **read-only by design**. Changes that add posting, editing, commenting, reviewing, merging, or other mutation endpoints are out of scope.
- For a new feature or a behavior change, open an issue first. Describe the workflow, the expected keys, and what should happen when the network fails. Small fixes and documentation corrections can go straight to a pull request.
- Report vulnerabilities privately according to [SECURITY.md](SECURITY.md), not in a public issue.

## Set up

You need **Rust 1.88 or newer**; CI tests the declared minimum. Then:

```sh
git clone https://github.com/0xuser64bit/github-tui.git
cd github-tui
cargo test --locked --all-features --all-targets
cargo run --release
```

The application needs a github.com token or a logged-in GitHub CLI to show real data. The test suite does not.

## Checks

Run the same gates as CI before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo test --locked --all-features --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
```

If you have the minimum toolchain installed, verify it too:

```sh
cargo +1.88.0 test --locked --all-features --all-targets
```

The suite skips the live smoke test by default because it needs credentials and network access:

```sh
cargo test --test live -- --ignored --nocapture
```

It should only be used deliberately, with a token that has access to the repositories you expect. Never put a token in a test file, fixture, commit, or issue.

## Project layout

| Path | What lives there |
| --- | --- |
| `src/app.rs` | Screens, key handling, filtering, request coordination |
| `src/github.rs` | REST client, pagination, response bounds, HTTP error handling |
| `src/auth.rs` | Token precedence and `gh` CLI fallback |
| `src/event.rs` | Event/message types and background threads |
| `src/models.rs` | Serde models for rendered GitHub resources |
| `src/ui.rs` | Ratatui rendering and overlays |
| `src/main.rs` | Startup, terminal lifecycle, error boundary |
| `tests/live.rs` | Ignored credentialed smoke test |

The binary is a thin wrapper around the `github_tui` library. Keep logic out of `main.rs`.

## Coding guidelines

- Match the surrounding style: explicit state types, small helpers, and comments that explain *why* rather than restating the code.
- Keep the UI thread free of network and process work. New fetches belong on worker threads; new results belong in an `Event`/`Message` path with a generation check.
- Preserve the read-only boundary. If a feature needs a mutation, the answer is to open the browser.
- Prefer actionable, user-facing error strings. The UI displays them directly; avoid leaking tokens or raw transport details that a user cannot act on.
- Avoid `unwrap`/`expect` in production paths. They are fine in tests and for fixed internal invariants.
- Public library items need documentation; CI builds rustdoc with warnings denied.
- Keep the minimum supported Rust version at 1.88 unless the project deliberately raises it. Avoid dependencies that silently raise it.
- Add a dependency only when the standard library or an existing dependency cannot reasonably do the job. Explain the trade-off in the pull request.
- Do not add telemetry, analytics, auto-update behavior, credential persistence, or hidden network calls.

## Tests

Behavior changes need regression coverage:

- **State and filtering:** unit tests in `app.rs` with constructed `App` values.
- **Rendering:** Ratatui `TestBackend` tests in `ui.rs`; assert on rendered text, not snapshots that require an external binary.
- **HTTP:** fixture tests in `github.rs` that bind a local `TcpListener` and assert request headers, pagination, and error paths. Do not make default tests reach the real API.
- **Token resolution:** pure tests in `auth.rs`; avoid invoking the real `gh` CLI.

Tests must be deterministic. Prefer local fixtures and explicit generations over sleeps. If a test needs a credential, mark it ignored and document the command.

## Documentation

Documentation is part of the product:

- Update [docs/usage.md](docs/usage.md) when a keybinding, screen, or limit changes.
- Update the in-app help overlay in `src/ui.rs` when adding a globally useful key.
- Update [docs/architecture.md](docs/architecture.md) when a request path, state model, or concurrency boundary changes.
- Keep README claims verifiable against the code. Do not describe unreleased installers, platforms, or APIs as available.
- Render screenshots from the real application UI with illustrative sample data, and label them as sample data. Do not hand-draw a product screenshot.
- Brand assets live in `assets/brand/`: the wordmark and mark are SVG, and `social-preview.png` is the 1280×640 repository social preview.

## Commits and pull requests

The history uses conventional prefixes such as `feat:`, `fix:`, `docs:`, `test:`, and `chore:`. Keep commits focused and explain the reason for a change, not only the mechanics.

A good pull request includes:

- the user-visible problem and the chosen behavior;
- the key path or API path affected;
- tests or a clear explanation of why no test is practical;
- the verification commands you ran;
- documentation updates;
- screenshots for UI changes, using sample data.

Keep pull requests small enough to review. Do not include generated build output, credentials, or unrelated formatting churn. There is no CLA; the repository's MIT license covers contributions.

## Releases

There are no published release artifacts yet. Version numbers live in `Cargo.toml`; the project will document packaging and release checks when a release process exists. Do not claim a release in documentation before it exists.

## Questions

Open a GitHub issue for product or contribution questions. For security reports, use the private channel in [SECURITY.md](SECURITY.md).
