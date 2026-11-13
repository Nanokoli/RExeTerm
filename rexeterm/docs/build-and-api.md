# 构建与 API

## 产品与模块

RexETerm 与 RemoteExecutor 相邻放置。提供完整 App、普通 MCP stdio 和带 daemon 模式的单一 CLI 二进制；
daemon 是内部共用模块，CLI 绑定 CLI 宿主的 UUID64 并连接原生本地 IPC（Unix socket / Windows 命名管道），rexctl 连接运维入口。Windows Tauri 工作区已有可运行预览，安装包尚未制作。

## 构建

```bash
# 纯状态核心、后台模块。
cargo build -p rexeterm-appcore
cargo build -p rexeterm-daemon

# 普通 MCP stdio：一个进程，无 HTTP/App 依赖。
cargo build -p rexeterm --no-default-features --features mcp-stdio --bin rexeterm-mcp

# App 后台，带可动态启停的 HTTP；默认没有打开监听。
cargo build -p rexeterm-app
cargo build -p rexeterm-app --no-default-features

# Windows 原生窗口：在 Linux 配置 cargo-xwin、LLVM 和 Windows SDK 后交叉编译。
cargo xwin build --target x86_64-pc-windows-msvc -p rexeterm-desktop --features desktop --bin rexeterm

# 既有辅助入口。
cargo build -p rexeterm --bin rexeterm
cargo build -p rexeterm --no-default-features --features admin --bin rexctl
```

纯 AppCore 没有 local feature；daemon/local 才引入本地执行器。
根包默认组合 stdio、mcp-stdio、local、admin，默认不含 HTTP 或 AppHost。
根包 app feature 导出 AppHost，mcp-http 为它增加 HTTP；App 库本身默认编入 HTTP，但运行时默认关闭。
两个适配器可按包分别构建。stdio、HTTP 与本地执行器的构建选择互不绑定。

## 接口与迁移

| 旧接口 | 新接口 |
| --- | --- |
| appcore::runtime::AppRuntime / AppOptions | rexeterm_daemon::Daemon / DaemonOptions |
| appcore::local | rexeterm_daemon::local |
| appcore/local feature | daemon/local feature |
| appcore::jsonl | rexeterm_adapter_common |
| rexeterm-mcp 库 | mcp-common、mcp-stdio-adapt、mcp-http-adapt 三个包 |
| 根包 mcp feature | mcp-stdio |
| rexeterm-mcp --transport stdio | rexeterm-mcp |
| rexeterm-mcp --transport http | AppHost 的 enable_mcp_http |
| start_mcp_http(AppOptions, settings) | start_mcp_http(AppCore, settings) |
| 适配器 run_*_with_options | 宿主先创建 Daemon，再 run_*_with_core |

AppCore 的 Client 和偏好 API 保留。AppHost::core 与 Daemon::core 返回该共享状态核心。
旧 HTTP 每会话独立应用核心的行为已改为共享 App 状态；DELETE/关闭 HTTP 不再退出 RE 会话。
需身份隔离时创建独立核心或独立产品实例。RE 线上协议及偏好文件格式未改。

```rust
use rexeterm_daemon::{Daemon, DaemonOptions};
let daemon = Daemon::new_with_options(DaemonOptions::default()).await?;
rexeterm_mcp_stdio_adapt::run_mcp_stdio_with_core(daemon.core().clone()).await?;
daemon.shutdown().await;
```

## 检查

```bash
bash scripts/check-boundaries.sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
cargo test --locked --workspace --doc
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
```

当前工作流将所有编译集中在 Linux Builder，Windows 只接收 exe、PDB 与来源记录用于测试和调试。
其他平台暂未启用桌面入口。Windows 运行端需要 WebView2，无需 Rust/MSVC 编译环境。
桌面默认不创建本地执行器，文件传输使用统一 RE 数据通道。
前端检查使用 `node --check app/frontend/app.js` 和 `node --test app/frontend/utils.test.mjs`。
普通 HTTP API 作为独立库适配器保留；CLI 不依赖 HTTP。CLI 自启动 daemon，原生 IPC 支持 Unix socket 和 Windows 本机命名管道，UUID64 在系统重启后失效；尚未接入系统服务管理器。
详见[HTTP 与 CLI 指南](http-api.md)。

## 相关文档

- [桌面运行指南](../app/docs/getting-started.md)
- [App 后端](../app/docs/modules/backend.md)
- [文档索引](README.md)
- [RE 构建](../../rexed/docs/build-and-api.md)
