# 文件与搜索工具

## 范围

该模块把文件读取、身份检查、局部修改和检索转换为统一工具结果。文件操作以目标节点的文件系统为准。

## 文件观察

### 文件身份

获取稳定文件键、规范路径、类型、大小和修改时间。

输入与输出：路径 → `FileStamp`。

实现位置：[fs/stamp.rs](../../crates/server/src/tools/fs/stamp.rs)：`file_stamp`。

### 内容散列

计算文件字节的完整 SHA-256。

输入与输出：字节/路径 → `hashCode`。

实现位置：`hash_bytes`、`file_hash_code`。

### 文本读取

读取有界文本片段并处理显示格式。

输入与输出：路径、偏移、限制 → 文本和元数据。

实现位置：[fs/read.rs](../../crates/server/src/tools/fs/read.rs)：`read_path`、`read_file`。

### 二进制读取

以字节片段和十六进制视图表示二进制内容。

输入与输出：字节范围 → 可读片段。

实现位置：`read_binary_file`、`hexdump`。

### 目录读取

枚举目录条目并提供有界结果。

输入与输出：目录、分页参数 → 条目列表。

实现位置：`read_dir`。

### 属性查询

提供不读取全文的文件状态。

输入与输出：路径 → 属性结果。

实现位置：[fs/stat.rs](../../crates/server/src/tools/fs/stat.rs)：`stat_path`。

### 路径匹配

遍历并匹配 glob 模式。

输入与输出：目录、模式 → 路径列表。

实现位置：[fs/glob.rs](../../crates/server/src/tools/fs/glob.rs)：`glob_paths`。

## 文件修改

### 操作分派

校验参数并选择创建、删除、重命名或补丁。

输入与输出：`FileActionOptions` → 修改结果。

实现位置：[file_action/mod.rs](../../crates/server/src/tools/file_action/mod.rs)：`file_action`。

### 版本前置检查

按请求核对当前内容散列。

输入与输出：预期散列、实际文件 → 允许执行或冲突。

实现位置：`file_action/mod.rs`。

### 文本补丁

以补丁开始时的行快照解释替换、删除、移动和插入。

输入与输出：文本、行指令 → 新文本。

实现位置：[file_action/text_patch.rs](../../crates/server/src/tools/file_action/text_patch.rs)。

### 二进制补丁

以原始字节偏移解释替换、删除和插入。

输入与输出：字节、偏移指令 → 新字节。

实现位置：[file_action/binary_patch.rs](../../crates/server/src/tools/file_action/binary_patch.rs)。

### 变更摘要

生成文件动作元数据、统一 diff 与新散列。

输入与输出：修改前后状态 → `ToolResult`。

实现位置：[file_action/result.rs](../../crates/server/src/tools/file_action/result.rs)：`diff_text`、`diff_binary` 等。

## 搜索

### 搜索参数与根路径

整理模式、目录、路径、glob、大小写和数量限制。

输入与输出：`RgOptions` → 搜索计划。

实现位置：[rg.rs](../../crates/server/src/tools/rg.rs)：`RgExecutor`。

### 文件遍历

遍历候选文件、应用忽略及过滤规则。

输入与输出：根路径、过滤器 → 候选文件。

实现位置：`search_sync`。

### 内容匹配

计算匹配行、列与文件信息。

输入与输出：候选文件、正则 → `RgMatch`。

实现位置：`search_file`。

### 结果呈现

生成路径、行号、列号与统计信息。

输入与输出：匹配集合 → `RgOutput`。

实现位置：`rg_search`。

### 协作超时

在搜索/遍历过程检查截止时间并记录是否超时。

输入与输出：时限 → 截止状态。

实现位置：[soft_timeout.rs](../../crates/server/src/tools/soft_timeout.rs)。

## 状态与边界

这些工具主要处理单次调用；持久状态是实际文件。会话级短引用与文件操作后的引用更新由上层维护。完整 `hashCode` 是内容版本凭据，显示给 agent 的短标签由上层生成。

`read` 有意返回内容片段；修改结果以变更摘要和元数据为主。文件操作通过 `ToolContext` 解析相对路径；内容搜索还需明确自身的 `root/path` 参数。

## 文件管理与 SFTP

目录浏览、属性、文件上传下载、创建、删除、重命名及散列校验可组合成类似 SFTP 的客户端体验。
现有传输使用 RE 工具与[统一文件数据协议](file-transfer.md)，标准 SFTP 客户端尚不能直接连接。
完整 SFTP 还涉及 SSH 子系统、目录修改、权限/时间戳、文件句柄及随机读写等语义；下载偏移不代表支持上传续传。

## 实现布局

`fs/mod.rs` 汇出读取、属性、匹配、身份与散列接口；子模块各自承载实现。`file_action/mod.rs` 集中处理文件读写和散列前置检查，参数在 `options.rs`，呈现在 `result.rs`。

文本与二进制补丁引擎只接收文本、字节与补丁指令，不依赖 `ToolContext`、网络、设置或文件系统。引擎成功后由文件操作入口写入结果。现有工具参数详见 [crates/server/src/tools/README.md](../../crates/server/src/tools/README.md)。

## 相关文档

- [文档索引](../README.md)
- [设计总览](../design.md)
- [文档规范](../documentation-guide.md)
