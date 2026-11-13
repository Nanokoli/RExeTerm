# 服务端日志

## 范围

独立 `rexed` 将连接摘要写到 stdout，将操作审计写到轮转文件。
启动地址、日志路径和运行错误写到 stderr。MCP 与普通客户端不输出这些服务端事件。
活动资源列表仍只保存当前运行状态，日志文件提供有大小上限的历史记录。

## 配置

在程序旁 `rexed-config/server.json` 中添加 `logging`，保留已有证书和运维配置：

```json
{
  "certificate": "server-cert.pem",
  "privateKey": "server-key.pem",
  "adminListen": "tls://127.0.0.1:45958",
  "logging": {
    "enabled": true,
    "directory": null,
    "detail": "operations",
    "categories": null,
    "maxFileBytes": 16777216,
    "maxFiles": 8,
    "maxTextBytes": 4096
  }
}
```

`--config-dir` 改变配置目录。配置只在启动时读取，修改后重启生效。
旧配置省略 `logging` 时使用上述默认值。

| 字段 | 行为与限制 |
| --- | --- |
| `enabled` | 是否写文件；`false` 仍输出 stdout 连接摘要 |
| `directory` | `null` 使用可执行文件旁 `.rexed-logs`；相对路径也以可执行文件目录解析，绝对路径直接使用 |
| `detail` | `summary`、`operations`、`results`；默认 `operations` |
| `categories` | `null` 记录全部；空数组不写普通文件事件；非空数组筛选类别 |
| `maxFileBytes` | 每个文件大小上限，4096～1073741824 字节 |
| `maxFiles` | 1～128 个文件，包含当前文件；默认总预算 128 MiB |
| `maxTextBytes` | 每个文本字段截断阈值，256～65536 字节；按 UTF-8 边界截断并附标记 |

文件位于 `<directory>/<executorId 的 SHA-256>/audit.jsonl`。
旧文件依次为 `audit.1.jsonl`、`audit.2.jsonl`，超过数量后删除最旧文件。
`writer.lock` 阻止同一目录下同一执行器的多个进程同时轮转。
日志目录和文件复用账号配置的私有权限：Unix 为 `0700` / `0600`，Windows 限制为运行账号。
初始化或获取锁失败会阻止启动，并在 stderr 报错。

### 粒度与类别

| 粒度 | 记录内容 |
| --- | --- |
| `summary` | 连接、生命周期、请求完成、成功/失败、持续时间和计数；省略请求开始及操作参数 |
| `operations` | 摘要，以及允许列表中的文件路径、搜索条件、命令、程序参数、工作目录、转发目标和结果元数据 |
| `results` | 操作，以及受 `maxTextBytes` 限制的返回文本 / stderr；可能含文件内容或命令输出 |

类别可选 `server`、`authentication`、`clients`、`files`、`search`、`commands`、
`channels`、`requests`。工具请求按方法归类：文件工具属于 `files`，rg/glob 属于 `search`，
exbash 属于 `commands`，其他工具属于 `requests`。链路、PTY 和转发属于 `channels`。
监听重试属于 `server`。计数在类别过滤之前更新，stdout 不受类别过滤影响。
内部 `logging.dropped` 总会写入，以说明日志缺口。

默认不记录文件内容、文件补丁正文、PTY 字节、输入正文、环境变量、密码、票据或私钥。
命令参数和搜索词本身可能包含用户传入的敏感文本；需要时使用 `summary` 或关闭相关类别。
`results` 是显式保存有限返回文本的选项，不代表完整结果归档。
文本、集合、嵌套和整条记录均有上限，超限有截断标记。

## stdout

只输出 `client.connected`、`client.resumed`、`client.disconnected` 三种 JSONL 事件：

```json
{"timestampMs":1700000000000,"event":"client.connected","executorId":"node","username":"alice","clientName":"editor","clientId":"client-example","sessionId":"session-example","ip":"192.0.2.10","peer":"192.0.2.10:52000"}
```

`username` 是 RE 账号，`clientId` 是持久客户端身份，`sessionId` 是当前绑定。
`peer` 包含对端端口，`ip` 为可解析的网络 IP；本地/注入链路可能为 `null`。
连接事件表示逻辑客户端会话变化。短查询链路关闭不产生客户端断开；
网断后的断开判定受默认 60 秒会话租约影响，不保证实时显示物理 Wi-Fi 状态。

## 文件事件与计数

每行含 `timestampMs`、`executorId`、`event`、`owner`、`peer`、`ip`、`counts`、`details`。
`owner` 含 RE 用户、客户端名称、版本、客户端 ID 和会话 ID。
未认证/未绑定阶段尚无资源归属，`owner` 为 `null`；认证尝试用户名放在 `details.username`。
有实际对端的请求使用其对端地址；异步退出等事件关联该会话最近的连接地址。

| 事件 | 内容 |
| --- | --- |
| `authentication.succeeded` / `authentication.failed` | 认证方法、尝试用户名、对端及错误；每条物理链路都认证 |
| `authentication.authorization_denied` | 网络运维权限拒绝 |
| `link.handshake_failed` | TLS / WebSocket 握手失败或超时，此时通常无法确定 RE 用户 |
| `client.connected` / `client.resumed` / `client.disconnected` | 会话变化、身份、对端和断开原因；断开包含累计计数 |
| `link.opened` / `link.bound` / `link.closed` | 业务链路与归属绑定，同一 `linkId` 关联 |
| `request.started` / `request.finished` | `operationId`、请求 ID、工具、允许参数、耗时、成功/错误及返回元数据 |
| `program.created` / `program.exited` / `program.removed` | 任务命令、参数、PID、目录、退出码或移除状态 |
| `channel.opened` / `channel.closed` | PTY、文件、转发活动通道类型、ID、归属和目标 |
| `file.started` / `file.finished` | 路径、方向、实际传输字节数、`outcome` 和错误 |
| `forward.started` / `forward.finished` | 目标、归属、成功/错误；包括权限拒绝 |
| `pty.control_requested` / `pty.control_changed` | PTY 主控请求和变更 |
| `listener.retry` / `listener.rebound` | 监听错误、重试间隔和同地址重绑成功 |

计数按当前服务端会话累计，随会话重建而重置：

| 字段 | 含义 |
| --- | --- |
| `fileReadRequests` | `read` API 请求次数 |
| `fileWriteRequests` | `FileAction` API 请求次数，包括创建、修改、移动和删除等 |
| `searches` | rg/glob 请求次数 |
| `commands` | exbash run/shell 启动请求次数 |
| `failedRequests` | 已返回失败响应的工具请求次数 |
| `uploads` / `downloads` | 上传 / 下载尝试次数，包括权限拒绝 |
| `channelsOpened` | 已登记的 PTY、文件、转发通道次数 |

这些是请求/尝试计数，包含失败操作，不是操作系统的 read/write 系统调用次数。
没有逐字节记录文件内容或 PTY 流。
`request.finished.ok=true` 仅表示本次 API 调用成功；后台命令最终结果查看 `program.exited.exitCode`。
文件 `outcome=ok` 表示服务端传输处理成功，不能代替对端最终落盘确认。
取消的请求记录 `outcome=cancelled`，提前关闭的流记录 `outcome=closed`。

## 资源与失败行为

文件日志使用单独 writer 线程和 256 条有界队列；业务线程不等待磁盘 I/O。
每条序列化记录最多 64 KiB，若文件上限更小则以文件上限为准。
队列满或写入失败时记录丢弃数量，stderr 提示，之后写 `logging.dropped`；
不阻塞业务、不声称日志绝不丢失。正常刷新会处理待报告数量；进程崩溃可能丢失队列中记录。
每 100 ms 尝试刷新缓冲；正常 Ctrl-C 停止会显式刷新，磁盘错误由 stderr 和 flush 错误报告。
轮转不是持久数据库，也不保证跨重启累计统计。

SDK/嵌入模式的 `EventLog::default()` 不输出日志；可注入 `with_writer` 捕获事件，
或显式使用 `standalone` 配置文件日志。`with_writer` 是同步 writer，吞吐取决于注入目标。

## 实现与验证

`crates/server/src/events.rs` 负责身份关联、筛选与计数；`events/request.rs` 负责工具参数和结果允许列表；
`events/file.rs` 负责有界队列、私有文件、独占锁和轮转。
事件单元测试验证凭据/正文排除、过滤、计数、UTF-8 截断、轮转与溢出报告。
`tests/standalone.rs` 启动真实 rexed，验证同名用户的多客户端与另一用户、文件/搜索/任务/上传、
stdout 与审计分离、TLS/WSS 成功及失败认证。

## 相关文档

- [认证与用户配置](authentication.md)
- [会话与运维](modules/management.md)
- [网络恢复](network-recovery.md)
- [文档索引](README.md)
