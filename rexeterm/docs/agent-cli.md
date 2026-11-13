# Agent CLI

## 范围

`rexeterm` 每次调用返回一份 Markdown 结果后退出，业务命令无交互终端、stdin 读取、
持续 PTY 输出或 SSE 订阅。先按 [CLI 宿主规则](cli-ipc.md) 绑定 UUID64，
各次调用复用常驻宿主的 AppCore、RE 客户端身份、任务与转发。
`daemon`（或 `--daemon`）是前台常驻模式，不受单次调用截止时间限制。
只分发一个 CLI 二进制；后台创建会自行启动同一个程序。默认绑定由当前目录的 `.rexeterm/binding` 选择，`service create` 自动写入；宿主映射位于程序旁 `.rexeterm-socket/`。
CLI 默认从当前目录的 `.rexeterm/config-dir` 启动宿主；App 读取自身二进制旁的同名目录。
见 [客户端配置布局](client-config.md)。

成功结果写 stdout，退出码为 0。请求失败将一份 Markdown 错误写 stderr，退出码为 1；
参数错误为 2。`--help`、`--version` 是普通文本。远端任务的 `exitCode` 是结果字段，
与 CLI 请求是否成功分开；例如远端退出 7，成功取得其结果的 CLI 仍退出 0。
输出文本放在 `___` / 类型 / `___` 分隔的块里：分隔符是比内容中任何一行更长的连续 `_`，
因此 payload 里的文本永远无法提前结束一个块（也就不存在围栏嵌套问题）；`_` 在 Markdown 里渲染成分隔线，但不会像 `---` 那样把类型行变成 setext 标题。终端控制字符不会直接发送到终端。

全局 `--timeout-ms` 默认 30000，上限 600000，限制本次命令总等待时间。
超时或中断不撤销宿主已受理的操作，重试写操作前先查询任务、传输或转发。
`api --idempotency-key` 可用于需要显式重试控制的 IPC 操作。

## 命令与能力

业务命令的 `HOST` 是宿主中的执行器 ID。`host` 管理保存的连接资料；
`executor` 管理实际连接，可通过本地 JSON 选项文件传入 SDK 的 TLS、认证、多链路和通道设置。
选项文件由 CLI 读取；其中证书等路径由宿主解析。认证密码不要写在命令行里。

| 命令 | 对应能力 |
| --- | --- |
| `daemon` / `--daemon`、`service create/list/start/stop`、`bind` | 单一二进制宿主与 cwd 绑定 |
| `info`、`capabilities` | 宿主版本、接口与预算 |
| `host list/show/add/edit/remove/connect/disconnect/status` | 保存的 Host 与连接生命周期 |
| `executor list/info/connect/default/disconnect` | `list_executor`、`_executor_info`、`connect_to_executor`、`set_default_executor` |
| `task list/run/status/read/input/input-file/close-stdin/stop/remove/forget/history` | Exbash 生命周期和客户端完成历史 |
| `read`、`file read/stat`、`stat` | 文本/二进制/目录读取、分页、可选哈希和元数据 |
| `glob`、`rg` | 文件查找、内容搜索、过滤和数量限制 |
| `file create/delete/rename/patch` | `FileAction` 四种操作、文本/二进制补丁、可选 hashCode 校验 |
| `upload`、`download` | CLI 文件与远端文件间传输，一次返回最终状态 |
| `transfer create/list/show/put/get/cancel` | 分步暂存传输，跨 CLI 调用查询与取消 |
| `transfer host-file` | SDK `file_transfer`，宿主本地路径、offset 续传、SHA-256 |
| `forward list/start/stop` | 宿主创建和维持本地 TCP 转发，CLI 退出后继续工作 |
| `resources` | 当前宿主客户端的远端程序及活动通道 |
| `preferences show/shells/set-shell/reload` | 客户端 shell 偏好，支持 HOST 与精确工作目录覆盖 |
| `api OPERATION` | 高级原生 IPC 调用，参数 JSON，结果 Markdown |

保留 `hosts`、`connect`、`disconnect`、`tasks`、`run` 的简写。
`pty HOST TASK` 是有限 `task read` 的兼容别名，不再打开交互窗口。
具体参数用 `rexeterm <命令> --help` 查询。

## Exbash 的两种 I/O

`run` 默认 `--io stdio`，通过管道执行，无终端回显、VT 解析或换行转换，
分别显示 `stdout`、`stderr`、`exitCode`。`--io pty` 使用真实终端，适合交互程序。
`--direct` 表示直接执行可执行程序；否则命令通过宿主的 shell 偏好执行。
I/O 类型与直接执行/shell 执行是两个独立选项。

```bash
rexeterm run node 'printf hello; printf error >&2; exit 7' --title '构建检查' --description '验证输出与退出码'
rexeterm run node 'python3 -V' --direct
rexeterm run node 'cat' --stdin 'hello'
rexeterm run node 'cat' --keep-stdin --wait-ms 0
rexeterm task input node <TASK> 'hello'
rexeterm task close-stdin node <TASK>
rexeterm task status node <TASK>
rexeterm run node 'bash' --io pty --wait-ms 0
rexeterm task read node <TASK> --wait-ms 1000
rexeterm task read node <TASK> --from-start
rexeterm task read node <TASK> --since 4096
rexeterm task stop node <TASK>
```

`--wait-ms` 默认 1000，可取 0..10000，控制本次读取时间，不限制任务寿命。
`--task-timeout-ms` 才限制远端程序总寿命。未完成的调用返回 `asyncID`。
`run` 和 `task run` 在 stdio、PTY 模式均支持 `--title`（40 个 Unicode 字符）与
`--description`（200 个 Unicode 字符），默认“未命名”和空字符串。
标题和描述由 rexed 保存并返回；CLI 的 Markdown 结果与宿主完成历史会保留这些字段。
stdio 默认关闭 stdin，`--stdin` 发送字面文本后关闭；`--keep-stdin` 允许后续输入。
`task input` 的文本按字面发送，包括实际换行；不会把 `\n` 两字符解释为换行。
`input-file` 读取远端文件，沿用 RE 4 KiB 输入限制。

`task read` 读取 **客户端缓冲**，不依赖远端是否还留着输出：`run` 返回仍在运行的 `asyncID` 后，
daemon 立刻挂上一个后台 `attach` 订阅（每 1 秒一次、无输出时退避 200 ms），持续把输出追加进缓冲，
直到任务停止、`task stop` 或 `task forget`；任务停止后 `attach` 本身不再返回数据，所以采集必须发生在运行期间。
daemon 为每个任务保留最近 256 KiB 输出和单调递增的字节偏移，默认从上次读取的游标续读；`--from-start` 从头重放整段缓冲，
`--since <offset>` 从指定偏移开始（偏移取自上一次结果的 `from`/`next_offset`）。
任务结束后缓冲仍然可读，直到 `task remove` 或 `task forget`。
缓冲溢出时结果里的 `dropped_bytes` 给出被丢弃的字节数，`buffered_bytes` 是当前保留量。
stdio 任务的缓冲按到达顺序合并 `stdout`/`stderr`；pty 任务保存终端流（含回显，`terminalOutput`
给出 `cols`/`rows`/`format`）。缓冲只保存文本流，不落盘；需要超过 256 KiB 的完整日志时，仍应让程序写远端文件并通过
`read/download` 读取。`run`/`task input`/`pty` 仍只返回本次调用期间的输出。
单次 stdio 合计收集最多 1 MiB；慢消费者或超限时显示 `outputTruncated`/`droppedChunks`。
文本按 UTF-8 有损解码；二进制结果应写文件传输。
`task list` 仅包含此客户端的活动任务和宿主保存的完成记录；运维全局视图属于 `rexctl`。
`task remove` 停止/移除远端任务并删除宿主完成记录；`forget` 只删除宿主完成记录。

## 文件和转发示例

```bash
rexeterm host add node tls://re.example.test:45957 --root /home/alice \
  --username alice --connection-profile /private/alice.json
rexeterm connect node
rexeterm read node /home/alice/readme.txt --offset 1 --limit 100
rexeterm glob node '**/*.rs' --path /home/alice/project
rexeterm rg node 'TODO' --root /home/alice/project --glob '*.rs'
rexeterm file create node /tmp/example.txt --content 'hello'
rexeterm file rename node /tmp/example.txt /tmp/renamed.txt
rexeterm file patch node /tmp/renamed.txt ./change.patch
rexeterm upload node ./data.bin /tmp/data.bin --title '样例数据'
rexeterm download node /tmp/data.bin ./copy.bin
rexeterm forward start node 127.0.0.1:8080 127.0.0.1:80 --title '开发服务'
rexeterm forward list node
rexeterm preferences set-shell node sh --rcwd /home/alice/project
```

普通 `upload/download` 将绝对本地路径交给 daemon，直接复用 RE 文件通道，单文件 256 MiB。
下载先写本地临时文件，完成后原子发布；默认拒绝覆盖，`--overwrite` 显式允许。
暂存传输的列表、状态、取消使用 `transfer`。`host-file` 的 local-path 属于宿主，
该直接 SDK 模式同步返回，不进入 HTTP 暂存列表，也没有暂存任务 ID。
文件传输的标题和描述分别最多 40 与 200 字符，默认“未命名”和空。
RE 文件字节始终走统一文件通道，不额外开启文件 HTTP 服务。

`api OPERATION` 直接调用本机 IPC 操作，不接受 HTTP 方法或 URL。`--output FILE` 保存 JSON 结果并拒绝覆盖。
查询任务、资源、传输和转发快照即可获得单次状态。
当前转发实现为 TCP；没有据此增加 UDP、KCP 转发或 SSH/SFTP 服务。

## 相关文档

- [HTTP 与宿主接口](http-api.md)
- [CLI 模块](modules/cli.md)
- [Exbash 服务端](../../RemoteExecutor/docs/modules/exbash.md)

## 当前目录禁用

在不希望 Agent 使用 CLI 的目录执行：

```bash
rexeterm disable
```

命令在当前工作目录创建 `.rexeterm.disable`，不需要绑定宿主。标记存在时，
CLI 在读取注册表、配置或连接宿主之前退出，状态码为 `1`，标准输出为空，
标准错误原样显示：

```text
Exit due to file [.rexeterm.disable]. Run in your project root, or set cwd to it, instead of the shared workspace.
```

使用 `rexeterm enable` 删除当前目录的标记，使用 `rexeterm --help` 查看帮助。
这两个指令在禁用状态下仍可使用。重复禁用不会截断已有文件，重复启用不会报错。
只检查当前工作目录，不搜索父目录；全局 `--service-id` 或 `--registry-dir` 不能绕过标记。
该标记只约束 CLI 调用，不会停止已启动的宿主、任务或其他适配器。

## 工作目录

CLI 不在参数里接受工作目录：`.rexeterm/`（`binding`、`config-dir/`、`../.rexeterm-out`）和目录层偏好都按进程 cwd 解析。需要换目录时用 `cd`，或设环境变量 `REXETERM_CWD=<dir>`（启动时切换，失败直接退出 2）。远端工作目录仍用 `--rcwd DIR`。

## 命令形状（当前）

顶层只剩 8 个命令，其余全是裸调用（不带子命令，靠 flag 推断）。

```
rexeterm [options]                        裸调用：-cmd / -A / -T / -F
rexeterm disable | enable                 .rexeterm.disable 开关，不需要宿主
rexeterm service  create|list|start|stop|info|bind
rexeterm info | capabilities
rexeterm executor list|status|connect|disconnect
rexeterm executor host list|add|edit|remove
rexeterm file     read|ls|stat|glob|rg|create|delete|rename|patch|upload|download
rexeterm preferences show|shell|shells|set-shell|reload
rexeterm api      <operation> [--body|--input|--output|--idempotency-key]
```

`executor` 有两种语义：

- `list` / `status` / `connect` / `disconnect`：连宿主，`-e` 指定执行器。
- `executor host …`：纯客户端数据，写 `<cwd>/.rexeterm/config-dir/executors/<id>.json`，
  不需要宿主；`executor connect -e <id>` 会把这个文件交给宿主建连。

裸调用：

```
rexeterm -e local -cmd '<command>'                 立即分离，等同于原来的 run
rexeterm -e local -s <SHELL> --stdio|-pty [COLSxROWS] -t <MS> -cmd '<command>'
rexeterm -e local -A                              任务列表：未读完成 → 运行中 → 已读完成
rexeterm -e local -A <ASYNCID> -status            任务状态
rexeterm -e local -A <ASYNCID> -read [-t <MS>]    读输出
rexeterm -e local -A <ASYNCID> -input <TEXT>      发文本
rexeterm -e local -A <ASYNCID> -input-file <PATH> 发远端文件
rexeterm -e local -A <ASYNCID> -stop | -remove
rexeterm -e local -A <ASYNCID> -png <FILE>|-raw <FILE>
rexeterm -e local -T [<TRANSFERID>] [-stop]       传输列表 / 详情 / 取消
rexeterm -e local -F                              转发列表
rexeterm -e local -F <PORT>:<HOST>:<PORT> -title -desc    新建转发
rexeterm -e local -F <FORWARDID> -stop            停转发
```

规则：

- `-e/--executor` 是唯一执行器开关；不带时只有真正要连执行器的动作才解析默认值。
- 已删除的子命令：`run`、`pty`、`task`、`tasks`、`transfer`、`forward`、`resources`、
  `hosts`、`host`、顶层 `connect` / `disconnect`。
- `glob` / `rg` / `ls` 的第二个位置参数是文件夹。
- 工作目录只能靠 `cd` 或 `REXETERM_CWD`，CLI 不接受参数改自身路径。
