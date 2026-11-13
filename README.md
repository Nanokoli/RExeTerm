# RExeTerm

RE 远程执行服务端与 RexETerm 客户端的单一 Cargo 工作区。

| 目录 | 内容 | 产物 |
| --- | --- | --- |
| `rexed/` | `remote_executor` 服务端，以及 `protocol`、`transport`、`terminal`、`data`、`client`（`rexe-client` SDK）、`server` crates | `rexed` |
| `rexeterm/` | RexETerm 客户端：`appcore`、`application`、`daemon`、`cli`、`cli-host`、MCP 适配器、App 与 Tauri 前端 | `rexeterm-cli`（`rexctl`、`rexeterm-mcp` 为附加二进制，不随版本发布） |

## 构建

```bash
cargo build --release --locked -p remote_executor --bin rexed
cargo build --release --locked -p rexeterm-cli  --bin rexeterm
```

工作区级的格式、Clippy、测试和依赖边界检查：

```bash
cargo fmt --all -- --check
bash rexed/scripts/check-boundaries.sh
cargo clippy --locked -p remote_executor -p rexeterm-cli --all-targets -- -D warnings
cargo test --locked -p remote_executor -p rexeterm-cli
```

各项目约定见 [`rexed/AGENTS.md`](rexed/AGENTS.md) 与 [`rexeterm/AGENTS.md`](rexeterm/AGENTS.md)；
设计文档见 [`rexed/docs/`](rexed/docs/README.md) 与 [`rexeterm/docs/`](rexeterm/docs/README.md)。

## 发布产物

按 `cargo build --release --locked` 交叉编译，产物名 `<程序>-<平台>`：

| 程序 | 平台后缀 | Rust target | 运行器 |
| --- | --- | --- | --- |
| `rexed` | `x64` | `x86_64-unknown-linux-gnu` | ubuntu |
| `rexed` | `x64_musl` | `x86_64-unknown-linux-musl` | ubuntu + cross |
| `rexed` | `i686_musl` | `i686-unknown-linux-musl` | ubuntu + cross |
| `rexed` | `arm64` | `aarch64-unknown-linux-gnu` | ubuntu + cross |
| `rexed` | `arm64_musl` | `aarch64-unknown-linux-musl` | ubuntu + cross |
| `rexed` | `armv7_musl` | `armv7-unknown-linux-musleabihf` | ubuntu + cross |
| `rexed` | `arm64_termux` | `aarch64-linux-android` | ubuntu + NDK |
| `rexed` | `x64_msvc` | `x86_64-pc-windows-msvc` | windows |
| `rexeterm-cli` | `x64` | `x86_64-unknown-linux-gnu` | ubuntu |
| `rexeterm-cli` | `x64_musl` | `x86_64-unknown-linux-musl` | ubuntu + cross |
| `rexeterm-cli` | `arm64` | `aarch64-unknown-linux-gnu` | ubuntu + cross |
| `rexeterm-cli` | `arm64_musl` | `aarch64-unknown-linux-musl` | ubuntu + cross |
| `rexeterm-cli` | `x64_msvc` | `x86_64-pc-windows-msvc` | windows |

Unix 产物为 `tar.gz`，Windows 产物为 `zip`。
