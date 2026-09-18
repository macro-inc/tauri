# Macro fork

Rebased onto upstream `dev` at `2d5c0157` (Tauri 2.11.5).
The original fork head is preserved by `macro-fork-before-upstream-rebase`
(`95a7521b`); the old feature branches are unchanged. The retained net changes
were consolidated before rebasing with `jj`.

## Remaining changes

- **Public protocol delegation:** upstream still keeps `tauri::protocol` private.
  Macro's bundle handler delegates to `tauri::protocol::tauri::get`. Preserve
  that entry point accepting an application handle and a borrowed origin.
  Internally it uses upstream's shared `AppManager` context.
- **Mobile dev proxy safeguards:** upstream #15220 implements asynchronous
  loading and a shared HTTP client. Retain the fork's six-request concurrency
  limit, six idle connections per host, 60-second response timeout, and error
  source-chain diagnostics. As before, the timeout starts after acquiring a
  concurrency permit; it does not bound queue waiting time.
  Keep upstream's short cache lock sections and request-body handling rather
  than the old async implementation.
- **Bundler test serialization:** retain serialization of tests that mutate the
  GitHub mirror environment variable; upstream still mutates it in parallel.
- **Development setup:** retain the Nix flake, editor configuration, and ignore
  rules. `.cargo/config.toml` still defaults to Android; pass an explicit target
  for host checks.

## Removed change and downstream follow-up

The old `Event::Resumed -> RunEvent::Resumed` patch is removed. Tao 0.37 no
longer has that top-level event. Upstream now forwards mobile lifecycle events
as `WindowEvent::Resumed` (initially introduced by #15199).

Before adopting this fork revision, Macro must switch its lifecycle handler
from `RunEvent::Resumed` to `RunEvent::WindowEvent` containing
`WindowEvent::Resumed`, gated to mobile. Macro's dependency pins and lockfile
must also be updated. This rebase does not modify Macro or publish the fork.

## Verification

Passed on `aarch64-apple-darwin`:

- `cargo check -p tauri --target aarch64-apple-darwin`
- `cargo test -p tauri --lib protocol::tauri::tests --target aarch64-apple-darwin`
  (two regression tests for public delegation, assets, request hooks, errors,
  and CORS)
- `cargo test -p tauri-bundler --lib utils::http_utils::tests --target aarch64-apple-darwin`
  (three tests)

Mobile device behavior has not been exercised. In particular, verify resume
handling in Macro and dev-server loading under concurrency on Android/iOS
before release.
