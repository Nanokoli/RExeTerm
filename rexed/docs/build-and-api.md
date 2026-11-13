# 构建与 API 迁移

## 范围

RemoteExecutor 保留 RE SDK 和执行端。RexETerm 作为相邻独立工作区，承接应用实例、客户端偏好和入口。
服务端仍为 rexed；REFS 独立项目已移除。线上工具、PTY、文件与会话协议保持原有标识。

## RE 构建

```bash
cargo build -p rexe-client
cargo build -p remote_executor --no-default-features --features server --bin rexed
cargo build -p remote_executor --no-default-features --features server,kcp --bin rexed
```

根包默认只有 server feature；可选 client feature 仅导出 SDK 命名空间，kcp 启用对应角色的可选后端。
构建 RE 不引入客户端偏好、MCP、CLI 或桌面程序。

## 应用迁移

| 原接口或入口 | 当前所有者 |
| --- | --- |
| rexe_client::Client | 保留；专门接入 RE 能力 |
| rexe_client::ClientInstance / ClientInstanceOptions | rexeterm_appcore::AppCore / AppCoreOptions |
| rexe_client::settings | rexeterm_appcore::preferences |
| Client::with_settings_store | AppCore::with_settings_store |
| Client::handle 的偏好工具与 shell 准备 | AppCore::handle |
| remote_executor::app::ClientRuntime / ClientOptions | rexeterm_daemon::Daemon / DaemonOptions |
| rexe-client JSONL 程序 | RexETerm 的可选 stdio 库；新 rexeterm-cli 连接 HTTP/IPC 宿主 |
| rexe-client-mcp | RexETerm 的 rexeterm-mcp，仅 stdio；HTTP 由 AppHost 可选开启 |
| rexctl | 移到 RexETerm CLI 适配器，程序名保留 |
| RexETermApp 目录 | rexeterm/app，Windows Tauri 工作区预览与 App 后台已实现 |

Daemon::core 返回 AppCore。AppCore::re_client 显式访问底层 SDK，绕过偏好准备。
不保留旧应用导出别名；下游升级时迁移依赖和入口。正式 rexed 默认启用 TLS 与用户认证，连接配置见[认证指南](authentication.md)。
原 Caller 命名仍已移除，不重新引入。

```rust
use rexe_client::Client;
let client = Client::new().await?;
// RE 原始工具和流能力。
client.shutdown().await;
```

需要客户端偏好的应用依赖相邻 RexETerm 的 appcore，详见其[构建指南](../../rexeterm/docs/build-and-api.md)。
已有 PTY 迁入、shellCommand、统一文件协议和运维生命周期见各模块文档。独立 pty-t 已移除，终端能力由 RE 内部模块维护。

## 原生 PTY API 迁移

`remote_executor_terminal::Session` 只保留原生 IO、尺寸、进程生命周期和有界原始输出。
原 `register_client`、`unregister_client`、`controller_id`、`force_controller`、
`set_controller`、`set_client_size`、`write_from_client`、`write_from_controller` 已从运行时移除；
原生摘要和详情移除 `clients` / `controller` 字段，随机客户端 ID 分配也上移到服务层。
未使用的 `ServerState::next_token` 分配器一并移除。

可信宿主直接写 PTY 时使用 `Session::write_input`；远端终端继续使用 SDK 的
`Client::open_terminal`，由 rexed 检查归属、连接代次和主控权后调用原生写入。
`ShellManager::list/detail` 继续返回包含连接和主控信息的兼容线上类型。
终端字节、meta、resize、request_control 和 exbash attach 的线上格式保持不变。

## 原始输出与只读查看

原生屏幕快照与 `output_history` / `output_history_text` API 已移除。
`subscribe_output` 返回 `OutputSubscription`，`try_recv` 返回 `Result<Option<Vec<u8>>>`，
`recv` 返回 `Result<Vec<u8>>`。新订阅只收实时字节，每个 viewer 的未读在途字节最多 4 MiB，超出时明确失败。
当前已到达的小块自动合并，单次交付最多 64 KiB；不增加固定等待。
`create_pty_with_output` 提供从进程启动前建立的订阅，避免快速命令在订阅前输出。

exbash 返回 `metadata.terminalOutput`，`format: raw-pty-v1` 表示本次等待窗口原始输出的 UTF-8 投影，
并携带 `cols` / `rows`。单次命令回答收集上限为 1 MiB，超出标记 `outputTruncated`，实时 PTY 流不受此回答上限影响。
后续 attach 不回放旧数据，停止或已退出任务返回状态和空输出。
RexETerm AppCore 按本次片段解析为 MCP/CLI 文字，标记 `client-screen-v1`；
GUI 的二进制输出只交给 xterm。RE SDK 本身继续提供原始能力，不保证新 viewer 恢复旧屏幕。

默认列表仅属当前客户端。已知 PTY ID 允许同 RE 用户的其他客户端只读附着，
`meta.read_only: true` 表达不可升级的权限；输入、resize 和 request_control 都在服务端拒绝，
原主控断开不会把这类 viewer 提为主控。跨用户不可查看，独立运维入口继续提供全局列表。

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

## 相关文档

- [文档索引](README.md)
- [RexETerm](../../rexeterm/README.md)
