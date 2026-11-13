# daemon：后台宿主

## 范围

rexeterm-daemon 是 App 和 MCP stdio 共用的后台宿主库，类型为 Daemon / DaemonOptions。
它负责初始化 AppCore、加载偏好、按需组合本地执行器和最终退出，
并托管 CLI 任务输出的客户端缓冲（见 [cli](cli.md)）。没有独立发布的 daemon 程序。
名称统一使用 daemon，避免 deamon 拼写。

## 内部结构

[lib.rs](../../crates/daemon/src/lib.rs) 负责启动参数和核心生命周期；
[local.rs](../../crates/daemon/src/local.rs) 仅在 local feature 下提供本地 PTY、执行器和观察接口。
身份和偏好在创建本地资源前验证。关闭本地能力时，不读取执行器配置，也不创建 shell 或回环节点。
daemon 不依赖 MCP、HTTP 或界面，不保存第二份业务状态。

## 组合

stdio 产品在主进程直接构造 Daemon，将 daemon.core 的克隆交给 mcp-stdio-adapt，EOF 后调用 daemon.shutdown。
这个过程不派生 daemon 子进程，也不需要后台地址、端口或 IPC。
AppHost 持有 Daemon，界面和可选 mcp-http-adapt 使用同一个核心。后台模块与界面模块分开。普通 HTTP 宿主组合同一 daemon、Application 和 HTTP/Unix IPC，
CLI 连接这个已有宿主，不自行构造核心。

## 生命周期

Daemon::core 返回共享状态核心，shutdown 等待核心关闭 RE 会话与转发；宿主应先结束适配器和在途工作。
业务状态和偏好不以监听器为生命期。App 的 HTTP 开关由 AppHost 独立管理。
local 资源沿用现有本地执行环境的生命周期，显式关闭会话回收所属远端运行资源。

## 相关文档

- [AppCore](appcore.md)
- [App 后端](../../app/docs/modules/backend.md)
- [构建与 API](../build-and-api.md)
- [文档索引](../README.md)
