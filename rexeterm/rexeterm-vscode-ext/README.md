# RexE Term for VS Code

Open a folder on a RexE executor in the VS Code Explorer. Read and edit files, start a remote terminal in the Terminal panel, and manage forwards in the **RexE Ports** tab. The desktop extension includes its own native `rexeterm-vscode-backend`. Connect to a machine where `rexed` is already running.

## Install and connect

1. In **Extensions: Install from VSIX…**, choose the package for the computer running VS Code. The extension starts its bundled backend when used.
2. Open **RexE Term** in the activity bar, add an executor in **Executors** and link its connection profile under **Credentials (CRs)**. Click the executor to connect. For local use, enable **RexE: Local Executor**.
3. To reuse existing saved executors and credentials, set **RexE: Client Directory** to the local directory containing `.rexeterm/config-dir`. Connect once in VS Code; the extension manages its own sessions across its windows. The old **CLI Path** and **Registry Directory** settings are no longer needed.
4. Click an active executor and choose **Open RexE Folder…** or **Executor Info**. Expand it to see **Folders**, **PTY**, **File Transfers**, and **Port Forwards**.

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
