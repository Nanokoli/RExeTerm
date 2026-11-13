# 会话与运维

## 范围

`rexed` 登记连接到本节点的 RE 客户端会话、程序和活动数据通道。
独立的只读管理入口向 `rexctl` 提供快照。业务请求、文件、PTY 和转发仍共用业务入口，
管理入口只接受 `re.admin.v1.snapshot`，不会接入工具或文件流。

## 内部结构

| 实现位置 | 任务与职责 | 持有状态 |
| --- | --- | --- |
| [protocol/management.rs](../../crates/protocol/src/management.rs) | 客户端身份、会话绑定、运维快照与默认 socket 路径 | 无运行状态 |
| [client/session.rs](../../crates/client/src/session.rs) | 建立会话、绑定业务链路、心跳与关闭 | 绑定凭据、端点通道池与心跳任务 |
| [server/registry.rs](../../crates/server/src/registry.rs) | 会话租约、链路统计归属和活动通道登记 | 最多 1024 个活动会话及通道记录 |
| [server/management.rs](../../crates/server/src/management.rs) | 独立监听、协议校验和快照组合 | 监听任务、最多 128 条管理连接及 socket 清理信息 |
| [client/management.rs](../../crates/client/src/management.rs) | 纯管理客户端 | 一条独立管理 Link 和请求序号 |
| [admin_view.rs](../../../rexeterm/crates/cli/src/admin_view.rs) | 五页过滤、归属与速率呈现 | 只消费快照 |
| [rexctl.rs](../../../rexeterm/src/bin/rexctl.rs)、[ui.rs](../../../rexeterm/src/bin/rexctl/ui.rs) | CLI、轮询、分页导航与终端恢复 | 当前页、滚动位置与最新快照 |
| [server/events.rs](../../crates/server/src/events.rs) | 事件身份关联、计数、stdout 和文件审计 | 有界会话缓存、文件队列和 writer |

### RE 用户与客户端

`username` 是 RE 用户身份，允许多个客户端使用相同值。正式 rexed 将其绑定到已认证账号。资源归属主键为持久 `clientId`，
`sessionId` 只标识当前绑定会话。任务和 PTY、文件、转发通道保存创建者的客户端 ID 与会话信息。
普通业务入口的任务和 PTY 列表、详情、输入、停止与移除仅接受当前客户端的资源。
已知 PTY ID 时，同一个 RE `username` 的其他客户端可以只读附着或无输入 attach 观察实时输出，
但不能输入、resize、申请主控或自动继任；不同 RE 用户仍不可查看。全局列表仅由独立运维入口提供。

`client_resources` 返回当前客户端的程序和活动通道，服务端根据 `clientId` 重建结果，
客户端不需要保存任务 ID 才能找回任务。该接口不返回其他客户端、恢复凭据或会话密钥。
同一用户名、相同 clientName、不同恢复凭据仍是不同客户端。

```bash
rexeterm-cli --target unix:///private/rexeterm/instance.sock connect node
rexeterm-mcp --re-user alice --client-name editor --no-local
```

`connect_to_executor` 可用 `username`、`clientName` 覆盖端点标签；Rust 使用
`Client::with_identity(ClientHello)`。身份字段不能为空、不能超过 128 字节或包含控制字符。
`list_executor` 返回公开 `clientSession` 信息与能力标志，不返回密钥。

### 持久身份与重连

SDK 通过 `ClientCredentialStore` 获取每个端点作用域的恢复凭据。默认 store 只在本 Client
及其克隆之间共享内存；应用可注入自己的持久存储。RexETerm 在偏好文件旁保存私有
`*.identities.json`，不把凭据写入偏好查询、MCP 工具结果或日志。Unix 文件权限为 `0600`，
写入通过文件锁协调，并用临时文件原子替换。独立客户端应使用不同的配置目录；共享身份文件
表示恢复同一个逻辑客户端，不能用它来区分两个客户端。

恢复凭据是随机 256 位秘密，`clientId` 为该秘密的 SHA-256 标识。
`re.client.v1.open` 校验秘密与标识的对应关系；仅更改公开 ID 不能冒领其他客户端。
宽限期内重连恢复原会话、程序及活动通道归属，累计链路流量继续合计。
已断开的物理文件/转发流需要重新建立；恢复列表不会自动重放传输字节或重新创建本地监听器。
服务端运行状态不写数据库，重启服务端不会恢复已经停止的任务或物理连接。

资源恢复凭据与账号认证分开；正式 rexed 的每条链路先经过[认证](../authentication.md)，
再校验资源会话所属用户名。可信嵌入模式可不启用认证。
程序和目标连接以启动 `rexed` 的操作系统账号执行，RE 不读取系统用户列表。

### 租约与通道生命周期

SDK 每 10 秒通过 Control 通道池发短心跳。服务端默认在最后一次有效心跳、绑定或恢复之后
60 秒没有续约时判定掉线；短查询连接正常关闭不会触发客户端掉线。
数据链路仍在传输也不能阻止超时。超时后撤销绑定凭据、终止该客户端仍在运行的任务，
并取消 PTY、上传、下载和转发处理，释放通道、文件暂存和写锁。其他客户端不受影响。
退出任务移出活动列表，原生资源释放；终态只有短期有界回执，见[生命周期](../lifecycle.md)。

启动设置文件可覆盖超时，值必须是非零的 32 位秒数；与 SDK 默认心跳配合时应大于 10 秒：

```json
{
  "version": 1,
  "clientDisconnectTimeoutSeconds": 60
}
```

嵌入者可用 `Executor::with_client_disconnect_timeout(Duration)` 配置。
所有监听及注入 Link 自动启动每 250 ms 一次的清理，`prune_client_sessions` 也可主动执行。
清理按超时工作，不依赖运维轮询。设置文件在启动时读取。

显式 `Client::shutdown().await` 停止本地转发并等待会话关闭响应，立即结束所属运行资源；
MCP stdio EOF、App 或普通 HTTP 宿主最终退出调用此路径；普通 CLI 退出只断开到宿主的连接。丢失进程、只析构句柄、网络中断或关闭失败进入上述宽限期。
关闭单个 App MCP HTTP 适配器不会关闭共享 Client。

PTY、文件、转发使用 RAII 登记并在取消时清理。查询只计入链路流量，不产生长期通道行。
转发页显示已经建立的转发流，本地尚无连接的监听器不登记为服务端活动流。
业务入口只允许未绑定的 `_executor_info` 探测；其他请求必须先绑定会话。
旧的 PTY admin 协议同样按客户端过滤。没有 `clientIsolation` 的旧服务端不支持这些保证，
新 SDK 拒绝向其发出 exbash 和资源列表调用，提示升级。

### 标题与描述

任务、文件与转发共用标题和描述约束。标题默认“未命名”，最多 40 个 Unicode 标量值；
描述默认空字符串，最多 200 个。空白标题归一为“未命名”，长度在归一前验证。
服务端验证创建任务的顶层 `title`、`description` 和文件/转发绑定的 `metadata`。
MCP 的 `exbash`、`file_transfer`、`forward` 将顶层元数据原样传入。

stdio 与 PTY 共用任务元数据。创建、查询、列表、attach、停止与首次等待内完成的结果
包含 `title`、`description`；`client_resources.programs` 和运维 `programs` 也包含这两个顶层字段。
PTY 的列表和详情复用任务标题，不提供独立命名接口，原始 PTY 数据帧不附加标题。
`rexctl` 程序页显示任务标题与描述，文件和转发页显示通道元数据。

PTY 的元数据由服务层 `ShellManager` 持有，stdio 的元数据保存在任务状态中；
移除、客户端租约回收和短期回执过期会一并清理，服务端不保留长期任务历史。
旧服务端返回中缺少标题、描述时，协议解码分别使用“未命名”和空字符串。

### 程序与 PTY 主控

程序记录包含 ID、命令、PID、工作目录、创建时间、创建用户、客户端及会话，
以及当前主控 PTY 客户端和控制会话。创建者固定，控制者只能在同一客户端归属内切换。
任务在掉线宽限期内继续运行，重连可恢复访问；宽限期结束则强制停止。
运维程序页只展示服务端仍维护的活动程序，不扫描系统进程，也不保存长期退出历史。
`exbash` 首次等待内已完成的运行自动移除，显式 `remove` 同样删除记录。
启动主终端没有客户端创建者，仅供可信进程内接口和运维查看。

## 五个页面

| 页 | 快捷键 / CLI 参数 | 内容 |
| --- | --- | --- |
| 客户端 | `1` / `clients` | RE 用户 → 客户端树；每客户端 RX、TX、TOTAL、活动链接数、对端与创建时间 |
| 程序 | `2` / `programs` | 全局维护的运行记录；创建归属与当前 PTY 主控归属 |
| PTY 通道 | `3` / `pty` | 全局活动终端附着和正在执行的 Exbash attach |
| 文件通道 | `4` / `files` | 全局活动上传与下载 |
| 转发流 | `5` / `forwards` | 全局活动转发流及目标 |

后四页各自展示全局列表，每条记录标明归属。它们不嵌在客户端树下。
TTY 中默认进入 TUI，可用 `1`～`5`、Tab、左右键切页，上下键或翻页键滚动，
`q`、Esc、Ctrl-C 退出。退出或错误会恢复终端模式、光标与屏幕。
网络读取与按键处理独立，等待服务端时仍可退出；管理请求失败后需要重新连接。

### 客户端全部链路速率

客户端行直接显示该会话所有已绑定业务链路的合计速率，包括控制、短查询、PTY、文件和转发。
RX 是服务端收到，TX 是服务端发出，TOTAL 为两者之和。用户树节点再合计所属客户端。
单位为十进制 `MB/s`，`1 MB = 1,000,000` 字节；数值按 100 ms 桶统计最近约 1 秒的流量。
JSON 同时保留接收、发送累计字节数与每秒速率；链接关闭后累计量仍保留到客户端租约移除。

计数器位于真实 `AsyncRead` / `AsyncWrite` 边界，低于 REX1 或 WebSocket 封装。
TCP / Unix / 注入字节流包含 REX1 头；服务端 WS 包含 HTTP Upgrade、WS 帧和 Ping/Pong。
绑定时补计该链路之前的握手字节，以后实时计入所属客户端。
统计不按文件进度、PTY 内容或工具响应长度估算，也不包含独立管理连接。

统计边界是适配器可见的链路 I/O，并非网卡线速：不含 TCP/IP 头、TCP 重传和物理开销。
可选 KCP 当前统计可靠流 I/O 与 REX1，未包含内部 UDP 重传或 ACK。
直接构造 `Link::new` / `from_websocket` 的自定义适配器应通过 `with_traffic` 注入下层
`TrafficMeter`；缺失时 `trafficComplete=false`，界面标明 `partial counters`。
传输层只维护计数，不认识用户名或会话；归属和汇总由服务端注册表负责。

## 独立监听与使用

Unix 上，独立 `rexed` 默认只开管理本地套接字：

- 设置了 `XDG_RUNTIME_DIR`：`$XDG_RUNTIME_DIR/rexed/<id>.admin.sock`。
- 否则：`$HOME/.cache/rexed/<id>.admin.sock`。

新建目录权限为 `0700`，socket 为 `0600`。不覆盖现有路径，正常关闭只清理自己创建的
socket。默认 ID 限 1～48 个字母、数字、`-`、`_`、`.`；其他 ID 需显式配置路径。
非 Unix 平台需要显式设置 TCP / WS。库和客户端组合的本地执行器不自动开管理入口。

```bash
# 默认管理 Unix socket；rexctl 使用同一 ID 定位。
rexed --insecure-no-auth --id node --listen tcp://127.0.0.1:9001
rexctl --id node
rexctl --id node --no-tui programs
rexctl --id node --json
rexctl --id node --json files
rexctl --id node --watch --interval 1 clients
rexctl --id node --username alice --no-tui clients

# 显式独立 socket。
rexed --insecure-no-auth --id node --listen tcp://127.0.0.1:9001 --admin-listen unix:///tmp/node.admin.sock
rexctl --target unix:///tmp/node.admin.sock --no-tui pty

# TCP / WS 省略端口时，使用业务实际端口 + 1，即 9002。
rexed --insecure-no-auth --id node --listen ws://127.0.0.1:9001/re --admin-listen ws://127.0.0.1/ops
rexctl --target ws://127.0.0.1:9002/ops --no-tui forwards
```

运维监听可持久保存到 `rexed-config/server.json` 的 `adminListen`；
显式 `--config-dir` 则读取该目录内的文件。例如：

```json
{
  "certificate": "server-cert.pem",
  "privateKey": "server-key.pem",
  "adminListen": "tls://127.0.0.1:45958"
}
```

选择顺序为 `--admin-listen` → `server.json.adminListen` → 平台默认值。
命令行覆盖只影响本次启动，不写回文件。旧文件省略此字段或使用 `null` 时维持默认行为；
空字符串、类型错误或无效的已选地址会阻止启动。文件仅启动时读取，修改后需要重启。
`adminListen` 接受与命令行相同的完整地址，包括 `unix:///绝对路径` 和 `wss://地址:端口/路径`；
网络运维仍要求 TLS/WSS、账号认证和 `management` 权限。

管理协议、地址、端口和 WS 路径可独立设置。业务端口为 `0` 时使用实际分配端口 + 1；
超出 65535、业务为非网络链路或邻接端口占用时需显式指定管理端口。
管理入口支持 Unix、TLS/WSS；可信开发模式也支持 TCP/WS，不支持 KCP。
网络管理要求认证与 management 权限；`rexctl --connection-profile /private/admin.json`
复用认证指南中的 username/authentication/tls 配置。私有 Unix 管理依赖套接字文件权限。

`--json` 不指定页面时返回完整快照，指定页面时返回对应数组。
`--username` / `--session` 过滤会话与通道；程序按创建者过滤，当前主控归属仍单独显示。
`--tui` 强制交互，`--no-tui` 单次输出，`--watch` 连续打印；默认刷新间隔为 1 秒。

## 服务端 stdout 与文件日志

独立 `rexed` 的 stdout 只输出客户端连接、恢复和断开，包含 RE 用户、客户端、IP 和时间。
详细操作审计默认写入程序旁 `.rexed-logs/`，启动与错误写 stderr。
日志粒度、类别、字段、请求计数、文件轮转和溢出行为见[服务端日志](../logging.md)。

TCP/TLS/WS/WSS 业务与网络管理监听会在 accept 错误后按原地址和端口重试绑定，
不会因一次错误永久结束监听。会话租约与物理流恢复见[网络恢复](../network-recovery.md)。

## 验证

[归属与超时测试](../../tests/ownership.rs) 验证同名用户列表和控制隔离、跨用户 ID 拒绝、恢复凭据、
持续连接撤销、任务终止和三类通道回收；[客户端恢复测试](../../../rexeterm/tests/ownership.rs)
验证私有凭据持久化及 MCP 标题透传。[管理集成测试](../../../rexeterm/tests/management.rs) 覆盖同名用户的多客户端、短查询、会话绑定校验、
文件和转发归属、创建者保留、PTY 主控切换、独立 WS 路径、Unix 权限与清理、
实际 `rexctl`；[执行端进程测试](../../tests/standalone.rs) 覆盖 rexed 和服务端 stdout 事件。
[链路计数测试](../../crates/transport/src/traffic.rs) 校验封装字节、WS 握手与自动 Pong、
多链接汇总、关闭后累计量及闲置速率归零。

## 相关文档

- [文档索引](../README.md)
- [设计总览](../design.md)
- [Client](client.md)
- [Executor](executor.md)
- [传输层与数据层](transport.md)
- [原生终端运行时](terminal.md)
- [构建与 API 迁移](../build-and-api.md)

## 管理权限拒绝

独立管理端完成认证后检查 management 权限。已认证但无管理权限时，尝试返回
`re.admin.v1.error`，消息为 `management permission denied`，随后关闭连接，不返回任何全局快照。
客户端遇到旧服务端直接关闭连接时仍应视为读取失败并清除旧数据。
