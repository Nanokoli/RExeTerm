# RexETerm

Entrypoints: full App, ordinary MCP stdio in one process, and one rexeterm binary with a daemon/--daemon mode (runtime role rexetermd). CLI business invocations own no AppCore/RE state; service create/start self-launch the current executable as a detached daemon.
CLI-only IDs/bindings survive process exit and expire on OS reboot. Discovery records and private tokens are under executable-adjacent .rexeterm-socket/<boot-id>/; Unix sockets are directly inside .rexeterm-socket. The default UUID64 binding is cwd/RexETermHostUUID.txt, written automatically by service create or an unbound foreground daemon; bind changes only the current directory. Explicit --service-id takes precedence without reading or changing the cwd binding; --registry-dir relocates runtime mappings only. Never fall back to executable-wide or parent-directory bindings. CLI config selection remains cwd/.rexeterm-config unless explicitly set. GUI/MCP do not use CLI bindings.
CLI-to-daemon IPC uses versioned length-prefixed messages over Unix sockets or local-only Windows named pipes, with service-ID and private-token verification. No HTTP/TCP fallback. File arguments use the same-machine filesystem and unified RE file channels. Preserve cwd .rexeterm.disable checks before all host discovery/startup.
Daemon remains a shared host module. Application owns shared Host/file/preview business; Desktop and the optional REST library reuse it. The standalone rexeterm-http executable is retired; HTTP adapters are independent of CLI IPC.
AppCore is the shared state core (Client, private recovery credentials, preferences, call preparation), never a process or transport host.
crates/daemon owns startup, optional local execution composition and final shutdown; no adapter dependencies.
mcp-stdio-adapt and mcp-http-adapt are independent packages, reusing mcp-common and adapter-common.
Adapters accept an existing AppCore; never launch their own daemon or shut down an owner's core.
App HTTP sessions share App state but retain independent protocol IDs, versions, cancellation and expiry.
HTTP stop/DELETE must preserve App connections, preferences and forwards; AppHost shutdown finally closes them.
AppHost serializes HTTP toggles and final shutdown. Root mcp binary is stdio-only, with no HTTP flags or default HTTP dependency.
Application owns saved hosts, bounded PTY preview and file jobs; reuse the shared core and SDK streams. AppCore owns bounded task outcome history; RE only keeps short final receipts.
Keep local execution optional in daemon, SDK RE capabilities in sibling RemoteExecutor, shell preferences in AppCore.
Do not duplicate router/session/preference state. Preserve stdout framing, limits, Origin validation and explicit shutdown.
Run boundaries, fmt, default/all-feature strict workspace Clippy/tests and doctests. Chinese docs follow ../RemoteExecutor/docs/documentation-guide.md.
Preserve prior work and Git index. REFS and the standalone pty-t project are retired. Never claim unimplemented GUI, auth or SSH capabilities.
