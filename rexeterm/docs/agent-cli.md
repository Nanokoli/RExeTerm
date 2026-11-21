# Agent CLI 使用指南

`rexeterm`（有些发行包命名为 `rexeterm-cli`）每次调用默认输出一份 Markdown。后台进程 `rexetermd` 由同一个二进制启动，负责维护活跃连接、PTY 屏幕、任务输出和文件传输状态。以下命令适用于包含本文功能的构建；用实际调用的 `--help` 和 `info` 的 `buildId` 核对版本。

## 找到正确的宿主

从项目根目录执行下列命令。已有项目使用原先的 registry；下面的 `.rexeterm/runtime` 是新项目的独立示例。Bash 函数确保每次都带同一个 `--registry-dir`。

```bash
REXE=/absolute/path/to/rexeterm
REGISTRY="$PWD/.rexeterm/runtime"
rx() { "$REXE" --registry-dir "$REGISTRY" "$@"; }
"$REXE" --help
rx service list
rx info
```

`info` 和 `service info` 显示实际二进制、当前本机 cwd、registry、绑定来源、服务 ID、运行状态，以及 `-e` 选中的在线执行器或宿主默认执行器。它们不会启动宿主。已有服务可执行 `rx service bind UUID64` 将其绑定到本项目；也可单次传 `--service-id UUID64`。新项目执行 `rx service create`，只想在本机试用时执行 `rx service create --local`，内置执行器 ID 为 `local`。

本机 cwd（`cd` 或 `REXETERM_CWD`）决定客户端配置、宿主绑定和本地相对文件路径。`--rcwd DIR` 指远端工作目录。省略 `--rcwd` 时使用执行器的默认目录；`executor host add --root` 只记录该执行器的文件浏览起点。布局和迁移见 [CLI IPC](cli-ipc.md) 与 [客户端配置](client-config.md)。

如果报错指向共享根目录的 `.rexeterm.disable`，检查选用的 `--registry-dir` 和服务的状态目录。从子项目调用时也应选用项目自己的 registry 和状态目录。

## 选择执行器与凭证

准备已有权限的私有 JSON profile。证书和私钥保存在受限目录，例如团队的 `HostNet`；不需要再复制到项目。这里只使用示例域名和路径。

```bash
rx -e RB5 executor host add tls://re.example.test:45957 --root /home/user
rx credential link rb5-codex /private/HostNet/RB5/profile.json
rx -e -list_local
rx -cr -list_local
rx -ec rb5-codex@RB5
rx -e -list_online
```

`-ec` 是 `executor connect` 的简写。`rb5-codex@RB5` 分别选择本地凭证名和保存的主机名（执行器 ID）。执行器提供连接地址，凭证提供用户名、认证方式和 TLS 文件。省略 `as` 时，`@` 后的主机名就是在线 ID，所以上例在在线列表中显示 `RB5`。需要自定义在线 ID 时，执行 `rx -ec rb5-codex@RB5 as rb5_codex_exe`。也可以用 `rx -e RB5 -cr rb5-codex executor connect` 选择同一组配置，再用 `--as rb5_codex_exe` 指定别名。`-cr` 等同于 `--credential`。

`-e -list_local`（`executor host list`）和 `-cr -list_local`（`credential list`）分别显示保存目录和条目路径；凭证列表还显示链接目标和是否可读到文件。`-e -list_online`（`executor list --online`）只列选中宿主的在线连接。`executor list` 同时列在线连接和保存条目；两类名称可以不同。所有这些查询都不会启动宿主。

`credential link` 创建文件符号链接，默认保存到 `.rexeterm/config-dir/credentials/<名称>.json`。`credential unlink NAME` 只移除链接。Windows 创建符号链接需要系统权限；也可以使用指向私有文件的 `--connection-profile`。旧的 `executor host add … --connection-profile PATH` 仍可用；单次使用完整 SDK 选项则执行 `rx executor connect --options-file PATH`（含 `id`、`url`，显式 `-e` 覆盖文件中的 `id`）。选项文件与 `-cr` 选一种。

## 通过跳板连接

先保存每一跳的执行器和凭证链接。每对 `凭证@主机` 选择该跳的配置，`:` 按连接顺序隔开各跳。

```bash
rx --timeout-ms 210000 -ec Sk-a@HostA:Sk-b@HostB:Sk-c@Host-C as Cfba
rx -e -list_online
rx -f
```

| 步骤 | 在线 ID | 为下一跳开的转发 |
| --- | --- | --- |
| 直连 `HostA` | `Cfba_HostA_node1` | `Cfba_HostA_node1_Forward_node2` |
| 经 A 连接 `HostB` | `Cfba_HostB_node2` | `Cfba_HostB_node2_Forward_node3` |
| 经 B 连接 `Host-C` | `Cfba` | — |

第一跳必须能从本机到达，之后每跳的地址由上一跳解析并连接。中间执行器需要支持 RE TCP 转发；后续跳各使用一个 `tcp://`、`tls://`、`ws://` 或 `wss://` 地址。TLS 仍验证原地址中的主机名或凭证里的 `tls.serverName`。

每跳从准备转发到会话建立最多 60 秒，分别计时；某个连接阶段也可能更早超时。任一跳失败会终止建链，整个过程允许超过 60 秒。`--timeout-ms` 另行限制 CLI 等待整次请求的时间，默认 30000 毫秒；上例给三跳留出 210 秒。CLI 先超时退出时，daemon 仍可能在处理请求，可用 `-e -list_online` 和 `-f` 重新查看状态。

`executor connect` 同样接受多跳写法。省略 `as` 时，用最后一跳的保存名称（上例为 `Host-C`）作为最终在线 ID。daemon 原来没有在线执行器时，最终节点成为默认执行器。各跳和转发只存在于当前 daemon。任意一跳失败会清理本次已建连接；`rx -e Cfba executor disconnect` 断开整条链，通过它创建的转发也会关闭。停止链中的转发或断开中间在线 ID 同样会关闭整条链。

## 运行、等待、判断结果

```bash
LIVE=RB5  # 如果指定过 as，改用对应别名；内置执行器用 local
REMOTE_DIR=/home/user
rx -e "$LIVE" --cmd 'printf "hello\n"' --rcwd "$REMOTE_DIR" -t 1000
rx -e "$LIVE" --cmd 'sleep 12; printf "done\n"' --rcwd "$REMOTE_DIR" --task-ttl 60000
```

长任务会先返回 `asyncID`。保存它并沿用同一个 registry、宿主和在线 ID：

```bash
# 将 ASYNCID 设为上一步返回的值。
rx -e "$LIVE" -A "$ASYNCID" -t 1000
rx -e "$LIVE" -A "$ASYNCID" --status
rx -e "$LIVE" -A "$ASYNCID" --read
```

任务默认用 stdio。`-A ID` 从宿主维护的游标继续读 stdio；PTY 则显示当前整个可见屏幕。
`--read` 从头读取尚保留的输出（最多 256 KiB），也可用 `--since OFFSET` 从上次的 `next_offset` 继续。
`--status` 只查状态，`-A` 不带 ID 列任务，`-A ID --stop` 停止任务。需要完整日志时，让远端程序写文件后下载。

三类时限都用毫秒：

| 参数 | 控制什么 |
| --- | --- |
| `-t / --timeout` | 本次等待输出的时间，`0..=10000`，默认 `0`。继续等待用 `-A ID`。 |
| `--timeout-ms` | 这次 CLI 调用的截止时间，默认 `30000`。超时后先查询在线连接、任务、传输或转发状态再重试。 |
| `--task-ttl` | 远端任务生命期。 |

CLI 退出 `0` 表示调用成功；远端命令是否成功看结果里的 `exitCode`。它为空或显示 running 时继续查询；完成后 `exitCode: 0` 才表示远端程序成功。命令和初始 `--stdin` 各最多 4096 字节，标题 `--title` 最多 40 个 Unicode 字符，描述 `--desc`（`--description` 也可）最多 200 个。长命令改为上传脚本再执行。`--rcwd C:/work` 可用于 Windows 执行器；示例里的 shell 命令是 POSIX 用法。

## 查看和操作 PTY

`--pty` 默认创建 80 列 × 24 行的终端，也可以指定 `--pty 100x30`，最大 300 列 × 120 行。
同一个 `rexetermd` 持续接收原始字节，并维护光标、颜色和主屏与备用屏。CLI 退出后，下一条命令仍可读到这份屏幕。

```bash
rx -e "$LIVE" --cmd nmtui --direct --pty 80x24 -t 1000 --png initial.png
# 将 ASYNCID 设为上一步返回的值。Bash 的 $'...' 会传入真实按键字节。
rx -e "$LIVE" -A "$ASYNCID" --text $'\eOB' -t 500 --png selected.png
rx -e "$LIVE" -A "$ASYNCID" --tui
rx -e "$LIVE" -A "$ASYNCID" --read --raw session.bin
```

- 默认文字显示整个可见屏幕，`--tui`（或 `--sink tui`）直接输出带颜色的屏幕快照。用 `-A ID --text` 发按键；`--tui` 本身只打印一次。
- `--png FILE` 导出同一屏幕，保留 TUI 的颜色、边框和选中行。`--raw FILE` 保存最近 256 KiB 的终端字节、包括转义序列；指定 `--since` 则只保存从该偏移开始的部分。
- 新版 rexed 从启动第一字节就连续输出。旧版使用启动响应和之后的实时流，二者之间可能漏字节；启动响应里的非 UTF-8 字节也可能已被替换。后来才接入的任务只看到接入之后的输出。
- 屏幕和回放存在当前 daemon 的内存中，已结束任务最多保留 30 分钟；达到容量上限时先回收较早结束的任务。重新启动 daemon 后这些内容需要重新采集。

默认 `--sink text` 的内联上限为 65536 字节，可通过 `--text-limit` 调整。`--sink text,file` 也把任务块保存到
`--out-dir`（默认 `.rexeterm-out`）。`--sink raw`、`--sink png` 自动选文件名；`--raw FILE`、`--png FILE` 指定路径。
保存失败时任务可能仍在运行，可用 `-A` 查找任务 ID。

## 上传脚本、下载产物

上传和下载都把**远端路径放在第一个位置**。已有本地文件时需传 `--overwrite`；可用 `--sha256` 指定预期摘要，接受 64 位十六进制或 `sha256:` 前缀。下载不匹配时保留原有本地文件。

```bash
printf '%s\n' 'printf "result\n" > result.txt' > job.sh
SHA=$(sha256sum ./job.sh | cut -d' ' -f1)
rx -e "$LIVE" file upload "$REMOTE_DIR/job.sh" ./job.sh --sha256 "$SHA"
rx -e "$LIVE" --cmd 'sh ./job.sh' --rcwd "$REMOTE_DIR" -t 1000
rx -e "$LIVE" file stat "$REMOTE_DIR/result.txt"
rx -e "$LIVE" file download "$REMOTE_DIR/result.txt" ./result.txt
```

下载前确认任务已结束且 `exitCode: 0`。本机相对路径始终从当前 CLI cwd 解析；远端相对路径按 `--rcwd` 解析。传输成功时显示 `localPath`、`targetPath`、字节数和摘要。

## 端口转发

建立转发时，`@` 后填写当前 daemon 中的**在线执行器 ID**。下面让本机 `127.0.0.1:8080` 经 `Cfba` 访问 `10.0.0.7:80`：

```bash
rx -f 8080:10.0.0.7@Cfba:80
rx -f                         # 列出全部转发，含出口执行器和 forwardID
rx -f forward-1 --stop        # 使用上一步实际显示的 forwardID
```

`-f`、`-F` 和 `--forward` 等价。这三种操作都无需 `-e`。本地监听默认为 `127.0.0.1`，可加 `LISTEN_HOST:` 前缀选择监听地址；本机端口填 `0` 可自动分配。IPv6 地址加方括号，例如 `8080:[2001:db8::7]@Cfba:80`。目标地址从出口执行器所在的机器访问。

## 其它入口

- `rx api rpc --body '{"tool":"list_executor","params":{}}'` 调用原生 IPC 的 SDK 工具；带 `--output PATH` 可保存 JSON 到新文件。业务工具的目标可在 body 中指定 `executor`。
- `rx preferences shell list` 查询项目 shell 配置；运行时可加 `-s NAME`。`rx disable` 只对当前 cwd 写 `.rexeterm.disable`，`rx enable` 移除；用绝对路径读取 `--help` 仍可用。

遇到旧示例的 `run`、`task`、顶层 `connect` 或 `upload/download`，分别改用 `--cmd`、`-A`、`executor connect`、`file upload/download`；宿主绑定用 `service bind`。
