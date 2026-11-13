# MCP 适配器

## 范围

两个传输适配器分成独立 Cargo 包，均接收现有 AppCore，复用
[mcp-common](../../crates/mcp-common/src/lib.rs) 的消息校验、工具声明和调用转换。
适配器不启动 daemon，不保存业务路由或偏好，不负责关闭共享核心。

## stdio 版

[mcp-stdio-adapt](../../crates/mcp-stdio-adapt/src/lib.rs) 提供普通 MCP stdio。
产品程序 rexeterm-mcp 在同一进程内嵌 daemon，只有 stdio 模式，无 --transport http 或 HTTP 监听选项。
EOF 等待在途工作后，入口再关闭 daemon。stdout 只写 JSON-RPC 响应，诊断写 stderr。
JSONL 含换行最多 16 MiB，最多 32 个在途请求；达到预算时先等待完成再读下一条。
共享输入辅助位于 [adapter-common](../../crates/adapter-common/src/lib.rs)。

## App 的 HTTP 适配器

[mcp-http-adapt](../../crates/mcp-http-adapt/src/lib.rs) 对外提供 Streamable HTTP，
AppHost 运行时可以开启或关闭它。监听默认关闭；开启的默认地址是 127.0.0.1 上的自动分配端口。
所有 HTTP 会话和 App 界面访问同一 AppCore，所以默认节点、偏好、连接、转发和本实例创建的 exbash 任务相互可见。
HTTP 会话的随机 ID、协议版本、取消和租约分别维护，不为每个 HTTP 会话创建独立 RE 用户或核心。

仅 /mcp 端点：POST 返回 JSON；合法通知/响应返回无正文 202；GET 返回 405；DELETE 返回 204。
initialize 分配 256 位随机 Mcp-Session-Id；后续请求需携带 ID 和匹配的 MCP-Protocol-Version。
缺失 ID 为 400，未知或过期为 404。Content-Type 为 application/json，Accept 须包含 application/json 和 text/event-stream。

DELETE、超时回收和 HTTP 关闭只取消对应协议工作，不关闭 AppCore、RE 会话或已建立的转发。
App 退出由宿主统一清理。取消等待不能撤回远端已接受的操作。
重新开启 HTTP 时旧 MCP 会话失效，App 状态保持；其他核心实例仍互不共享。

## 协议与限额

单条 JSON-RPC 2.0 对象，不接受 batch。错误码 -32700、-32600、-32602、-32601 分别表示解析、
信封、参数和方法错误；工具执行失败使用 isError。工具参数 targetExecutor、directory、toolTimeoutMs 转为核心字段。
stdio 支持 2024-11-05、2025-03-26、2025-06-18；HTTP 支持后两个版本，其他版本回应 2025-06-18。

HTTP 默认限制 128 个会话、32 个在途 POST、单正文 16 MiB、正文读取 30 秒、空闲租约 30 分钟。
在途工作不因空闲回收关闭。配额耗尽返回 429 或 503，正文过大返回 413。
Origin 缺失允许原生调用；存在时须精确匹配监听 origin 或 allowed_origins，否则 403。
未实现 SSE、事件恢复、TLS、账号认证、OAuth、ACL、进度推送或 notifications/cancelled。
会话 ID 是协议绑定，不是认证。PTY 与文件实时 UI 事件应直接接 SDK，文件内容仍走 RE File 通道。

## 使用

```bash
cargo build -p rexeterm --no-default-features --features mcp-stdio --bin rexeterm-mcp
rexeterm-mcp --no-local
```

App 的 HTTP 开关接口见 [App 后端](../../app/docs/modules/backend.md)。
旧 rexeterm-mcp --transport http 启动方式已移除；HTTP 属于 App 版的可选集成。

## 资源归属与通道标签

`client_resources` 查询当前持久客户端的服务端资源；全局列表由独立运维连接提供。
`exbash`、`file_transfer`、`forward` 接收 `title`（默认“未命名”，最多 40 字符）和 `description`
（默认空，最多 200 个 Unicode 字符），stdio / HTTP 共用 schema 和透传路径。
`exbash` 的 stdio、PTY 两种 I/O 使用相同任务元数据；PTY 直接对应任务，没有独立标题。

## 相关文档

- [MCP 传输规范](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
- [AppCore](appcore.md)
- [daemon](daemon.md)
- [构建](../build-and-api.md)
- [文档索引](../README.md)
