# RexETerm

RexETerm 提供完整 App、普通 MCP stdio、普通 HTTP 宿主，以及连接宿主的 CLI。
两者共用状态核心 appcore 和后台宿主 daemon；底层 [rexe-client](../RemoteExecutor/docs/modules/client.md) 承接 RE 能力。

## 产品形态

| 版本 | 组合 | 当前状态 |
| --- | --- | --- |
| App | 界面、AppHost、daemon、AppCore；可选 MCP HTTP | Windows Tauri 预览已实现，可连接真实 RE |
| MCP stdio | 单进程内嵌 daemon、AppCore、mcp-stdio-adapt | 已实现普通 MCP；不提供 HTTP 运行模式 |
| HTTP 宿主 | daemon、AppCore、Application、http-adapt | REST、PTY WebSocket、SSE、Unix IPC |
| CLI | UUID64 绑定的本机客户端 | 创建/启停 CLI 宿主，按当前目录绑定，复用 RE 身份与偏好 |

daemon 是共用后台模块。普通 HTTP 宿主提供 REST、二进制 PTY WebSocket、SSE 和本地 Unix IPC；CLI 复用宿主身份。

## 模块

| 模块 | 职责 |
| --- | --- |
| crates/appcore | 共享状态核心：RE 客户端实例、偏好与调用准备 |
| crates/daemon | 生命周期宿主：初始化核心、可选本地资源、退出清理 |
| crates/mcp-stdio-adapt | 普通 MCP stdio 输入输出，接收宿主提供的 AppCore |
| crates/mcp-http-adapt | 可选 HTTP 监听和 MCP 协议会话，使用现有 AppCore |
| crates/mcp-common | 两个适配器共用消息校验、工具声明和调用转换 |
| crates/adapter-common | 有界消息读取和并发预算 |
| crates/cli | UUID64 选择、Unix IPC/回环 HTTP CLI，以及可选 JSONL 和运维库 |
| crates/cli-host | CLI 宿主发现、系统启动期 ID、目录绑定与宿主配置进程锁 |
| crates/application | Host、浏览、搜索、文件任务和 GUI 终端预览 |
| crates/http-adapt | 普通 REST、PTY WebSocket、SSE 与文件暂存 |
| app/backend | AppHost、Tauri 命令组合与 MCP 开关，业务委托共享 Application |
| app/frontend、app/src-tauri | 原生 Tauri 窗口与工作区前端 |

## 文档

- [设计总览](docs/design.md)
- [构建与 API](docs/build-and-api.md)
- [普通 HTTP 与 CLI](docs/http-api.md)
- [文档索引](docs/README.md)
- [完整 App](app/README.md)
- [桌面运行指南](app/docs/getting-started.md)
