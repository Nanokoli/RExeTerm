# 共享应用服务

## 范围

`rexeterm-application` 位于 AppCore 之上，为 Tauri、普通 HTTP 等入口提供共同业务。
它接受已有的 `AppCore` 共享句柄，不创建 daemon，不拥有协议监听器，也不复制 RE 会话。

## 内部职责

[lib.rs](../../crates/application/src/lib.rs) 持有 Host 工作区、连接操作锁、文件 jobs 与终端预览。
Host 配置原子落盘，只保存认证文件路径。连接与 Host 更新通过异步锁串行化；
REST 请求始终指定 Host，连接不改变全局默认目标，GUI 可显式选择默认 Host。
目录、搜索、属性和预览使用同一 AppCore 准备后的 RE 调用。
传输任务调用 SDK `transfer_file`，进度来自同一 observer，终态保留字节、SHA-256 和速率。

[terminal.rs](../../crates/application/src/terminal.rs) 维护 GUI 的有界 PTY 预览和二进制批次。
普通 HTTP 的 WebSocket 直接桥接 SDK 原始流，CLI 在本地终端显示，不使用 GUI 预览解析。
任务终态观察和历史由 AppCore 保存，不在各适配器重复实现。
桌面的运行命令对话框接收任务标题和描述，转交 rexed 保存。
任务表显示标题，悬停标题可见描述；终端页签和总览复用任务标题，同时保留任务 ID 与命令。
已退出任务从 AppCore 历史读取同一组元数据。

## 生命周期与边界

`Application::shutdown` 关闭应用任务与预览，核心由其宿主最终关闭。
`app/backend/Desktop` 只组合 AppHost 和共享 Application，并处理 MCP HTTP 开关。
`http-adapt` 拥有 REST、SSE、WebSocket、认证、中断和暂存，不依赖 daemon 或 Tauri。
CLI 默认只有 HTTP 客户端依赖，不链接 RE SDK、AppCore 或服务端。

## 相关文档

- [HTTP 与 CLI 使用](../http-api.md)
- [AppCore](appcore.md)
- [App 后端](../../app/docs/modules/backend.md)
- [文档索引](../README.md)
