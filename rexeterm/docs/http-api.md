# 普通 HTTP 与 CLI

## 范围

普通 REST API 由 `rexeterm-http-adapt` 库承载，组合现有 Application/AppCore，供需要网页或网络 API 的宿主显式启用。
独立 `rexeterm-http` 可执行程序已合并到 CLI 的 daemon 运行形态；CLI 本身不使用 HTTP 适配器，
业务命令通过原生 Unix socket / Windows 命名管道直达共享业务层。
MCP stdio / MCP HTTP 是独立适配器，不使用这里的 REST 路由。

## CLI 宿主与目录绑定

CLI 仅需一个 `rexeterm` 二进制，`service create/start` 自启动同一程序的 `daemon` 模式。
当前目录的 `.rexeterm/binding` 选择默认 UUID，`service create` 自动写入；
UUID 对应的宿主记录、私有 token 和 socket/管道定位保存在二进制旁 `.rexeterm-socket/`。
配置仍默认从创建命令的 cwd/.rexeterm/config-dir 选择。详见 [原生 IPC](cli-ipc.md)和 [Agent CLI](agent-cli.md)。
`--host-binary` 和 HTTP 形式的 `api METHOD PATH` 不再使用。

## 可选普通 HTTP 适配器

应用组合层可调用 `start_http(Application, HttpOptions)` 显式启动 TCP HTTP 或 Unix HTTP IPC。
TCP 要求配置 token；这不是 CLI daemon 的默认监听，不存在自动 TCP 回退。
Unix socket 权限为 0600，已占用路径不会由通用适配器自动删除。

TCP 每次请求要求 `Authorization: Bearer <token>`，token 32..256 个 ASCII 字母数字、`-` 或 `_`。
token 不放进 URL。宿主直接监听 HTTP，跨机部署可由 HTTPS 反向代理保护；
RE 连接的 TLS 配置独立，见 [RE 认证](../../rexed/docs/authentication.md)。
浏览器 Origin 必须精确匹配本监听地址或 `HttpOptions.allowed_origins`。
`POST /session` 可设置 HttpOnly、SameSite=Strict cookie；cookie 认证始终要求有效 Origin。
普通 API 当前不提供跨 Origin 的 CORS 预检，浏览器宜使用同源代理。

## 接口

路由统一位于 `/api/v1`，成功 JSON 当前返回 `200`。
随程序提供的 [OpenAPI 3.1](../crates/http-adapt/openapi.json) 可从 `GET /api/v1/openapi.json` 获取。
错误包含 `error.code` 与 `error.message`；业务调用失败返回 `502`，参数错误 `400`，
认证失败 `401`，Origin 拒绝 `403`，冲突 `409`，容量不足 `429`。

| 路径 | 方法 | 行为 |
| --- | --- | --- |
| /info、/capabilities | GET | 版本与能力/预算；CLI 宿主 info 含 serviceId |
| /cli-host/stop | POST | 仅 CLI 宿主可用；要求匹配服务 ID，通知进程所有者关闭 |
| /hosts | GET、POST | Host 列表、新增 |
| /hosts/{host} | GET、PATCH、DELETE | 配置读取、更新和删除；更新/删除前先断开 |
| /hosts/{host}/connection | GET、PUT、DELETE | 连接状态、幂等连接、按 Host 断开 |
| /hosts/{host}/tasks | GET、POST | 当前客户端活动任务和应用终态历史、创建任务 |
| /hosts/{host}/tasks/{task} | GET、DELETE | 查询状态、删除本地完成历史 |
| /hosts/{host}/tasks/{task}/stop | POST | 停止自己的任务 |
| /hosts/{host}/tasks/{task}/pty | GET WebSocket | 原始终端字节与控制 |
| /hosts/{host}/resources | GET | 当前客户端的远端活动通道 |
| /hosts/{host}/fs/entries、/stat、/preview | GET | `path` 查询参数；entries 接受 `offset` |
| /hosts/{host}/searches | POST | 同步搜索：`path`、`query`、`content` |
| /hosts/{host}/transfers | POST | 创建 HTTP 暂存上传/下载 |
| /transfers、/transfers/{transfer} | GET | 此 HTTP 适配器创建的暂存任务列表和状态 |
| /transfers/{transfer}/content | PUT、GET | 二进制上传内容、取回已完成下载 |
| /transfers/{transfer}/cancel | POST | 取消暂存或底层文件任务 |
| /hosts/{host}/forwards | GET、POST | 本宿主当前 Host 的转发列表和创建 |
| /hosts/{host}/forwards/{forward} | DELETE | 按 Host 校验后停止转发 |
| /preferences | GET、PATCH | 偏好读取；PATCH 提供 `host`、`directory`、`options` |
| /preferences/reload | POST | 重新读取偏好 |
| /events | GET SSE | 每秒 `state` 快照，无历史重播 |
| /requests | POST | 高级工具调用；远端业务必须指定 `executor` |

创建任务、转发和文件任务要求 `Idempotency-Key`，同 key 与同请求返回已有结果，
不同请求复用 key 返回 `409`。最多保存 256 项、10 分钟。
请求中断后的未知结果也保留占位，重试不会再次执行，调用方应查询资源确认结果。

```bash
rexeterm api hosts.save \
  --body '{"id":"node","name":"Linux","url":"tls://re.example.test:45957","root":"/home/alice","username":"alice","connectionProfile":"/private/alice.json"}'
rexeterm connect node
rexeterm run node 'printf hello; sleep 10'
rexeterm tasks node
rexeterm pty node <asyncID>
```

任务创建可设置 `mode`（`shell` 或 `run`）、`io`（`pty` 默认，或 `stdio`）、`command`、
`cwd`、`timeoutMs`、`readTimeoutMs`、`title`、`description`、`shell`，以及 stdio 专用 `stdin`、`keepStdin`。
标题最多 40 个 Unicode 字符、默认“未命名”，描述最多 200 个字符、默认空；
空白标题归一为“未命名”。stdio 和 PTY 共用任务元数据，创建结果、状态、列表和宿主完成历史
均保留顶层 `title`、`description`。
HTTP 默认首次读取 0 ms，CLI 的 run 默认 stdio 并读取 1000 ms。
PTY WebSocket 二进制帧传输入/输出，文本帧仅控制和元数据。
客户端控制消息是 `{"type":"resize","cols":120,"rows":30}` 或 `{"type":"request_control"}`。
PTY WebSocket 是 HTTP/GUI 实时入口。CLI 每次输出一份 Markdown 后退出，
不打开交互 viewer；`task read` 读 daemon 的客户端缓冲（缓冲为空时才发有限 Exbash attach），
`task input` 仍使用有限 Exbash attach。详见 [Agent CLI](agent-cli.md)。
关闭 HTTP 适配器不关闭宿主 AppCore；最终关闭宿主才释放连接与运行资源。

## 文件与容量

下载先创建 `{"mode":"download","path":"/remote/file"}`，轮询到 `status: "done"`，再 GET content。
上传先创建 `{"mode":"upload","path":"/remote/file","size":123}`，随后 PUT 原始内容。
可提供 `overwrite`、`sha256`、`title`（40 字符）与 `description`（200 字符）。

```bash
rexeterm upload node ./local.bin /tmp/remote.bin
rexeterm download node /tmp/remote.bin ./copy.bin
```

HTTP 文件内容在宿主私有临时目录流式暂存，RE 侧始终走统一文件数据通道。
单文件上限 256 MiB，总暂存 512 MiB，最多 64 项，创建 10 分钟后回收并取消未完成任务。
PUT 需要精确匹配声明大小，超时 120 秒；完成状态包含字节、SHA-256 和实际平均速率。
普通 JSON 请求上限 64 KiB，同时处理 32 请求、128 连接及 16 个 PTY/SSE 流。
搜索是同步操作，SSE 是状态快照；尚无异步 operation 注册表或事件历史游标。

## 相关文档

- [共享应用服务](modules/application.md)
- [CLI 模块](modules/cli.md)
- [构建](build-and-api.md)
- [文档索引](README.md)
