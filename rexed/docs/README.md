# RemoteExecutor 文档

RemoteExecutor 提供 RE 执行能力。`rexed` 承担执行，`rexe-client` 是承接 RE 能力的纯 SDK。
应用实例管理、客户端偏好、MCP、CLI 与桌面程序位于独立 [RexETerm](../../RexETerm/README.md)。

## 设计

- [设计总览](design.md)：RE 的范围、架构和协作。
- [模块边界](module-boundaries.md)：依赖、状态与项目边界。
- [构建与迁移](build-and-api.md)：RE 构建、SDK 与 RexETerm 迁移。

## 模块

- [契约与调用上下文](modules/contracts.md)
- [rexe-client](modules/client.md)：节点、会话、工具、PTY、文件及转发。
- [会话与运维](modules/management.md)
- [Executor](modules/executor.md)
- [统一文件传输](modules/file-transfer.md)
- [文件与搜索](modules/files.md)
- [Exbash 与终端](modules/exbash.md)
- [原生终端运行时](modules/terminal.md)
- [传输层与数据层](modules/transport.md)
- [通用流与端口转发](modules/forwarding.md)

## 使用与验证

- [认证、加密与用户配置](authentication.md)
- [服务端日志](logging.md)
- [短暂断网与监听恢复](network-recovery.md)
- [活动资源与最终回执](lifecycle.md)
- [加密性能测试](performance-encryption.md)

## 维护

- [工具接口](../crates/server/src/tools/README.md)
- [文档规范](documentation-guide.md)
- [RexETerm 偏好管理](../../RexETerm/docs/modules/preferences.md)
