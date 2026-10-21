# Audit remediation

Implemented on `codex/audit-stabilization`, starting from `71953c0`.
The application remains read-only and supports github.com only.

## Commit sequence

| Commit | Change | Audit coverage |
| --- | --- | --- |
| `844fbe0` | Activate the filtered selection before clearing search | F1; keyboard regressions for wrong-target and zero-match activation |
| `ebcf579` | Bind GitHub CLI credential lookup to github.com | F2; precedence, command, and failure tests without credentials |
| `4026c98` | Establish Rust 1.88 and patch rustls/LRU dependencies | F6; exact minimum-compiler CI gate; cache ordering and documentation checks |
| `cef07d0` | Make manual live smoke coverage explicit | F7; missing credentials now fail instead of reporting success |
| `76b14e1` | Show repository ownership and readable states | F10; accessibility, search, personal queues, and narrow-screen rendering tests |
| `23eb3e1` | Add safe page APIs and honest Search/error handling | F5/F9; origin checks, pagination cycles, bounded decoded responses, shared rate-limit cooldowns, and HTTP fixtures |
| `1e7cd85` | Integrate request generations and incremental browsing | F3/F4/F7/F8; account retry, request coalescing, stale-result rejection, retained rows, continuation retry, input errors, and idle redraws |
| `516ba9c` | Reap browser launchers and validate opened URLs | Runtime cleanup; only canonical github.com HTTPS resource URLs reach launchers |
| `2ebbbcb` | Add read-only preview and review-queue API methods | Shared typed details and Search behavior; local HTTP tests |
| `c0020c6` | Add state filters and search by number/author | Small product improvements using existing loaded data |
| `aa6936c` | Update webpki-roots to 1.0.10 | Additional certificate-trust advisory found by refreshing the advisory database |
| `aa51ab4` | Integrate previews and review requests | Optional scrolling descriptions, captured browser targets, fresh preview state, and queue switching |

Focused regression tests accompany the behavior changes rather than a separate
test-framework migration. No new direct dependency was added. Ratatui's existing
`unstable-rendered-line-info` feature provides its own wrapped-line count for
preview scroll bounds; preview tests protect that behavior during dependency
updates.

## Product behavior

- `n` loads the next page while keeping existing rows visible. A failed page can
  be retried without discarding previously loaded data. `r` refreshes from the
  first page and replaces existing rows only after success.
- Duplicate refreshes for an in-flight resource are coalesced. Results carry
  generations so navigating away and back cannot accept an older response.
- Personal queues recover failed account lookup, retain their current results
  and selection while visiting repositories, and start a new query when switching
  between authored PRs (`m`) and requested reviews (`v`).
- `/` matches loaded titles, numbers, and authors. Personal queues also match
  repository names. Repository search still matches names and descriptions.
- `s` cycles state filters; merged and closed PRs are distinct. State and text
  filters compose, and empty results cannot activate an unrelated resource.
- `p` fetches a plain-text description on demand. Closing a preview rejects its
  pending response. Retrying or opening its browser target uses the captured item
  even if the underlying list has changed.
- Browser launchers are waited on outside the UI thread. Terminal input failures
  return through normal cleanup. Idle ticks do not rebuild loaded lists.

## Verification

Validated locally on macOS with the final dependency lockfile:

| Check | Result |
| --- | --- |
| Current Rust: `cargo test --locked --all-features --all-targets` | 93 tests passed; one credentialed live smoke test ignored |
| Rust 1.88.0: the same test suite | 93 tests passed; live smoke ignored |
| `cargo clippy --locked --all-features --all-targets -- -D warnings` | Passed |
| `cargo fmt --all -- --check` and `git diff --check` | Passed |
| `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features` | Passed |
| `cargo build --locked --release` | Passed |
| Fresh RustSec database, `cargo audit --no-fetch --no-yanked` | Passed after dependency patches; 1,348 advisories loaded |
| Release binary in a terminal with tokens removed and `gh` unavailable | Authentication screen, help, Escape, quit, and alternate-screen cleanup passed |

The filtered-Enter regressions were also run against the original behavior:
both failed before the fix and passed afterward. HTTP tests use local fixtures
and synthetic tokens, including redirects, pagination, malformed/oversized and
compressed JSON, Search warnings, and rate-limit recovery.

## Remaining boundaries

GitHub Search exposes at most 1,000 results and searches at most 4,000 matching
repositories per query. The app displays reported cap/timeout warnings and does
not promise exhaustive personal PR history. Live updates can move items between
pages; appended rows are deduplicated by identity, but that cannot recover every
item skipped by a moving upstream page boundary.

Search and state filters operate on loaded rows. There is no disk cache,
credential persistence, GitHub mutation, Enterprise support, rich Markdown
renderer, notification system, or arbitrary saved-query builder.

Real credentialed GitHub requests and GitHub-hosted Linux/Windows/macOS CI runs
were not performed during this implementation. The existing cross-platform
matrix is retained, with an added exact Rust 1.88 job. Yanked-package checks were
excluded from the advisory scan; no claim of a comprehensive security guarantee
is made.
