# 通用流与端口转发

## 范围

转发的数据契约是可靠有序的双向字节流。首个入口是类似 SSH `-L` 的本地 TCP 监听：
Client 接受本地连接，经选定传输链路送到 rexed，再由 rexed 打开目标 TCP 或 Unix 套接字。
串口、设备可以通过目标适配器接入；传输链路本身也可以是不同介质。

当前没有 UDP 数据报、远端监听 `-R` 或 SOCKS `-D`。KCP 是链路后端，不改变被转发服务的 TCP 语义。
此模块也没有实现 SSH 握手、认证或加密。

## 模块职责

| 模块 | 所有状态或输入输出 | 职责 |
| --- | --- | --- |
| 客户端 `ForwardRegistry` | listener、会话任务、活动数量、最近错误 | start / list / stop；Client 克隆共享 |
| transport `ChannelPool` | Forward 租约与物理容量 | 每条连接选择一个链路，不解释目标 |
| protocol `StreamMessage` | Open / Ready / Eof / Error | 与链路类型无关的数据契约 |
| data `StreamOpener` | `StreamTarget` → `ByteStream` | 打开目标，允许注入设备适配器 |
| data `bridge` | Link ↔ ByteStream | 双向字节、背压、独立 EOF |
| 服务端 Executor | opener 与 32 个共享 stream slots | 识别开流首帧，限制活动目标连接 |

实现位置：[forward.rs](../../crates/client/src/forward.rs)、[forward 协议](../../crates/protocol/src/forward.rs)、[data crate](../../crates/data/src/lib.rs)、[服务端接入](../../crates/server/src/executor/data.rs)。

## 工具入口

`forward` 由 Client 本地处理，stdio、MCP 和 Rust Client 使用同一注册表。直接向 Executor 发送同名工具不会创建监听器。

### 开始

```json
{
  "id": "forward-1",
  "tool": "forward",
  "executor": "box",
  "params": {
    "mode": "start",
    "listen": "127.0.0.1:15432",
    "target": {"kind": "tcp", "address": "127.0.0.1:5432"},
    "maxConnections": 8
  }
}
```

原始 stdio / Rust 使用顶层 `executor`；MCP 将 `targetExecutor` 放在工具 arguments 内。

`listen` 是 Client 所在机器的 TCP 地址，可加 `tcp://`；省略时绑定 `127.0.0.1:0`。
`target.address` 在 rexed 所在机器解析。`kind: "unix"` 使用远端 Unix 路径；其他类型需要注入 `StreamOpener`。
`maxConnections` 为 1～1024，默认采用端点 `channels.forward`，同时受该端点通道总限额约束。

结果的 `metadata` 包含 `forwardID`、实际 `listen`、`executor`、`target` 和 `maxConnections`。
开始只建立本地监听器；接收到本地连接后才打开远端目标。目标不可达时关闭该本地连接，并将错误记入 `lastError`。
端点不具备 `streamForward` 能力时，开始操作直接报升级错误。

### 查看与停止

```json
{"id": "list", "tool": "forward", "params": {"mode": "list"}}
{"id": "stop", "tool": "forward", "params": {"mode": "stop", "forwardID": "forward-1"}}
```

list 返回当前 Client 的全部 `forwards`，包括 `activeConnections`、`lastError` 和 `running`。
stop 通过本地 `forwardID` 定位，关闭监听器并取消该项的活动会话，不必再选择执行器。
未知 ID 返回错误，已停止条目不再列出。
转发不持久化；最后一个 Client 克隆析构时取消全部监听器和会话。已启动转发持有端点快照，不随重新注册同名端点迁移。

## 数据与关闭

### 握手

链路首帧为文本 `{"type":"re.stream.v1.open","target":{"kind":"tcp","address":"host:port"}}`。
服务端取得 stream slot，在最长 30 秒内打开目标；成功响应 `re.stream.v1.ready`，失败响应 `re.stream.v1.error` 及消息。
客户端通道等待、建立与握手合计最长 30 秒。握手后只传递二进制数据与流控制消息。

### 双向桥接

每个方向按最多 64 KiB 读取和发送；发送与写目标均等待下游接受，形成背压。
一条流固定在一个 Link 上，不跨链路拆分字节，不重放数据。
本地读取 EOF 后发送 `re.stream.v1.eof`；收到 EOF 后只关闭目标写方向，保留反向读取，直到双方完成。
因此“请求方写完并半关闭，服务端读到 EOF 后再回复”的 TCP 协议可以工作。

异常关闭会取消另一个方向并释放本地资源。传输 Close 表示整条链路结束，区别于单方向 Eof。
停止时有界尝试发送 Close；断网、取消于残缺帧或设备没有可靠关闭语义时，远端释放仍取决于后端超时。

## 设备扩展

嵌入方通过 `Executor::with_stream_opener(Arc<dyn StreamOpener>)` 注册目标打开器，返回任意满足 `ByteStream` 的对象。
可在适配器中分派 `kind: "serial"`、设备路径及设备专有选项；替换 opener 后需自行委托 NativeStreamOpener 以保留 TCP / Unix。
这与 Client 的 `with_connector` 不同：前者打开远端被转发的目标，后者建立 Client 到 rexed 的链路。

字节流不会自动保留设备报文边界。UDP 等有消息边界的目标需要另行定义数据报协议，不能直接伪装成 ByteStream。

## 验证

[forwarding.rs](../../../rexeterm/tests/forwarding.rs) 覆盖 TCP / WS / 可选 KCP 的 8 个并发会话、各 256 KiB 二进制往返、EOF 后响应、停止释放两端和 Client 析构清理。
同文件验证多链路节点核对、Query 分布、模拟设备目标和不支持的 UDP 目标。
[MCP 测试](../../../rexeterm/tests/mcp_test.rs) 与 [stdio 测试](../../../rexeterm/tests/stdio_test.rs) 验证两种入口。
设备覆盖使用内存双向流，尚未测试真实串口或物理硬件。

## 相关文档

- [传输层与数据层](transport.md)
- [Client](client.md)
- [Executor](executor.md)
- [文档索引](../README.md)
