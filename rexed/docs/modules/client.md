# rexe-client：RE 能力接入

## 范围

`rexe-client` 是 RE 的客户端 SDK，位于 crates/client。它承接执行端的能力，应用实例和偏好由
[RexETerm appcore](../../../RexETerm/docs/modules/appcore.md) 管理。
SDK 不加载客户端偏好，不处理 list_shells / set_default_shell / request_reload，不定义 MCP 或 CLI 入口。

## 内部结构

| 模块 | 任务与状态 | 实现 |
| --- | --- | --- |
| 节点与路由 | 节点注册、默认和显式目标、端点快照 | [state.rs](../../crates/client/src/state.rs) |
| 能力探测 | 获取身份、版本与能力，核对额外链路 | [endpoint.rs](../../crates/client/src/endpoint.rs) |
| 请求调用 | 工具请求与响应、预算、写操作门控 | [lib.rs](../../crates/client/src/lib.rs)、[rpc.rs](../../crates/client/src/rpc.rs) |
| RE 会话 | 用户标签、绑定凭据、心跳与关闭 | [session.rs](../../crates/client/src/session.rs) |
| PTY | 双向连接及其租约、会话存活 | [terminal.rs](../../crates/client/src/terminal.rs) |
| 文件 | File 通道、收发、进度、散列与提交 | [transfer.rs](../../crates/client/src/transfer.rs) |
| 转发 | 本地 TCP listener、连接与停止 | [forward.rs](../../crates/client/src/forward.rs) |
| 运维 | 独立管理连接和只读快照 | [management.rs](../../crates/client/src/management.rs) |

## 接口与生命周期

Client::new 创建空路由，不启动 shell、PTY 或监听。connect_to_executor 注册远端，首个成功节点成为默认，
随后可以 set_default_executor 或逐请求指定目标。selected_executor 返回身份与 shellCommand 能力快照，供应用层准备请求。
Client::handle 传递 RE 工具；应用层预先生成 shellCommand 时原样发送，不在 SDK 内解析 shell profile。
open_terminal 和 transfer_file 分别提供原始 PTY 与文件 API，ManagementClient 与业务连接独立。

克隆 Client 共享节点、写锁、通道和转发。宿主先停止新工作并等待/取消在途任务，再释放终端并等待 shutdown。
shutdown 清空共享节点、停止转发、尽力关闭 RE 会话，保留远端程序；当前不是永久关闭屏障。
构造阶段可注入身份与 Connector；改变构造对象不会重配已有端点。

## 协作边界

SDK 依赖 protocol、transport 与 data，不依赖服务端、PTY 创建、RexETerm、偏好存储、MCP、HTTP 适配器或 UI。
可选本地执行器由 RexETerm daemon 外层组合，再作为普通端点接入 SDK。
按节点断开/更新、连接状态事件和文件阶段仍是后续接口需求。

## 客户端身份与资源归属

`ClientCredentialStore` 由应用提供持久恢复凭据，SDK 默认仅内存保存。资源归属、宽限期回收和
`client_resources` 由服务端负责；详见[会话与运维](management.md)。
任务 `exbash` 与转发工具的顶层 `title`、`description`，以及文件请求
`FileTransferOptions.metadata`，共用 40 / 200 个 Unicode 字符限制。
任务的 stdio 与 PTY 模式共用元数据；PTY 没有独立标题。

## 相关文档

- [文档索引](../README.md)
- [构建与迁移](../build-and-api.md)
- [RexETerm](../../../RexETerm/README.md)
