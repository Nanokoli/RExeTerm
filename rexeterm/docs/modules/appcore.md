# appcore：共享状态核心

## 范围

rexeterm-appcore 定义 AppCore，持有 rexe-client 实例和客户端偏好。它是状态与调用核心，
不负责进程、监听器、输入输出循环或本地执行器启动。App、stdio 和普通 HTTP 宿主复用同一套实现；CLI 访问宿主。

## 内部结构

| 子职责 | 状态与处理 | 实现 |
| --- | --- | --- |
| 实例状态 | Client、RE 身份和偏好 store | [lib.rs](../../crates/appcore/src/lib.rs) |
| 偏好 | profile、默认 shell、节点/目录覆盖、保存与重载 | [preferences](../../crates/appcore/src/preferences/mod.rs) |
| 终端文字 | 对标记的当前原始输出片段解析为 MCP/JSONL 文字，不恢复历史屏幕；GUI 由 xterm 解析 | [terminal_output.rs](../../crates/appcore/src/terminal_output.rs) |
| 调用准备 | 选目标、解析有效偏好、生成 shellCommand | AppCore::handle |
| 持久身份 | 偏好旁私有恢复凭据文件，原子保存与文件锁；交给 SDK 使用 | [identity.rs](../../crates/appcore/src/identity.rs) |
| RE 运行事实 | 连接、RE 会话、PTY、文件、转发和调用锁 | 委托现有 rexe-client，不复制状态 |

list_shells、set_default_shell、request_reload 在核心本地处理；其余工具与流能力复用 Client。
任务列表和资源归属由 rexed 按持久 `clientId` 返回，核心不保存确认 ID 的过滤索引。
重连或重建 AppCore 时，同一个身份文件可以恢复仍在宽限期内的任务；默认内存核心各自独立。
克隆与 App 的 MCP HTTP 会话共享 SDK Client，因此共享一个客户端身份。
App 内 MCP 仅是适配入口，不单独登记为 RE 客户端；独立创建的 AppCore 才有独立实例。
恢复凭据位于各执行器目录的 `identities.json`，不进入偏好工具结果；
Unix 权限 `0600`，Windows 使用当前用户 DACL。旧集中凭据按连接延迟迁移，保持 clientId。
见 [客户端配置布局](../client-config.md)。
共享该文件表示同一逻辑客户端；不同客户端应使用独立配置目录。凭据保存由文件锁协调。
任务、文件与转发的 `title`、`description` 原样传给 SDK，PTY 使用对应任务本身的信息。
标题、描述分别最多 40、200 个 Unicode 字符；缺省为“未命名”和空字符串。
re_client 显式访问 SDK，会绕过偏好准备。克隆共享同一实例状态；独立构造的核心内存隔离。
同一个偏好文件被不同核心打开时，写入协调仍由外层宿主负责。

## 任务历史

[history.rs](../../crates/appcore/src/history.rs) 观察远端终态并保存有界元数据历史，
确认完成后消费短期回执。持久化、失败与重启行为见[生命周期](../../../rexed/docs/lifecycle.md)。
GUI 和 REST 查询共用该历史，完成记录保留任务的标题和描述，不要求 rexed 保存完成任务。

## 接口与生命周期

```rust
use rexeterm_appcore::{AppCore, AppCoreOptions};
let core = AppCore::new_with_options(AppCoreOptions::default()).await?;
// core.handle 处理偏好与 RE 调用；core.re_client 访问原始 RE 能力。
core.shutdown().await;
```

Core 的构造仅初始化状态。daemon 负责正常宿主流程，AppHost 负责 App 的 HTTP 开关。
关闭某个适配器不应关闭这个共享核心。退出时宿主先停止新请求并取消/等待在途工作，再 shutdown。
独立 ManagementClient 的连接与业务连接分别管理。UI 展示状态及尚未实现的任务事件模型另见 App 设计。

## 相关文档

- [daemon](daemon.md)
- [偏好](preferences.md)
- [MCP](mcp.md)
- [文档索引](../README.md)
