# RexE Term for VS Code

Open a folder on a RexE executor in the VS Code Explorer. Read and edit files, start a remote terminal in the Terminal panel, and manage forwards in the **RexE Ports** tab. The desktop extension includes its own native `rexeterm-vscode-backend`. Connect to a machine where `rexed` is already running.

## Install and connect

1. In **Extensions: Install from VSIX…**, choose the package for the computer running VS Code. The extension starts its bundled backend when used.
2. Open **RexE Term** in the activity bar. In **Executors**, save each host's ID and RexE endpoint. In **Credentials**, import a local JSON credential profile. Import keeps only the login and TLS settings and resolves any relative certificate paths against the original profile.
3. In **Configs**, choose an executor, a credential and the starting folder on that host. Choose **No Credential** only if the server accepts unauthenticated connections. A config can connect directly or choose another config under **Jump Host**. Click a config to connect. Different configs can connect to the same host at the same time; their names identify the sessions and their VS Code folders.
4. **Active Executors** lists the hosts you connected to. Choose **Open RexE Folder…** or **Executor Info** to see the folder or the connection route. Expand a connection to see **Folders**, **PTY**, **File Transfers**, and **Port Forwards**. For local use, enable **RexE: Local Executor**.

To reuse an existing inventory, set **RexE: Client Directory** to the local directory containing `.rexeterm/config-dir`. Existing executor entries with a starting path or saved credentials still appear as legacy configs with their original IDs. Create a named config to use a saved host as a jump. Using the original ID for a named config keeps existing workspace folders associated with that ID.

For example, create a direct **gateway** config, then create **build-container** with **gateway** as its jump and `/workspace` as its start folder. If **gateway** needs an upstream config, RexE follows the full chain automatically. The client must reach the first host; each following address must be reachable from the preceding executor, which needs permission to forward ports. If the gateway cannot resolve a Docker service name, use the container's Docker bridge IP and keep the certificate's name in the credential's `tls.serverName`. RexE verifies the target's TLS identity through each forward.

You can open a folder from the sidebar, the remote indicator in the status bar, the Command Palette, or **File → New File… → Open RexE Folder…**. VS Code's stable extension API does not offer a contribution point for a direct top-level File menu item.

With several executors connected, **Open RexE Folder…** lets you choose the executor even if a RexE folder is already open. Opening a folder on another executor opens a new VS Code window, keeping the original folder and its terminals open. VS Code workspaces can also contain multiple RexE roots; a new RexE terminal follows the active editor's root.

In an open RexE folder, select **RexE** in the Terminal profile dropdown, run **RexE: New RexE Terminal**, or right-click a folder in the Explorer to open a terminal there. The remote shell starts with the panel's initial size; live resizing is not supported yet. Use **RexE Ports** in the lower panel to forward, copy, open, or stop ports. The extension uses RexETerm forwarding and listens on loopback when creating a forward.

The workspace is virtual: it uses the native Explorer, editor, and terminal, but extensions requiring a locally mounted repository or the VS Code Server (for example, some Git and debugger integrations) may not work. Use Explorer Refresh for changes in unopened subfolders. The server's directory listing is line based and does not support names containing newlines or distinguish directory symlinks. File reads and saves are limited to 256 MiB. Creating, moving, or deleting folders uses the executor's shell permissions.

## Build

Use Rust and Node 22 or later. For a native Linux x64 build, run these commands from the repository root:

```sh
cargo build --locked --release -p rexeterm-cli --bin rexeterm-vscode-backend
cd rexeterm/rexeterm-vscode-ext
npm ci
npm test
npm run package -- linux-x64 ../../target/release/rexeterm-vscode-backend
```

The VSIX includes the native backend for its target (`linux-x64`, `linux-arm64`, or `win32-x64`). It contains no user settings or connection profiles. The backend shares the RexETerm core and private local IPC with the CLI, and uses JSON on its own stdin/stdout interface.
