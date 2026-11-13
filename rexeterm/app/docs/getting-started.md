# 运行 Tauri 工作区

## 前提

RemoteExecutor 与 RexETerm 两个源码目录相邻。
当前原生入口支持 Windows。编译在 Linux Builder 完成，需要 Rust Windows MSVC 目标、cargo-xwin、LLVM 与 Windows SDK。
Windows 运行端只需 WebView2 Runtime；测试和调试不在该端编译源码。
准备可连接的 rexed；文件和转发使用统一 RE 协议，需要相应版本的服务端。

## 构建和启动

在 Linux Builder 的 `research_rexeterm` 目录运行：

```bash
bash scripts/build-windows.sh
```

该入口交叉编译六个 Windows 程序，并生成 `artifacts/windows/debug/windows.zip`，
包含 exe、PDB 调试符号和来源清单。部署工具校验后解压到 Windows 的独立运行目录，
再运行其中的 `rexeterm.exe`。不会向 Windows 同步源码或启动 Cargo。

前端随编译嵌入程序，修改前端后需在 Linux 重新构建；不需要 npm install 或网页开发服务器。
发布优化程序可加 `--release`，当前没有安装包配置。

## 连接与操作

首次启动没有预置 Host。左侧 Host 页点击加号，填写名称、RE 地址和远端起始目录，
保存后点击该 Host 连接。正式服务使用 tls 或 wss；Host 表单可指定 RE 用户名和本机私有认证配置文件路径，
格式见 [RE 认证](../../../RemoteExecutor/docs/authentication.md)。
可信开发链路还可使用 ws、tcp 和 unix，实际可用性依赖平台和链路配置。
随后可浏览目录、搜索、预览文件，切换文件树并在下方查看属性。

底栏任务页可运行命令，点击记录在中心打开该任务 PTY。获得主控后可输入；
只读时可以显式申请主控，关闭预览保留远端程序。
文件传输通过原生文件选择框确定本地路径，再确认远端路径和是否覆盖。
转发页建立本地 TCP 监听到远端目标，端口填 0 自动分配。
顶部 MCP HTTP 默认关闭，可设置监听地址后开启，并显示实际地址。

## 查看 Host 运维

点击左栏“运维”，选择 Host 即尝试读取独立管理端，不需要先连接业务端。
未填写地址时仅尝试该 Host 的网络业务端口 + 1；“连接设置”可填写实际 tcp、tls、ws、wss 或 unix 地址。
Unix 地址指 App 所在机器的 socket，不会自动访问远端文件路径。
rexed 默认只开本地管理 socket；远程管理须先在 rexed 的本地配置中显式设置管理监听。

默认复用 Host 的认证 profile；业务用户没有 management 权限时，选择独立的管理员 profile。
保存后重新认证，五个页面展示全局客户端、任务与三种通道；权限不足和连接失败显示状态，不保留旧数据。
成功后约 1.8 秒刷新，失败后点击“刷新”重试。该入口只读，不改变业务 Host 或操作他人的任务。

## 配置和退出

默认配置为 App 二进制旁 `.rexeterm-config/`，不会跟随 cwd 或 APPDATA。
`workspace.json` 保存 Host ID 索引与最后选择；连接、用户密钥、偏好按执行器目录保存。
任务终态保存到 `preferences.task-history.json`，详细布局见
[客户端配置](../../docs/client-config.md)。

正常关闭窗口会取消并等待文件任务，释放当前 PTY、HTTP 和 AppCore。
正常退出立即结束本客户端的运行资源；意外失联进入服务端默认 60 秒宽限期，期间可重连恢复任务，超时强制停止。
文件和转发表单支持标题（默认“未命名”，最多 40 字符）及描述（默认空，最多 200 字符）。PTY 使用任务信息。

## 检查

```bash
# 在 Linux Builder 检查前端和后端。
node --check app/frontend/app.js
node --test app/frontend/utils.test.mjs
cargo test --locked --test desktop --features mcp-http,server
```

PTY 输入输出集成测试在 Unix 测试端运行；Windows 需通过原生窗口另行验证实际交互。
当前未启用 Linux/macOS 原生窗口入口。

## 相关文档

- [App 入口](../README.md)
- [设计](design.md)
- [后端模块](modules/backend.md)
- [前端模块](modules/frontend.md)
