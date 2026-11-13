# Tauri 原生入口

包 `rexeterm-desktop`，可执行程序 `rexeterm`。
Windows 使用 Tauri 2 和系统 WebView2；`desktop` feature 显式启用原生依赖。
其他平台保留 workspace 检查能力，尚未启用原生窗口入口。

`src/windows.rs` 初始化 Desktop、注册 `desktop_call`、二进制 `terminal_poll` 与原生文件选择命令，
在退出事件中等待任务和 AppHost 清理。`tauri.conf.json` 从相邻 frontend 嵌入静态资源，
无需启动网页开发服务器。运行步骤见[指南](../docs/getting-started.md)。
