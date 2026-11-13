# Exbash 与终端运行时

## 范围

Exbash 共用一套命令操作，提供 `io: "stdio"` 和 `io: "pty"` 两种 I/O。
未指定 io 时保持 PTY 兼容行为。`mode: run/shell` 选择直接程序或 shell，
与 I/O 类型无关；status/list/attach/stop/remove 沿用原模式。

## stdio 执行

`io: "stdio"` 通过操作系统管道执行，不分配终端。`metadata.stdout`、
`metadata.stderr` 分别返回本次捕获的 UTF-8 文本，`exitCode` 返回退出码；
Unix 信号退出使用 128 + signal。`timedOut`、`stopped` 区分超时和停止。
不提供终端尺寸、VT 屏幕、回显或终端换行转换。

```json
{"mode":"shell","io":"stdio","command":"printf hello; printf error >&2; exit 7","read_timeout":1000}
```

首次读取最多等待 10000 ms；未结束返回 `asyncID`，前缀 `stdio-`。
默认关闭 stdin；`stdin` 可发送最多 4096 字节字面文本后关闭。
`keepStdin: true` 保留输入管道，后续 `attach` 沿用 `text/filePath` 的转义解码与大小限制，
`closeStdin: true` 发送 EOF。后续按 ID 自动选择后端；显式不匹配的 io 会失败。

[stdio_tasks.rs](../../crates/server/src/stdio_tasks.rs) 独立持有进程、输入管道、
两个输出泵、有限广播队列、退出状态和取消信号。它不创建 PTY、viewer 或主控权。
同一客户端的 status/list/attach/stop/remove 才能访问任务；同用户的跨客户端只读例外只适用于 PTY。
stdio 操作需要 execute 权限，文件输入另需 filesRead。
列表合并到 Exbash 当前客户端任务以及运维程序列表；PTY 通道列表不包含 stdio 读取。

输出泵持续排空管道，广播队列为 32 个、每个最多 16 KiB 的块，不保留回放。
单次响应 stdout/stderr 合计最多 1 MiB；超限或消费者落后用 `outputTruncated`、
`droppedChunks` 标注。无订阅者时输出直接丢弃；需要完整日志时由程序写文件。
服务器最多同时维护 256 个 stdio 任务，终态仅保留最多 256 条、30 秒回执，无输出历史。
客户端长期历史由 RexETerm AppCore 维护。

[stdio_tasks/tree.rs](../../crates/server/src/stdio_tasks/tree.rs) 在 Unix 使用独立进程组，
Windows 使用配置了 KILL_ON_JOB_CLOSE 的 Job Object。
停止、超时、会话租约失效、管理器释放都会终止所属进程及子进程。
根进程结束后也清理其剩余子进程，给 stdout/stderr 最多 1 秒排空尾部，避免后台子进程长占管道。

## 命令操作

### 参数与模式

检查输入大小、时限和模式。

输入与输出：`ExbashOptions` → 有效操作。

实现位置：[exbash/options.rs](../../crates/server/src/tools/exbash/options.rs)；模式路由在 `mod.rs`。

### 命令启动

直接执行程序，或接收 Client 已选定的 shell profile 规格并在远端解析候选程序，再创建 PTY 与运行标识。

输入与输出：命令、目录 → `asyncID`、运行实例。

实现位置：[exbash/run.rs](../../crates/server/src/tools/exbash/run.rs)：`run_command`、`start_job`。

### 首次观察

等待结束或读取窗口到期后返回当前结果。

输入与输出：运行实例、`read_timeout` → 结束结果或 detached。

实现位置：`wait_for_stop_with_output`。

### 输入准备

读取文本或文件输入并约束大小。

输入与输出：输入来源 → 所选 I/O 后端的字节。

实现位置：[exbash/input.rs](../../crates/server/src/tools/exbash/input.rs)：`input_data`。

### 附着观察

可写入输入，等待退出或时限后取得本次观察窗口的原始输出。

输入与输出：`asyncID`、输入、读取时限 → 当前输出与状态。

实现位置：[exbash/attach.rs](../../crates/server/src/tools/exbash/attach.rs)：`attach_input`、`attach`。

### 控制权检查

输入前由服务层原子取得主控并写入，等待期间检查服务层当前控制者是否仍匹配。

输入与输出：预期控制者、当前控制者 → 继续或控制丢失错误。

实现位置：`wait_attach_timeout`。

### 运行列表与详情

提供运行身份、描述、进程与状态。

输入与输出：管理器、筛选参数 → 运行信息。

实现位置：`list_run_details`、`run_detail`。

### 停止与超时

结束进程并记录停止原因。

输入与输出：`stop` 或总运行时限 → 最终状态。

实现位置：`stop_run`、超时任务。

### 移除

停止需要结束的运行、关闭客户端并移除条目。

输入与输出：`asyncID` → 移除结果。

实现位置：`remove_run`。

### 结果注解

维护描述及 `timeout/stopped` 等退出原因显示。

输入与输出：进程状态、原因 → 工具元数据。

实现位置：`runs.rs` 的标签与格式化逻辑。

## 终端桥接

### PTY 能力组合

组合 RE 原生终端运行时的创建、查询、原始输出和退出订阅。

持有状态：底层 `PtyManager`。

实现位置：[shell_manager.rs](../../crates/server/src/shell_manager.rs)。

### 连接登记

跟踪终端连接、内部终端事件通道与连接代次 token。

持有状态：连接注册表。

实现位置：`register_client`、`remove_client`。

### 用户锁定

维护 RE 特有的终端锁定策略。

持有状态：已锁定 PTY 集合。

实现位置：`lock_pty`、`unlock_pty`。

### 状态广播

把控制者、尺寸和退出变化转为 `TerminalEvent`，由网络适配器发送给终端。

持有状态：客户端元数据。

实现位置：`broadcast_meta`、退出观察。

### 终端协议适配

处理 Hello、输入、Resize、RequestControl 和状态查询。

持有状态：当前传输 Link。

实现位置：[terminal.rs](../../crates/server/src/terminal.rs)。

## 生命周期与状态边界

`run/shell → 创建 PTY → 读取窗口结束后返回或进程退出 → attach/stop → remove`。

总运行 `timeout` 控制进程寿命；`read_timeout` 控制一次观察等待。detached 表示本次调用已返回而进程可能继续运行。`attach` 返回本次观察窗口的原始输出，其职责有别于无限追加的日志流。

真实进程和实时输出订阅位于 RE 的 [原生终端运行时](terminal.md)，终端屏幕由客户端解析维护。ShellManager 增加连接和锁定策略，以内部事件通知适配器；Exbash 增加命令操作与结果语义。RE 服务端保存 RE 客户端的创建归属与当前控制归属；宿主业务会话和任务配额仍由上层项目管理。

## 实现布局

`exbash/mod.rs` 只编排模式和管理操作；`options.rs` 定义参数与校验；`run.rs` 构造命令、启动 PTY 并观察初始输出；`attach.rs` 处理控制权与当前输出观察；`input.rs` 解码文本和文件输入；`runs.rs` 维护运行详情、标签、停止与超时。内部接口限制在 Exbash 模块内。详细接口见 [Exbash README](../../crates/server/src/tools/exbash/README.md)。

## RE 会话归属

新会话的输入控制标识使用 `rec:<asyncID>:<sessionId>`，避免多个客户端共享虚拟控制 ID。
旧客户端保留 `rec:<asyncID>`。程序创建者不会因控制切换或创建客户端断线而改变。
`rexctl programs` 分别显示创建者和当前主控，`rexctl pty` 显示当前终端通道；
服务端 stdout 记录程序创建、主控请求和实际切换。详见[会话与运维](management.md)。

## 相关文档

- [文档索引](../README.md)
- [设计总览](../design.md)
- [文档规范](../documentation-guide.md)

## 客户端显示与跨客户端只读观察

rexed 不再解析终端屏幕。`output.text` 保留原始终端操作的 UTF-8 投影，
`metadata.terminalOutput.format` 为 `raw-pty-v1`，并提供尺寸。
RexETerm AppCore 对本次观察片段按需解析为文字，GUI 的实时二进制流由 xterm 解析。
单次命令回答最多保留 1 MiB 原始输出，超出标记 `outputTruncated`；实时流不受此回答上限影响。
结束任务不再保留原生 PTY，`status` 读取短期终态回执，长期历史归客户端。

同 RE 用户的其他客户端可以凭已知任务 ID 调用无 `text` / `filePath` 的 attach，
输入 attach、stop、remove 和按 ID 列表仍要求归属客户端。
App 内 MCP 共享 AppCore 与客户端身份，是同一 App 的适配入口。

## Windows stdio 文本兼容

Windows 执行端在生成 stdio 响应的 `stdout` / `stderr` 文本时，优先保留有效 UTF-8；
非 UTF-8 输出使用 Windows 系统 OEM 代码页转换为 Unicode，简体中文系统通常为 936（GBK）。
文本中的 CRLF 统一为 LF，避免 Markdown 客户端将回车显示为 `�`。
兼容处理仅位于执行端的 stdio 文本投影，字节计数、管道、PTY 原始流和其他平台行为保持原有语义。

自动兼容不能确定任意自定义编码，也不保证同一输出流混合编码的正确显示。
使用其他代码页或混合输出的程序应配置为统一输出 UTF-8。
