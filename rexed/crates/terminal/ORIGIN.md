# 代码来源

本 crate 的会话内核与测试迁自 [Yeamika/pty-t](https://github.com/Yeamika/pty-t)，
采用原 RemoteExecutor 子模块固定的提交 `145981f2308b7ce8ddd41abc63407d2e3a9e8b24`。
此次导入于 2026-10-28，作为 RE 自有终端运行时继续维护。

## 导入范围

- 上游 `crates/core/src/`：命令规格、会话登记、进程、PTY、输出、屏幕与客户端控制。
- 上游 `crates/core/tests/`：命令目录与环境、退出订阅、超时后终止、历史限制、控制者及客户端 ID 测试。
- 上游 `crates/protocol/src/lib.rs`：迁入 RE 的 `crates/protocol/src/terminal.rs`，保留现有终端线上消息兼容性。

## 本地整理

包名改为 `remote_executor_terminal`，底层直接依赖 `portable-pty` 与并发基础库。
原单文件 Session 按创建与共享状态、控制、输出、进程退出及元数据分为子模块。
删除未采用的 `PtyServer` 别名和协议层尺寸辅助函数；RE 网络侧只保留统一监听入口。
运行时不引入 WebSocket、SSH、HTTP 或 RE 网络协议依赖。

终端协议中的远端管理修改消息仍可被识别并拒绝，以保持现有协议行为。
连接登记、随机 ID 分配及主控管理已迁到 RE server 的 attachment 服务；原生输入接口只接收字节。
输出改为有界实时分发，不保留历史；慢订阅者报错且不阻塞其他订阅者。

## 许可记录

上游该固定提交未提供独立 LICENSE 文件，core/protocol 的 Cargo 清单也未声明 license 字段。
此文件保留来源与修改记录，不推断或替代上游许可条款。

服务端 VT 屏幕解析和历史缓存已移除，仅保留实时原始字节订阅与累计字节计数。
