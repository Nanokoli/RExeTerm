# CLI 与运维工具

## 范围

`rexeterm` 同时提供单次 Agent 命令和常驻 `daemon` 模式。只需分发这一个二进制，
`service create/start` 以当前可执行文件启动独立后台进程，运行身份显示为 `rexetermd`。
Linux 进程显示名称会改变，系统用户不改变；Windows 映像名称仍为 `rexeterm.exe`。
业务调用复用 daemon 的 AppCore、RE 身份和偏好，每次返回一份 Markdown 后退出。

## 内部结构

[main.rs](../../crates/cli/src/main.rs) 区分长期 daemon 与短期调用，统一错误和输出；
[commands.rs](../../crates/cli/src/commands.rs) 定义命令并映射原生 IPC 操作；
[markdown.rs](../../crates/cli/src/markdown.rs) 隔离终端控制字符，并用 `___` 块（而非代码围栏）承载 payload 文本。
[services.rs](../../crates/cli/src/services.rs) 负责当前程序的后台启动、选择、列举和停止。

[cli-host](../../crates/cli-host/src/lib.rs) 负责 UUID64、boot ID、私有注册表和配置进程锁；
[ipc.rs](../../crates/cli-host/src/ipc.rs) 只处理帧、版本、长度上限和本地连接，不依赖 RE 或 Application。
[ipc_client.rs](../../crates/cli/src/ipc_client.rs) 执行有限请求并验证宿主身份。
CLI 的 `ipc-client` 库 feature 保持无 AppCore、RE 和 HTTP 依赖；产品二进制的 `daemon` feature
才组合共享核心和 [host.rs](../../crates/cli/src/host.rs)。`local` feature 允许显式开启本地执行器。

[host.rs](../../crates/cli/src/host.rs) 持有注册锁、Application、IPC 监听和最终关闭，
并维护每个任务的客户端输出缓冲（`tasks.read`）——回放属于客户端，rexed 只负责连续送流：
`run` 后按 `asyncID` 起一个后台 `attach` 泵，持续记录到任务停止或 `task stop`/`forget`，
[transfers.rs](../../crates/cli/src/host/transfers.rs) 维护有限暂存任务，复用 Application 的传输能力。
普通上传下载传递绝对本地路径，daemon 直接使用 RE 文件通道，文件内容不编码为 JSON 或 HTTP。
分步上传先把源文件复制到私有暂存区；分步下载通过共享文件系统读取完成内容并原子发布。

[stdio.rs](../../crates/cli/src/stdio.rs) 是可选 JSONL 嵌入适配；
[admin_view.rs](../../crates/cli/src/admin_view.rs) 是可选运维页面库。
根 [rexctl](../../src/bin/rexctl.rs) 使用独立 ManagementClient，不使用 CLI 的 UUID64。

## 绑定与生命周期

缺省注册表为二进制旁的 `.rexeterm-socket/`：`<boot-id>/` 下保存 UUID64 记录、
私有 token，Unix socket 直接保存在根目录。当前目录的 `.rexeterm/binding` 只保存所选 UUID64。
Windows 命名管道保存在内核，记录保存 `pipe://` 名称；禁止远程管道客户端，不监听 TCP。
`--registry-dir` 可显式迁移映射和私有运行目录，不改变 cwd 绑定位置。Linux 长安装路径通过目录文件描述符寻址，socket 仍在同一目录。

`service create` 自动生成或复用 UUID64 并写入当前目录的绑定；`bind` 选择已有宿主。
不同 cwd 独立选择宿主，不搜索父目录或程序旁的旧默认绑定。
显式 `--service-id` 优先，不读取或改写 cwd 绑定。配置默认从创建命令的 cwd/.rexeterm/config-dir 选择，
绑定后所有调用使用该宿主已经保存的配置目录。

UUID64 保留至系统重启；CLI 退出或 daemon 停止/重启不使其失效。
配置目录进程锁防止重复宿主；异常退出后可重用 ID 并清理本服务遗留 socket。
新 daemon 每次启动更换本机 token；旧 token 不再有效。最终退出清除活动端点、释放核心与连接。
`daemon` 或 `--daemon` 前台常驻，不受普通命令的 `--timeout-ms` 截止时间限制。

## 目录禁用标记

[directory.rs](../../crates/cli-host/src/directory.rs) 在任何配置、注册表或启动操作之前检查 cwd 的
`.rexeterm.disable`。`enable`、`disable` 和帮助可在禁用状态使用。
标记也阻止从该 cwd 启动 daemon，不会停止已经运行的宿主。

## 相关文档

- [Agent CLI 命令](../agent-cli.md)
- [原生 IPC 协议](../cli-ipc.md)
- [配置布局](../client-config.md)
- [构建](../build-and-api.md)
