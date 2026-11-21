# CLI 本机 IPC

## 范围与使用

只需 `rexeterm`，不再需要附属的宿主可执行文件。后台创建或前台启动：

```bash
rexeterm service create --local
rexeterm info
rexeterm service list
rexeterm service stop
rexeterm service start
# 另一种运行方式：先 stop，再在前台运行已选宿主。
rexeterm --daemon
```

`daemon` 与 `--daemon` 等价；前台模式持续运行，Ctrl-C 或另一个 CLI 的 `service stop` 可关闭。后台模式独立于调用它的终端。默认不开本地执行器；`--local` 需要构建时启用 local。

## 文件布局

```text
<二进制目录>/.rexeterm-socket/    # 或 --registry-dir DIR
  <UUID64>.sock                 # Unix；Windows 使用内核命名管道
  <boot-id>/
    <UUID64>.json               # 宿主状态目录、活动端点、PID
    <UUID64>.token              # 每次 daemon 启动更新
    registry.lock
    transfers-*/                # 有界暂存区
<当前 cwd>/
  RexETermHostUUID.txt          # 当前目录选用的宿主 UUID64
  .rexeterm-config/             # 宿主私有锁、日志、应用工作区；可通过 --config-dir 另选
  .rexeterm/config-dir/         # CLI 保存的执行器、凭证链接和偏好
  .rexeterm.disable             # 可选禁用标记
```

Unix 私有目录 0700、记录/token/socket 0600；Windows 使用当前用户私有 ACL。不可写时明确报错。
`service create` 启动或复用指定状态目录的宿主，写入当前目录绑定；未绑定的前台 `daemon` 也会写入。`service bind UUID64` 选择同一 registry 里的一个已有服务，可选已停止的服务。UUID64 只在当前 OS 启动期有效。

显式 `--service-id` 优先，不读或改写当前目录的绑定。`--registry-dir` 只控制 IPC 映射；如果绑定的 ID 不在所选 registry 里，先核对 `service list`。读取绑定时兼容同目录的旧 `.rexeterm/binding`，不搜索父目录或 registry 的旧 `binding`/`uuid`。旧的共享绑定可通过 `service list` 找到服务后，逐项目用 `service bind UUID64` 导入。手工编辑之前可用 `service info` 查看确切来源。

`info`、`service info/list`、`executor list/status`、本地执行器与凭证列表不启动宿主。业务调用可以重启已经绑定的服务；未绑定时显式创建或选择。重启后需重新连接远端执行器。

共享目录有 `.rexeterm.disable` 时，从子项目传入指向共享目录的 `--registry-dir` 也会被拒绝。
请使用项目自己的 registry（例如 `.rexeterm/runtime`），并将宿主状态留在项目目录中。
标记的具体作用范围见[目录禁用标记](modules/cli.md#目录禁用标记)。

## 协议

Linux 使用 Unix socket，Windows 使用 `\\.\pipe\rexeterm-<UUID64>` 命名管道。
管道设置 `reject_remote_clients`，地址解析不接受远程主机名；两端都无 TCP/HTTP 回退。
这仅定义 CLI 与 daemon 的本机通信，daemon 到远端 RE 继续使用用户选择的认证传输链路。
显式开启的本地执行器仍使用其既有内部 RE 链路。

每个连接承载一个请求和一个响应：8 字节魔数 `52 58 54 49 01 00 00 00`、
4 字节 little-endian 长度、UTF-8 JSON 控制消息。请求最大 256 KiB，响应最大 8 MiB，
分配前校验长度；不含 HTTP 头或 URL 路由。普通文件传输通过共享文件系统；按需返回的 PTY 截图和原始字节在这里使用 Base64。
请求携带 `serviceId`、私有 `token`、`operation`、`args`、可选 `key`。
宿主先检查身份和 token，再分发业务；响应包含 `serviceId`、`result` 或 `error`。
同一 daemon 最多同时处理 32 个连接，初始读取和响应发送分别有 5 秒预算。

普通 CLI 请求仍有全局截止时间；超时不撤销已经受理的业务操作。
显式幂等 key 保存最多 128 项、10 分钟，同 key 不重复执行不同请求；
未完成/取消的请求保留未知结果占位，超过重试缓存预算的结果需要查询资源。
单项缓存结果最多 64 KiB；完整首次响应仍受 8 MiB 上限约束。

## 业务与文件

`info`、`capabilities`、`hosts.*`、`tasks.*`、`forwards.*`、`preferences.*`、
`resources`、`transfers.*` 和 `rpc` 复用共享 Application/AppCore。

`hosts.connect_chain` 接受最终在线 `id` 和 `hops` 数组，每项是已解析的 SDK 连接选项（`id` 为保存的主机名、`url` 为连接地址）。daemon 用普通的 RE 连接和 TCP 转发逐跳建立，每跳有独立的 60 秒建连上限。成功时返回按顺序排列的 `executors` 和 `forwards` ID。中途超时或失败会回收本次已建立的资源；通过 `hosts.disconnect` 断开链内任一在线 ID 会关闭整条链。

`forwards.start` 要求 `host`（出口的在线 ID）和 `options`。`forwards.list` 默认列所有转发，`forwards.stop` 按 `id` 定位；两者可选 `host` 限定出口。停止跳板链占用的转发时，daemon 也会关闭该链。

`tasks.read` 入参为 `host`、`id`，可选 `wait_ms`、`from_start`、`since`。
stdio 默认读取游标之后的文字；PTY 默认返回全屏文字快照，多次读取不会消耗屏幕。
`from_start` 或 `since` 读取最近 256 KiB 保留的字节范围，返回
`output.text` 和 `metadata` 中的 `from`、`next_offset`、`buffered_bytes`、`dropped_bytes`。

对 PTY，`tasks.read` 或 `rpc` 的 `args` 可另外传 `terminal: {"tui":true,"png":true,"raw":true}`。
只返回选中的字段：`output.ansi` 是彩色快照；`output.pngBase64` 是同一屏幕的 PNG；
`output.rawBase64` 是本次选定的保留字节范围。三个字段均由 daemon 生成；
CLI 负责显示或保存到调用者指定的路径。`metadata.terminalOutput.capture` 标明从启动接入还是从中途观察。
`tasks.forget` 同时丢弃该任务的屏幕和回放。
高级操作示例：

```bash
rexeterm api info
rexeterm api hosts.list
rexeterm api rpc --body '{"tool":"list_executor","params":{}}'
```

`api` 不再接受 HTTP 方法和 URL。`--body` 或 `--input` 提供 JSON 对象，
`--output` 将 JSON 结果保存为新文件。日常命令默认输出单次 Markdown。

CLI 和 daemon 位于同一机器、使用同一系统账户，文件操作传递绝对本地路径。
普通上传下载由 daemon 直接执行 RE 文件传输，避免在本机 IPC 再复制一遍内容。
分步暂存保留 256 MiB 单文件、512 MiB 总量、64 项及 10 分钟回收限制。
暂存上传从本机文件复制并核对声明大小，限时 120 秒；暂存下载完成后由 CLI 从共享文件系统
复制到目标旁临时文件，成功后原子发布，默认拒绝覆盖。

## 相关文档

- [Agent CLI](agent-cli.md)
- [CLI 模块](modules/cli.md)
- [配置布局](client-config.md)
- [可选 HTTP 适配器](http-api.md)
