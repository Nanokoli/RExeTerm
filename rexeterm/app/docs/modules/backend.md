# App 后端模块

## 范围

rexeterm-app 提供 AppHost 和 Desktop。原生窗口使用独立的 rexeterm-desktop 包，
不会给 AppCore 或普通 MCP stdio 产品引入 Tauri 依赖。

## 子职责

### 生命周期宿主

[lib.rs](../../backend/src/lib.rs) 中的 AppHost 持有 daemon 与唯一 AppCore。
HTTP 入口默认关闭，动态启停和最终关闭通过异步锁串行化。
关闭 HTTP 保留核心，最终 shutdown 才释放连接和转发监听器。

### 桌面命令和工作区

[desktop.rs](../../backend/src/desktop.rs) 委托共享 [Application](../../../docs/modules/application.md) 验证并原子保存 Host 配置，
按白名单分派连接、目录、预览、搜索、exbash、文件任务、转发和 HTTP 命令。
远端请求都指定 Host，复用 AppCore 的工具准备与偏好处理。
Host ID 唯一，重复新增返回错误；Application 提供新增、更新、删除和按节点断开，普通 HTTP 暴露这些能力；GUI 当前表单提供新增和连接。

目录每页最多 500 项，文件预览先 stat；搜索分别复用 glob 与 rg，结果保留截断和超时信息。
传输任务复用 transfer_file 和 TransferObserver；取消通过 watch 中止该 Future。
任务完成、失败和取消都有真实状态，文件分块与提交语义由 SDK/RE 维护。
任务页消费 rexed 按持久 clientId 隔离的 exbash list，不显示其他客户端的 task。
Desktop 的 resources 命令读取同一客户端的服务端活动通道；独立运维连接保留全局视图。

### 只读运维

[management.rs](../../backend/src/desktop/management.rs) 维护每个 Host 的独立 ManagementClient。
`management_snapshot` 读取保存的 Host 地址及私有 profile；`management_configure` 保存独立管理设置并关闭缓存连接；
`management_disconnect` 释放所选 Host 的管理连接。普通 HTTP/MCP 业务入口不分派这些 Desktop 命令。

地址支持 tcp、tls、ws、wss、unix；默认网络候选保留协议、IPv6 和 WS 路径，将业务端口加一。
这不启用服务端监听；默认 Unix 管理入口及远程 socket 必须按实际可达性配置。
管理连接有 12 秒读取预算，SDK 另有协议超时；只缓存成功连接，失败、取消和认证变更时重新建连。
返回状态包含 connected、denied、unavailable、unconfigured、invalid_configuration；失败时 snapshot 为空。
凭据只留在后台，不返回 profile 内容；管理失败不关闭业务连接或任务。

### PTY 预览

[terminal.rs](../../../crates/application/src/terminal.rs) 保留 TerminalConnection、通道租约与 RE 会话，
用 asyncID 连接 exbash 的 PTY。输入队列上限 32 帧，输出队列上限 64 个消息，
大历史帧拆成 32 KiB 消息，队列中保留字节而非 JSON 数字数组。
发送输入和接收输出分别驱动，输出队列满时不会阻塞输入发送。
轮询在没有数据时最多等待 100 ms，字节到达就返回；等待期间不持有预览状态锁。
每批最多消费 64 个消息，累计输出达到 256 KiB 后停止追加，允许最后一块最多多出 32 KiB；
相邻字节块在二进制 IPC 封装前合并，元数据顺序保持不变。
发送超时会结束通道；切换与退出取消流泵并等待清理，超时后中止任务。
主控和退出状态来自协议 meta，不从 UI 焦点推断。

`Desktop::terminal_poll` 返回原始 `Vec<u8>`；它不经过通用 JSON 命令分派。
原生桥接以 `tauri::ipc::Response` 交付二进制响应，前端收到 ArrayBuffer。
PTY 输出不转换成 JSON Value、数字数组、字符串或 Base64。
终端输入仍通过小型命令传递 UTF-8 字符串，控制和状态元数据使用 JSON。

App 内部二进制批次以 `RTP1` 四字节版本标记和一个 closed 标志字节开头；
标志只允许 0 或 1，表示该批交付后是否已排空并关闭。
后续按顺序放置记录，每条记录由一个类型字节、四字节小端无符号长度和原始载荷组成。
类型 0 是 PTY 字节，类型 1 是 UTF-8 JSON 元数据。该格式只用于 App 内部 IPC，不改变 RE 线上协议。
关闭标志可以与最后一批数据同时出现，前端先消费记录，再处理关闭。
旧预览 ID 拒绝、输出预算、队列背压、取消和任务保留语义不变。

### 原生桥接

[windows.rs](../../src-tauri/src/windows.rs) 注册 desktop_call、独立二进制 terminal_poll 和原生文件选择，
正常关闭窗口时等待 Desktop 清理。业务不经过 MCP 子进程；前端不直接访问文件系统或网络 RE。

## 状态与验证

保存的配置、客户端传输历史和远端运行事实分别维护。
实际业务通过真实 TCP RE 集成测试验证浏览、搜索、预览、持久化、文件字节、转发和 PTY 生命周期；
Windows 原生窗口另行验证 UI 与 WebView 交互。管理连接另以真实 TLS/WSS、权限拒绝、跨 Host 和重新认证测试验证。

## 相关文档

- [设计](../design.md)
- [前端](frontend.md)
- [运行指南](../getting-started.md)
