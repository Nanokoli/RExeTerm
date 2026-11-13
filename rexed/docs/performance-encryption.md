# 加密对速度的影响

## 测试范围

2026-10-30，在 Linux 6.12、Intel i5-13500 上，以 Rust release 构建测试。
服务端和 SDK 位于独立进程，走同机 TCP 回环，不渲染终端、不解析 VT。
对比 TCP/TLS 和 WS/WSS；明文端显式关闭认证，加密端使用密码登录后复用票据。
它测量当前完整链路的开销，包括 TLS 和认证帧，不能视为纯密码算法微基准。

每种协议预热一轮，测量三轮，下表取中位数。MB/s 为十进制，1 MB = 1,000,000 bytes。
PTY 每轮产生 131,072,000 字节；文件和转发每轮 256 MiB。
PTY 按产生的有效载荷计速，实际读取量另含换行转换、控制字节和完成标记。
所有轮次验证完整性：PTY 字符计数、文件 SHA-256、转发全字节与远端 ACK。

## 吞吐

| 能力 | TCP MB/s | TLS MB/s | WS MB/s | WSS MB/s | TCP→TLS 下降 | WS→WSS 下降 |
| --- | --- | --- | --- | --- | --- | --- |
| PTY 短行 | 183.2 | 181.8 | 192.2 | 183.7 | 0.7% | 4.4% |
| PTY 连续输出 | 692.2 | 659.1 | 715.3 | 705.3 | 4.8% | 1.4% |
| 文件上传 | 923.2 | 760.3 | 911.4 | 761.5 | 17.6% | 16.4% |
| 文件下载 | 925.0 | 755.5 | 939.1 | 764.8 | 18.3% | 18.6% |
| TCP 转发 | 3208.4 | 2166.0 | 3210.5 | 2296.5 | 32.5% | 28.5% |

当前 PTY 的下降约 1–5%，文件约 16–19%，转发约 29–33%。
这是同机回环峰值结果；三轮不足以断言很小的差异具有统计显著性。
文件包含既有流式 SHA-256 和最终提交；未将 SHA-256 从正常文件路径剥离。

## 延迟与 CPU

| 指标 | TCP | TLS | WS | WSS |
| --- | --- | --- | --- | --- |
| PTY 往返 p50，μs | 144.5 | 146.3 | 152.5 | 173.9 |
| PTY 往返 p95，μs | 244.3 | 221.3 | 231.2 | 265.4 |
| 短连接查询 p50，μs | 167.7 | 1690.5 | 617.7 | 1783.2 |
| 首次连接单样本，ms | 0.56 | 16.27 | 2.82 | 28.41 |

PTY 延迟预热 10 次后取 200 次，查询预热 10 次后取 100 次。
查询每次新建连接，包含 TLS 握手；首次连接只有一个样本，包含初次认证，不能当作平均值。
已建立的交互通道与短连接请求应分别评估。

| 主进程 CPU 秒/GB，三轮中位数 | TCP | TLS | WS | WSS |
| --- | --- | --- | --- | --- |
| PTY 短行 | 15.39 | 15.03 | 13.89 | 15.18 |
| PTY 连续输出 | 4.50 | 4.50 | 3.74 | 3.92 |
| 文件上传 | 1.95 | 2.37 | 2.00 | 2.43 |
| 文件下载 | 2.01 | 2.61 | 2.05 | 2.56 |
| TCP 转发 | 0.93 | 1.30 | 0.86 | 1.30 |

CPU 来自 `/proc/<pid>/stat`，时钟频率 100 Hz，只统计客户端与服务端主进程之和。
PTY 生成器子进程未计入；短于一秒的样本有计时量化误差。
文件 CPU 大致增加 20–30%，转发约增加 40%。

## 重现

原始样本见 [JSON 数据](performance-encryption-data.json)。
[测试程序](../tools/encryption-bench/src/main.rs) 和 [运行器](../tools/encryption-bench/run.py) 使用当前 SDK。
需要 Linux、Python 3 和 Rust；运行器只启动临时测试服务并在结束后关闭。

```bash
cargo build --release --bin rexed
cargo build --release --manifest-path tools/encryption-bench/Cargo.toml
rustc -O tools/encryption-bench/stdout-flood.rs -o tools/encryption-bench/stdout-flood
# 使用共享 CARGO_TARGET_DIR 时，显式提供两个 release 程序的绝对路径。
REXED_BIN=/absolute/target/release/rexed \
RE_BENCH_BIN=/absolute/target/release/re-encryption-bench \
python3 tools/encryption-bench/run.py
```

此轮未测跨机器、Windows、UDP/KCP 或多并发总带宽，不能据回环结果推算这些场景。
原始文件内容统一 SHA-256 为
`sha256:d4e0d5a6082e9536f1ff4fbc69855d8b3e458328f27af8d72cb104d8e81b5bc2`。

## 相关文档

- [认证与配置](authentication.md)
- [传输层](modules/transport.md)
- [文档索引](README.md)
