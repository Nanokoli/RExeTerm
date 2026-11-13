# 原生终端运行时

## 范围

`remote_executor_terminal` 是 RE 自有的 PTY 与进程运行时，直接依赖 `portable-pty` 和并发基础库。
它承接原 pty-t core 的行为，由 RE 维护；没有 WebSocket、SSH、HTTP 或 RE 网络协议依赖。

## 内部结构

### 运行时入口

`PtyManager` 提供创建、查询、输入、退出观察与终止，克隆后共享会话状态。

输入与输出：命令规格、会话名称和尺寸 → 会话或运行结果。

实现位置：[manager.rs](../../crates/terminal/src/manager.rs)。

### 命令与数据

`CommandSpec` 描述程序、参数、目录和环境；`TermSize` 表示终端尺寸。
运行时摘要与详情只表达本地状态，网络层负责转换为兼容消息。

实现位置：[types.rs](../../crates/terminal/src/types.rs)。

### 会话注册表

按名称登记会话并拒绝重名，复用或创建默认会话，列出状态及移除条目。

持有状态：共享的会话映射、默认命令与尺寸。

实现位置：[state.rs](../../crates/terminal/src/state.rs)。

### PTY 和进程创建

通过 `portable-pty` 打开原生 PTY，启动命令并建立读写、退出观察线程。
`Session` 持有主端、写入器、进程与退出信号、实时输出订阅与累计字节计数。

输入与输出：`CommandSpec`、尺寸 → `Session`。

实现位置：[session/mod.rs](../../crates/terminal/src/session/mod.rs)。

### 原始输入

`Session::write_input` 接收原始字节并写入 PTY，不保存客户端 ID、连接 token 或主控权。
RE 服务层先完成归属与主控检查，再调用该接口；直接嵌入运行时的宿主自行承担输入授权。

Windows 后台终端默认自动应答设备状态查询。服务层在可控制的交互终端接入时通过
`set_auto_status_response(false)` 关闭该应答，只剩跨客户端只读 viewer 时恢复。
这是 Windows 无交互窗口时的最小兼容应答，不维护 VT 屏幕或光标状态。
该配置表达终端协议行为，不把网络客户端身份传入运行时。

实现位置：[session/input.rs](../../crates/terminal/src/session/input.rs)。

### 实时原始输出

PTY 输出原样分发给当前订阅者，服务端没有 `vt100` / `vte` 依赖，不存历史，不生成屏幕快照。
没有订阅者时直接丢弃输出；新 viewer 只收到接入后的字节。读取容量为 64 KiB，读取有字节即返回。

`OutputSubscription` 为每个 viewer 保存最多 **4 MiB** 未读在途字节，按字节限制而非 read 块数。
队列将当前已到达的小块合并为最多 64 KiB 的发送块，消费时直接移交 Vec，读完不保留。
从空变为非空时立即唤醒，没有固定等待或历史回放。慢订阅者超出字节预算时明确报错并关闭网络连接，
不会拖住原生读取或其他 viewer。累计输出字节数独立记录，兼容字段 `output_history_bytes` / `output_history_limit` 恒为 0。

`create_pty_with_output` 在读取线程启动前订阅，供一次 exbash 调用收集当前等待窗口的输出。
该调用返回后释放收集结果；后续 attach 只观察自己的等待窗口，停止或已退出任务不返回旧输出。
GUI 在本地由 xterm 解析，MCP/CLI 在 AppCore 按本次观察片段生成文字；不保证重建之前的屏幕。

输入与输出：PTY 字节 → 当前订阅者的在途队列与累计字节计数。

实现位置：[session/output.rs](../../crates/terminal/src/session/output.rs)。

### 尺寸、终止与退出

调整 PTY 尺寸，终止进程，缓存退出码并通知当前及后续订阅者。
终止、等待退出与从注册表移除由不同操作表达；Exbash 在上层组合它们。

输入与输出：尺寸、终止或等待请求 → PTY 状态及退出码。

实现位置：[session/process.rs](../../crates/terminal/src/session/process.rs)。

### 元数据

生成运行时会话摘要与详情，包括命令、目录、进程 ID、尺寸、输出和退出状态；连接列表与主控权由服务层补入线上摘要。

实现位置：[session/metadata.rs](../../crates/terminal/src/session/metadata.rs)。

## 与 RE 服务端协作

`ShellManager` 使用终端运行时，服务层的 [attachments.rs](../../crates/server/src/shell_manager/attachments.rs)
集中维护连接登记、连接代次、尺寸、主控者、锁定和主控会话归属。
每条终端连接取得缓存的 `TerminalAttachment`，输入只访问该任务的状态锁与原生写入器。
主控切换、当前连接代次检查、输入授权和实际写入通过同一把每任务锁串行化。
旧连接不能通过已重用的显示 ID 重新取得主控、修改尺寸或清除新连接。
exbash 有输入的 attach 通过同一服务层申请主控并写入，也遵守终端锁定和客户端归属。
同 RE 用户的其他客户端可以凭已知 PTY ID 只读查看，无权输入、resize、申请主控或在主控断开后继任。
默认列表和任务详情仍仅返回归属客户端的资源；跨客户端观看不改变任务归属或回收计时。
它向适配器发送 `TerminalEvent`，终端数据适配器负责转换为线上消息或关闭帧，传输层负责具体链路封装。
监听统一在 `start_shared_executor_ws` 创建，终端管理器不再自行创建第二套监听入口。

终端消息定义由 RE 的 [protocol/terminal.rs](../../crates/protocol/src/terminal.rs) 持有。
现有 `ptyt`/`ptyc` 消息继续兼容；远端管理只允许列表与详情。
独立 pty-t 项目已移除；会话内核和协议由 RE 内部模块维护。

## 验证与裁剪边界

[core_api.rs](../../crates/terminal/tests/core_api.rs) 验证目录与环境、退出码、退出订阅、实时订阅、无历史回放、快速命令首字节和慢订阅者隔离。
服务层 [attachments/tests.rs](../../crates/server/src/shell_manager/attachments/tests.rs)
验证 ID 冲突、主控输入、旧代次拒绝、关闭后重新附着与 GUI / exbash 统一锁定。
RE 既有测试继续验证 Exbash、客户端归属、终端连接、超时清理及停止移除。

原生运行时已移除连接登记与主控 API，输入改为原始字节接口；线上终端消息保持兼容。
公开的 `server::pty`、`server::terminal_protocol` 继续指向 RE 自有代码。
`ShellManager::core()` 仍返回具体的 RE 运行时；进一步收紧运行时接口属于后续裁剪，不视为已完成的 trait 抽象。

## 程序与控制归属

RE 服务端 ShellManager 记录程序创建时的 RE 用户 / 会话，以及终端客户端绑定的会话归属。
程序当前控制者及控制会话以服务层的 attachment 状态为事实来源；
创建者和控制者分别呈现，切换控制不会改写创建者。
这些信息不进入本 crate，避免引入用户、链路或运维协议依赖。详见[会话与运维](management.md)。

## 相关文档

- [文档索引](../README.md)
- [Exbash 与终端](exbash.md)
- [模块边界](../module-boundaries.md)
- [构建与 API 迁移](../build-and-api.md)
- [文档规范](../documentation-guide.md)
