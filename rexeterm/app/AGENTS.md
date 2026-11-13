# RexETerm App

Full GUI product, composed from AppHost, daemon, AppCore and optional mcp-http-adapt.
backend implements AppHost and Desktop command composition over crates/application; frontend and src-tauri implement the Windows Tauri preview.
MCP HTTP is compiled optionally and starts disabled. UI calls enable/disable and read actual listener status.
HTTP uses the same AppCore as the UI, and closing HTTP preserves business state. Final exit awaits AppHost::shutdown.
Daemon is an internal host module. Ordinary HTTP/Unix IPC host is separate from MCP HTTP. Do not claim Windows named pipes, service installation or full operations GUI exists.
Keep independent ManagementClient and business targets; use raw SDK terminal streams, no MCP subprocess for GUI operations.
Follow ../../rexed/docs/documentation-guide.md and parent AGENTS.md. Compile on the Linux Builder, including Windows cross builds; Windows machines only run tests and debugging. Run frontend checks and real RE/PTY/transfer/forward UI checks for desktop changes.
