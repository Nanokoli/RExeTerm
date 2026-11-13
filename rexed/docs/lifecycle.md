# 活动资源与最终回执

## 范围

`rexed` 保存活动任务、PTY、文件和转发；完成文件没有服务端历史列表。
任务终态只保留短期轻量回执，长期历史由 RexETerm 保存。
默认启动不创建空闲 `main` shell；显式 `--pty-program` 才创建启动 PTY。

## 退出与回收

任务创建时立即建立退出观察，无需连接 viewer。
进程退出后最多等待一秒读取尾部，关闭原始输出并从活动列表移除。
已连接 viewer 最多再有两秒排空在途数据，随后释放附件和主控记录。
退出看门狗随进程退出释放，不为长超时任务持续持有原生 PTY。

最终回执最多 256 项、保留 30 秒，只包含元数据，不包含输出、环境变量或原生 PTY 句柄。
`exbash` 的 `mode: "status"` 可在此窗口查询最终 `exitCode`、`endedAt` 和输出字节数；
`mode: "remove"` 消费回执。超过窗口后状态不可查询，不代表任务成功。
活动列表始终只返回当前客户端的运行任务；运维入口可看所有客户端的活动资源。
客户端显式退出会回收自己的资源；失联按服务端心跳宽限期处理，默认 60 秒。

## 输出预算

服务端不保留原始 PTY 回放缓存。实时 viewer 的未读在途数据最多 4 MiB，
超限明确结束该订阅；每次合并最多 64 KiB，不加入固定批次等待。
CLI、GUI 和普通 HTTP PTY WebSocket 消费原始二进制，RE 不解析 VT。

命令请求的 JSON 回答最多收集 1 MiB，同时继续排流使程序完成；
`outputTruncated` 和 `outputLimitBytes` 标识截断。
这个限制只影响一次命令回答，不限制原始 PTY 字节流的总量。
新 viewer 只接收实时数据，不能恢复过去的屏幕。

## 应用历史

RexETerm AppCore 按 Host 别名保存任务最终元数据，活动任务最多观察 256 项，
完成历史最多 200 项，保存于偏好文件旁的 `*.task-history.json`。
每两秒读取一次 `status`，确认终态后消费远端回执。
连续三次查询失败记为 `unknown`；应用重启后未确认的旧运行记录也标为 `unknown`。
该历史没有终端输出，文件和转发完成记录仍由客户端应用管理。

## 相关文档

- [Exbash](modules/exbash.md)
- [认证与配置](authentication.md)
- [RexETerm 状态核心](../../rexeterm/docs/modules/appcore.md)
- [文档索引](README.md)
