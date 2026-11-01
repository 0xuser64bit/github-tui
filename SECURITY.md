# Security policy

This policy covers private reporting, the protections around your token and network traffic, and what is in or out of scope.

## Project status

`github-tui` is pre-1.0 and has no published release artifacts. The maintainer applies security fixes to `main`. If you build from an older commit or a local fork, you are responsible for carrying those fixes forward.

## Report a vulnerability

Please **do not open a public issue with vulnerability details**.

If the repository offers private vulnerability reporting, [open the private vulnerability report form](https://github.com/0xuser64bit/github-tui/security/advisories/new) from **Security → Report a vulnerability**.

If that form is unavailable, open a minimal public issue asking for a private channel. Do not include reproduction steps, tokens, or exploit details in the public issue.

A useful report includes:

- the affected commit or version;
- your operating system and terminal;
- the relevant token type and scopes, **never the token itself**;
- reproduction steps and the observed impact;
- whether the issue requires a malicious repository, a compromised local environment, or another precondition.

The maintainer handles reports on a best-effort basis. There is no bug bounty program.

## Security model

### Credentials

- The app resolves a token once at startup from `GITHUB_TOKEN`, `GH_TOKEN`, or `gh auth token --hostname github.com`.
- The app holds the token in memory for the session. It never writes the token to disk, includes it in logs, or renders it in the UI.
- `Github` has a manual `Debug` implementation that prints `<redacted>` instead of the token.
- The app supports github.com only. It ignores Enterprise hosts and `GH_HOST`.
- Use a fine-grained token with read-only Metadata, Issues, and Pull requests permissions, limited to the repositories you need.

The token is still visible to anything that can inspect your environment or process memory with the same operating-system privileges. A shell history entry containing `GITHUB_TOKEN=...`, a terminal recording, or a screenshot can also expose it. Those are local operational risks, not protections the app can provide.

### Network behavior

- The app sends HTTPS requests to `https://api.github.com` with the standard GitHub API version header.
- The client validates pagination continuations against the GitHub API scheme and authority before attaching the bearer token.
- The client may preserve authorization across same-host redirects; it does not forward authorization to another host.
- Requests have a 20-second timeout and bounded response bodies.
- A session runs at most four network fetches concurrently with at most sixteen queued. Abandoned queued requests are cancelled, and a full queue reports a retryable error without blocking the UI.
- There is no telemetry, analytics, update check, or hidden third-party request.
- The app restricts browser launches to canonical `https://github.com/...` URLs with no query string and a conservative path character set.

### Read-only behavior

The client models read operations only. It does not expose a generic arbitrary-method request function, and there are no endpoints for creating, editing, commenting, reviewing, merging, or deleting anything on GitHub. Every application request is a `GET`.

The browser handoff is intentionally outside this boundary: actions you take on github.com use your normal GitHub session and are not mediated by `github-tui`.

### Local state

- No configuration, cache, credential, or state files are written.
- There is no background daemon, scheduled sync, or auto-update process.
- The app restores the terminal on normal exits and on input errors. If the process dies abruptly, terminal state restoration may not run.

## In scope

Examples of issues that are in scope:

- token disclosure through logs, errors, debug output, or UI;
- sending the token to a host other than the GitHub API;
- bypassing the read-only request boundary;
- URL validation bypasses in pagination or browser launches;
- unbounded response handling that can exhaust memory or block the UI indefinitely;
- a vulnerability in a dependency that is exploitable through `github-tui`'s use of it;
- a rendering or input bug with a credible security impact.

## Out of scope

- GitHub service vulnerabilities, API behavior, or account compromise;
- a stolen token used with its legitimate permissions;
- vulnerabilities that require an already-compromised shell, `gh` binary, terminal, or operating system;
- social engineering;
- GitHub Enterprise, which is not supported;
- missing features or product limitations without a security impact;
- reports that require disabling TLS verification or modifying the source to reproduce.

## Secure usage

- Prefer a fine-grained, read-only token over a classic `repo` token.
- Scope the token to the repositories you actually browse.
- Do not paste tokens into shell history, issue reports, screenshots, or recordings.
- Keep Rust dependencies and the GitHub CLI up to date.
- Treat previews and list text as untrusted GitHub content. The app renders them as plain text and never executes them.
- Remember that opening an item in the browser leaves `github-tui`'s read-only boundary; the browser session can perform real actions.

## Disclosure

Please allow time for a fix and a coordinated disclosure. The maintainer will credit you in the advisory unless you ask to remain anonymous. The project offers no bounty or service-level guarantee.
