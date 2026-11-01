# Navigation Reliability Implementation Plan

**Goal:** Keep rapid navigation responsive without unbounded network workers or
silently changing the highlighted resource after a refresh.

**Architecture:** Retain blocking HTTP, one UI-owned state machine, and the
existing result channel and generation checks. Bound fetch dispatch with a
lazy, session-owned worker pool. Preserve list selections by resource identity,
not their previous position in a refreshed result set.

**Tech stack:** Rust 1.88+, standard-library synchronization, ureq, Ratatui.

**Spec:** The read-only and responsiveness guarantees in
[`../../architecture.md`](../../architecture.md).

## Constraints and Decisions

- No new dependencies, async runtime, persistent state, or mutation endpoints.
- Four concurrent fetches and at most sixteen queued fetches per application.
- Queue admission never waits for network work; saturation reports a retryable
  error through the existing loading-state machinery.
- Invalidating a request cancels its queued work. Active HTTP requests can
  finish within the existing twenty-second timeout; generations reject their
  stale results.
- Purge cancelled queued work on admission so abandoned views do not crowd out
  current work. Dropping the pool discards queued work without blocking exit.
- Preserve an existing selected identity through filtering and successful page
  replacement when it remains visible; otherwise select the first visible row.
- Implement inline, one verified commit per deliverable, followed by the
  requested independent SOL 6.1 max review of the complete PR.

## Review Focus

- Queue saturation must not strand a request in a loading state.
- Cancellation must not cancel a newer generation of the same resource.
- Pool shutdown must neither execute queued HTTP work nor join slow requests.
- Independent repository tabs and personal queues must keep correct selections.
- Reordered rows with the same issue number in different repositories must not
  be mistaken for the same personal-queue item.

## Deliverables

### 1. Bounded, cancellable request lifecycle

- [x] Add deterministic worker-pool tests in `src/requests.rs`: concurrency and
  queue bounds, cancellation reclamation, skipped work, and nonblocking shutdown.
- [x] Observe those tests fail before implementing the pool.
- [x] Add a private lazy pool and cancellation handle; integrate all six fetch
  paths in `src/app.rs` without changing the public REST client.
- [x] Test generation cancellation and synchronous admission errors in the app.
- [x] Update architecture and security documentation for the bounded lifecycle.
- [x] Run formatting, Clippy, and the complete tests; commit the complete unit.

### 2. Identity-stable selection

- [x] Reproduce wrong selections after refreshed repositories, issues, PRs, and
  personal queues reorder; cover query changes and missing selections.
- [x] Preserve each list's selected identity through message application and
  refiltering, retaining existing empty and fallback behavior.
- [x] Update usage documentation; run the full stable/MSRV/documentation/release
  and dependency-audit checks; commit the complete unit.

Verified locally: 106 unit tests on stable and Rust 1.88.0, Clippy with warnings
denied, formatting, rustdoc with warnings denied, release build, RustSec audit
(266 dependencies, no vulnerabilities reported), and the credentialed live
smoke test. Reordering regressions failed on the previous implementation before
the identity-preserving fix.

### Delivery

- [ ] Open a PR to `main` and link it to this thread.
- [ ] Request an independent `gpt-6.1-sol` review with `reasoningEffort=max`.
- [ ] Verify and address actionable findings before merging; observe PR CI.
- [ ] Merge the reviewed head, observe the exact main CI run, and settle only
  after every main check succeeds.
