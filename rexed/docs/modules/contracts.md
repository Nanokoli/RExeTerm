# 契约与调用上下文

## 范围

本模块统一工具调用的语言，并把一次调用需要的目录、配置和终端管理器传入执行代码。

## 内部结构

### 节点身份

描述节点 ID、系统、设备与标签。

输入与输出：节点属性 → `ExecutorInfo`。

实现位置：[protocol.rs](../../crates/protocol/src/lib.rs)。

### 请求契约

携带请求标识、工具、参数、目录、目标节点和工具时限。

输入与输出：调用参数 → `ExecutorRequest`。

实现位置：`crates/protocol/src/lib.rs`。

### 响应契约

将成功结果或错误与请求标识关联。

输入与输出：执行结果 → `ExecutorResponse`。

实现位置：`crates/protocol/src/lib.rs`。

### 工具结果

分开承载模型可读内容与结构化信息。

输入与输出：文本及元数据 → `ToolResult`。

实现位置：`crates/protocol/src/lib.rs`。

### Shell 调用规格

`ShellCommand` 携带客户端选择的 profile、候选程序与展开后的参数。Executor 仅在自己的环境中解析并执行。
配置管理在 Client 完成，不通过该规格修改服务器配置。

实现位置：[shell.rs](../../crates/protocol/src/shell.rs)。

### 路径基准

解析相对路径并生成显示用相对名称。

输入与输出：`directory`、文件路径 → 执行路径、标题。

实现位置：[context.rs](../../crates/server/src/context.rs)。

### 依赖注入

为工具提供可选的设置和 ShellManager。

输入与输出：节点资源 → `ToolContext`。

实现位置：`context.rs`。

### 对外库面

根库按角色导出命名空间；协议、客户端和服务端也可以直接引用对应 crate。
`ToolContext` 属于服务端；协议 crate 只依赖 serde 与 serde_json。

输入与输出：模块能力 → Rust 库接口。

实现位置：[lib.rs](../../src/lib.rs)。

## 契约边界

Rust 字段 `ExecutorRequest.method` 在线上序列化为 `tool`。原始工具请求使用 `tool`；MCP/JSON-RPC 外层使用的 `method` 属于另一层协议。

成功的工具结果包含 `output` 与 `metadata`。`output` 的 `message`、`text`、`info` 面向阅读；`metadata` 供调用端继续处理。工具错误由响应层转换成 `ok: false` 与错误字符串。

`ToolContext` 持有一次调用的执行基准；它不拥有跨会话的文件引用，也不创建执行器注册表。搜索工具的 `root/path` 还受各自参数逻辑约束，不能仅凭上下文推断所有搜索路径。

## 典型协作

Client 选定节点后传递请求；Executor 根据目录装配 `ToolContext`；工具返回 `ToolResult`；Executor 把结果包装成对应请求的响应。各层新增字段时应保持该传递链一致。

## 相关文档

- [文档索引](../README.md)
- [设计总览](../design.md)
- [文档规范](../documentation-guide.md)
