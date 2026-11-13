# RemoteExecutor

RE execution platform. rexed is the server; crates/client is the pure rexe-client SDK.
RexETerm is a sibling project owning appcore, client preferences, MCP/CLI adapters and the full user app.

Keep protocol, transport, data, terminal, client and server dependencies isolated. RE must never depend on RexETerm.
The client SDK owns nodes, sessions, heartbeats, lanes, raw tools, PTY, file and forwarding; it must not own preference files,
list_shells/set_default_shell/request_reload handlers, shell profile selection or application adapters.
selected_executor exposes target metadata for application request preparation. Keep wire protocols and staged file semantics intact. Resource ownership is persistent clientId; all remote business requires a bound session except _executor_info. Enforce client ownership on inventories and mutations; known PTY IDs permit read-only viewing/attach without input under the same RE username, never control or automatic promotion; and reclaim tasks/PTY/file/forward after configurable heartbeat expiry (default 60 s).
No HTTP file bypass. No caller branding. Preserve the retained projects and Git index.

Run scripts/check-boundaries.sh, cargo fmt, default and all-features strict workspace Clippy/tests and doctests before delivery.
Tests for standalone rexed live in tests/standalone.rs; application integration tests moved to rexeterm/tests.
Read docs/module-boundaries.md and docs/build-and-api.md for state, lifecycle, build and migration details.
Maintain Chinese docs under docs using docs/documentation-guide.md. Do not claim unimplemented authentication, SSH or GUI capabilities.
