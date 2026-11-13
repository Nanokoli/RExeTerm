# CLI 本机 IPC

## 范围与使用

只需 `rexeterm`，不再需要附属的宿主可执行文件。后台创建或前台启动：

```bash
rexeterm service create
rexeterm daemon
rexeterm --daemon
rexeterm info
rexeterm service list
rexeterm service stop
rexeterm service start
```

`daemon` 与 `--daemon` 等价；`deamon` 作为拼写兼容别名。前台模式持续运行，Ctrl-C 或
另一 CLI 的 `service stop` 触发最终关闭。后台模式独立于调用它的 CLI 终端。
默认不开本地执行器；显式 `--local` 需要编入 `local` feature。

## 文件布局

```text
<二进制目录>/
  rexeterm[.exe]
  .rexeterm-socket/
    <UUID64>.sock              # Unix；Windows 使用内核命名管道
    <boot-id>/
      <UUID64>.json            # 配置目录、活动端点、PID
      <UUID64>.token           # 每次 daemon 启动更新
      registry.lock
      transfers-*/            # 有界暂存区，正常退出删除
<当前 cwd>/
  .rexeterm/binding         # 当前目录选择的宿主 UUID64
  .rexeterm-config/            # 配置、偏好、身份、有限任务结果、宿主日志
  .rexeterm.disable           # 可选的当前目录禁用标记
```

Unix 私有目录 0700、记录/token/socket 0600；Windows 配置和记录使用当前用户私有 ACL。
不可写时明确报错，不回退到临时目录或网络端口。`--registry-dir` 可以显式另选运行目录。
`service create` 自动生成或复用所选配置的 UUID64，启动宿主并写入 cwd 的 `.rexeterm/binding`；
未指定 ID 的前台 `daemon` 启动后也写入 cwd。后续普通调用只读取当前目录的绑定，不搜索父目录。
不同目录可以分别绑定不同宿主；`bind <UUID64>` 将已有宿主绑定到当前目录。
显式 `--service-id` 优先，不读取或改写绑定文件；`--registry-dir` 只迁移映射和私有运行记录。
缺少或无效的绑定会明确报错；绑定所属宿主不在本次系统启动的注册表中时，需要重新创建或绑定。
之前保存在 `.rexeterm-socket/` 下的默认绑定不再读取，但原文件保留；
可通过 `service list` 查找宿主，执行 `bind <UUID64>` 切换全机绑定（宿主唯一，binding 在 `.rexeterm-socket/binding`）；CLI 偏好固定在各项目 `.rexeterm/config-dir`，宿主不读取。

## 协议

Linux 使用 Unix socket，Windows 使用 `\\.\pipe\rexeterm-<UUID64>` 命名管道。
管道设置 `reject_remote_clients`，地址解析不接受远程主机名；两端都无 TCP/HTTP 回退。
这仅定义 CLI 与 daemon 的本机通信，daemon 到远端 RE 继续使用用户选择的认证传输链路。
显式开启的本地执行器仍使用其既有内部 RE 链路。

每个连接承载一个请求和一个响应：8 字节魔数 `52 58 54 49 01 00 00 00`、
4 字节 little-endian 长度、UTF-8 JSON 控制消息。请求最大 256 KiB，响应最大 8 MiB，
分配前校验长度；不含 HTTP 头、URL 路由或 base64 文件内容。
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

`tasks.read` 由 daemon 自己维护输出缓冲：入参 `host`、`id`，可选 `wait_ms`、`from_start`、`since`；
返回 `output.text` 和 `from`、`next_offset`、`buffered_bytes`、`dropped_bytes`。
缓冲为空时才向宿主发一次 `exbash attach`（`read_timeout=wait_ms`）并追加结果。
所有 `rpc` 回包中 `exbash` 的 `output.text/stdout/stderr` 会自动进入同一缓冲；
`tasks.forget` 同时丢弃该任务的缓冲。
高级操作示例：

```bash
rexeterm api info
rexeterm api hosts.list
rexeterm api rpc --body '{"tool":"list_executor","params":{}}'
```

`api` 不再接受 HTTP 方法和 URL。`--body` 或 `--input` 提供 JSON 对象，
`--output` 将 JSON 结果保存为新文件。日常命令仍输出单次 Markdown。

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
