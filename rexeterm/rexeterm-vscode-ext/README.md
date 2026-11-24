# RexE Term for VS Code

Open a folder on a RexE executor in the VS Code Explorer. Read and edit files, start a remote terminal in the Terminal panel, and manage forwards in the **RexE Ports** tab. The desktop extension uses the existing local `rexeterm-cli` and its daemon; it does not install a server on the executor.

## Install and connect

1. Install the VSIX with **Extensions: Install from VSIX…** and have `rexeterm-cli` 0.2.13 or later available on this computer. Set **RexE: CLI Path** if it is outside PATH.
2. To reuse your CLI setup, set **RexE: Client Directory** to the local directory with `RexETermHostUUID.txt` and `.rexeterm/config-dir`. Set **RexE: Registry Directory** to the same directory you pass to `--registry-dir`. If you use the CLI's executable adjacent registry, enter the full path to its `.rexeterm-socket` directory.
3. For a fresh setup, open **RexE Term** in the activity bar. Use **Start RexETerm Service**, then add an executor in **Executors**. Use a saved executor's profile or select a profile linked under **Credentials (CRs)** when connecting. Existing CLI setup and profile file formats are described in the parent project's [CLI guide](https://github.com/Nanokoli/RExeTerm/tree/main/rexeterm/docs).
4. Click an active executor and choose **Open RexE Folder…** or **Executor Info**. Expand it to see **Folders**, **PTY**, **File Transfers**, and **Port Forwards**.

You can open a folder from the sidebar, the remote indicator in the status bar, the Command Palette, or **File → New File… → Open RexE Folder…**. VS Code's stable extension API does not offer a contribution point for a direct top-level File menu item.

With several executors connected, **Open RexE Folder…** lets you choose the executor even if a RexE folder is already open. Opening a folder on another executor opens a new VS Code window, keeping the original folder and its terminals open. VS Code workspaces can also contain multiple RexE roots; a new RexE terminal follows the active editor's root.

In an open RexE folder, select **RexE** in the Terminal profile dropdown, run **RexE: New RexE Terminal**, or right-click a folder in the Explorer to open a terminal there. The remote shell starts with the panel's initial size; the current CLI IPC does not expose live resize. Use **RexE Ports** in the lower panel to forward, copy, open, or stop ports. The extension uses RexETerm forwarding, and listens on loopback when creating a forward.

The workspace is virtual: it uses the native Explorer, editor, and terminal, but extensions requiring a locally mounted repository or the VS Code Server (for example, some Git and debugger integrations) may not work. Use Explorer Refresh for changes in unopened subfolders. The server's directory listing is line based and does not support names containing newlines or distinguish directory symlinks. File reads and saves use the CLI file-transfer limit (256 MiB). Creating, moving, or deleting folders uses the executor's shell permissions.

## Build

Use Node 22 or later. From `rexeterm/rexeterm-vscode-ext` in the RexETerm repository:

```sh
npm ci
npm test
mkdir -p dist
npm run package
```

The VSIX contains no executables, user settings, or connection profiles. The same VSIX works with Windows x64 or Linux ARM64 desktop VS Code; install the matching RexETerm CLI for each OS.
