# 模块边界

## 项目与依赖

| 模块 | 负责 | 项目内依赖 |
| --- | --- | --- |
| protocol | 线上类型与方法策略 | 无 |
| transport | 不透明帧与物理链路适配 | 无 |
| data | 文件和通用流数据交换 | protocol、transport |
| terminal | 原生 PTY 与进程 | 无 |
| rexe-client | 承接 RE 能力 | protocol、transport、data |
| server | 执行工具、PTY 和服务监听 | protocol、transport、data、terminal |
| RexETerm appcore | 管理 RE 客户端实例与偏好 | rexe-client，无执行环境依赖 |
| RexETerm daemon | 初始化与退出、可选本地执行器 | appcore；local feature 可选引入 server |
| RexETerm application | Host、文件任务和终端预览共享服务 | appcore |
| RexETerm 适配器 | MCP / REST / SSE / WebSocket | appcore 或 application |
| RexETerm CLI | 连接既有宿主的 HTTP/IPC 客户端 | HTTP 客户端，无 SDK/core |
| RexETerm app | AppHost、Desktop 命令、Windows Tauri 工作区与 HTTP 开关 | daemon、appcore、可选 HTTP 适配器 |

依赖方向为 RexETerm -> rexe-client -> RE 基础 crate。RE 不依赖 RexETerm。
偏好管理与 shell 调用准备位于 RexETerm/crates/appcore/src/preferences 和 appcore::handle，SDK 只传递已准备的请求。

## 状态与生命周期

Client 持有节点、会话、转发、通道和调用锁；AppCore 持有 Client 与偏好 store；Daemon 组合可选本地执行器。
服务端持有实际进程和文件。原生 terminal 持有 PTY / 进程及实时原始字节订阅，不维护终端屏幕；
server 的 attachment 服务持有终端连接、主控、锁定和输入授权，运行时不保存客户端身份。运维连接独立，错误或取消后重新建立连接。
宿主负责在途任务和退出顺序，SDK shutdown 显式关闭 RE 会话并回收所属运行资源。

## 验证

两个工作区分别运行 scripts/check-boundaries.sh、fmt、严格 Clippy、workspace tests 和 doctests。
单独验证角色时使用 -p，避免 workspace feature 合并掩盖依赖边界。

## 相关文档

- [设计总览](design.md)
- [构建与迁移](build-and-api.md)
- [文档索引](README.md)
