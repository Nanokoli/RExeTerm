# 短暂断网与监听恢复

## 范围

本页说明 IP 不变、Wi-Fi 短暂断开并恢复时，rexed 的监听与客户端会话行为。
监听可接收新连接、已有物理链路是否存活、会话是否过期是三个独立状态。
通常网卡短暂断开不会关闭监听 socket，但可能中断既有 TCP 流。

## 监听恢复

原 TCP/TLS/WS/WSS 业务和网络运维入口使用 `while let Ok(...) = accept().await`，
任意一次 accept 错误都会结束监听任务。rexed 主进程仍可能运行，但端口不再接受连接。
该缺陷已修复；这不代表已经确定某台机器 Wi-Fi 故障的唯一原因。

`crates/server/src/resilient.rs` 保存绑定后的实际 IP 和端口。
accept 失败时关闭当前监听 socket，100 ms 后尝试按原地址、原端口重新绑定；
失败继续重试，等待间隔指数增长至最多 5 秒。Interrupted 直接重试。
原监听指定端口 `0` 时也保留首次分配的真实端口。
Unix 的原始监听 socket 启用 SO_REUSEADDR，以便仍有已接受连接时重绑监听。

只替换监听 socket，不主动关闭已经接受的连接，也不变更管理/业务地址、路径或认证。
重试记录 `listener.retry`，重绑成功记录 `listener.rebound`；错误写 stderr。
绑定地址暂时不可用或端口暂时被占用时持续重试。
首次启动无法绑定仍报错；IP 改变时不自动改绑其他地址。
Unix socket 与可选 KCP 的 accept 错误改为等待后重试，未实现它们的自动重绑。

## 客户端与资源

SDK 每 10 秒发心跳，每次使用新链路，单次最多等待 15 秒；失败后仍继续下一次尝试。
有效心跳、链路绑定或恢复续租。服务端默认 60 秒未续租时终止任务并回收通道，
可用 `settings.json.clientDisconnectTimeoutSeconds` 调整。
短暂断网后能够在租约结束前续租时，任务可继续；超过租约后要重新连接，已回收任务不会恢复。

文件、PTY、转发流断开后需要重新建立。服务端监听恢复不会自动恢复旧字节流，
不会重放命令、上传或其他写请求。没有实现传输无缝续传承诺。
新的查询正常而旧 PTY 卡住时，应检查旧链路和客户端重连，而非仅检查监听端口。

## 验证与排查

Unix 单元测试 `real_accept_failure_rebinds_same_port_and_preserves_existing_stream` 对真实监听 socket
执行 `shutdown`，让 OS 的 accept 返回错误，再验证同一端口的新连接与已有连接均可读写。
测试不改变 IP，也不关闭真实 Wi-Fi，不能替代 Windows 网卡实机测试。

现场出现问题时，应分别检查：

1. rexed 进程是否仍在，stderr 是否出现监听/连接错误。
2. `.rexed-logs` 中是否有 `listener.retry`、`listener.rebound` 或认证失败。
3. 端口是否监听，新建客户端能否连接。
4. 新链路可连但旧流卡住时，重建该流，并检查会话是否已超时。

应用层不会修改防火墙、网卡或路由设置；这些状态需要结合故障现场判断。

## 相关文档

- [服务端日志](logging.md)
- [会话与运维](modules/management.md)
- [传输层与数据层](modules/transport.md)
- [文档索引](README.md)
