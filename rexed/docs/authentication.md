# 认证、加密与用户配置

## 范围

`rexed` 默认使用 TLS，接受密码、指定客户端证书、用户 CA 三种认证。
每条物理链路在业务协议之前登录，包括发现、控制、PTY、文件和转发。
RE 用户是应用身份；多个客户端可以使用同一用户名，资源仍按持久 `clientId` 隔离。
进程和转发使用运行 `rexed` 的操作系统账号，不切换系统用户，也不提供操作系统沙箱。

## 初始化与启动

默认配置目录是可执行文件旁的 `rexed-config`，与当前工作目录无关。
`--config-dir` 同时适用于初始化、用户管理和服务启动。

```bash
rexed --config-dir /private/rexed init --server-name re.example.test
# 从终端静默读取；不把密码写入命令参数、历史或配置明文。
read -rs password
printf '%s' "$password" | rexed --config-dir /private/rexed user alice \
  --password-stdin --allow execute,files-read,files-write,pty-view,pty-control,forward
unset password
rexed --config-dir /private/rexed --id node --listen tls://0.0.0.0:45957
```

`init` 创建自签名服务端证书；客户端需信任它，或改用受信 CA 签发的服务端证书。
服务端证书 SAN 必须包含客户端使用的主机名或 IP。
已有 `server.json` 不会被初始化覆盖；空用户目录拒绝所有登录。
Windows 没有默认 Unix 运维套接字，须在 `server.json` 配置 `adminListen`，
或启动时传入 `--admin-listen tls://127.0.0.1:45958`。
用户按需加载，新增账号不需要重启。新用户默认无权限，`--allow` 提供完整的新权限集合；
`--allow none` 清空权限，`--disabled` 禁用该用户。
更新命令未指定密码、证书或 CA 时保留已有值；移除认证方式可编辑用户 JSON；已缓存账号的修改见下文缓存行为。

```text
rexed-config/
  server.json
  server-cert.pem
  server-key.pem
  settings.json
  users/
    alice/
      config.json
      client.pem
      ca.pem
```

`server.json` 的 `certificate` / `privateKey` 相对配置根解析。
可选的 `adminListen` 保存独立运维入口的完整协议、监听地址、端口或 Unix socket 路径：

```json
{
  "certificate": "server-cert.pem",
  "privateKey": "server-key.pem",
  "adminListen": "tls://127.0.0.1:45958"
}
```

启动时按 `--admin-listen` → `server.json.adminListen` → 平台默认值选择监听地址。
旧文件不含此字段时保持原默认行为；空字符串或无效地址报错，不回退。
网络地址省略端口时使用业务实际端口 + 1；Unix 默认本地套接字。
业务监听仍由 `--listen` 指定。`server.json` 在启动时读取，修改后重启生效。
用户证书和 CA 路径相对该用户目录解析，也接受绝对路径。
密码保存为带随机盐的 Argon2id 哈希。
Unix 创建目录为 `0700`、文件为 `0600`；Windows 为当前进程账号设置私有 protected DACL。

## 按需用户缓存

启动只加载服务端证书与 `server.json`，不扫描 `users/`。
登录或权限查询事件先按用户名查内存，未命中才读 `users/<username>/config.json`，
并加载该账号引用的证书 / CA；成功验证配置后保存到内存。
不存在、禁用、格式错误或读取失败的用户不缓存，之后仍可再次尝试。
其他账号的配置错误不会阻止服务启动。

所有未命中的本地查找共享一个异步限流入口，每秒最多发起一次，失败尝试也计入。
不同用户名共享该限制；同名并发在进入入口后再次查缓存，已被前一个请求加载则直接复用。
等待不占磁盘工作线程，读取与证书文件解析在 blocking worker 中进行。
内存命中不等待此入口，也不做周期扫描。
登录等待仍受现有 10 秒认证预算约束，排队过长会认证超时，客户端需重试。

正向缓存最多 1024 个账号，满时淘汰最久未使用的条目。
缓存没有定时过期或文件变更监听；已缓存账号的密码、权限、禁用状态和信任配置继续使用内存值，
重启服务或缓存淘汰后下次查找才读取修改。该规则也适用于删除用户文件。
服务端自身证书、日志和监听配置仍在启动时读取。

证书登录需要先完成 TLS，用户名在后续 RE 登录消息中提供。
TLS 握手校验 leaf 有效期、客户端用途和私钥持有证明；
随后 RE 登录按需加载对应用户，验证完整证书 pin，或 CA 链及该用户 URI SAN。
TLS 握手成功不代表取得 RE 用户权限，登录通过前不会开放发现、任务、文件或转发。
密码和票据认证同样要求找到可用的用户配置。

实现位于 `crates/server/src/auth/cache.rs`（缓存、限流与单用户加载）、
`auth/mod.rs`（认证和权限）与 `auth/verifier.rs`（TLS 私钥证明）。
单元测试覆盖命中绕过限流、跨用户名失败限流、并发合并和禁用账号重试；
`tests/authentication.rs` 验证运行中新增三种认证用户及未授权证书拒绝。

## 日志

独立服务默认在程序旁 `.rexed-logs/` 写轮转审计，`server.json.logging` 可配置路径、粒度和类别。
stdout 只显示客户端连接、恢复与断开；启动与错误写 stderr。
认证记录用户名、方法、对端和结果，不记录密码或票据；握手失败时可能尚无法确定 RE 用户。
配置和字段见[服务端日志](logging.md)。

## SDK 与应用连接

下列 JSON 可用于 `connect_to_executor`，或作为 RexETerm Host 的私有 `connectionProfile` 文件。
Host 覆盖其中的 `id` / `url`，可显式覆盖 `username`。
工作区只保存配置路径，不复制密码或私钥；该连接文件由用户自行限制读取权限。

```json
{
  "id": "node",
  "url": "tls://re.example.test:45957",
  "username": "alice",
  "authentication": {"method": "password", "password": "example-only"},
  "tls": {"caFile": "/private/rexed/server-cert.pem"}
}
```

`tls://` 在 TLS 内承载 REX1 帧，`wss://` 在 TLS 内承载 WebSocket。
始终验证服务端证书和主机名；`tls.serverName` 可显式指定验证身份，不提供跳过验证选项。
首次认证签发内存票据，有效期 10 分钟，后续并行链路复用票据。
失效后自动重新认证；服务重启会清空票据。
明文链路不能发送认证凭据。`--insecure-no-auth` 仅显式启用开发兼容模式；
嵌入式可信宿主也可自行创建未配置认证的 Executor。
KCP 当前没有加密实现，启用认证的 Executor 拒绝它。

## 指定证书和 CA

```bash
rexed --config-dir /private/rexed user alice --certificate client.pem
rexed --config-dir /private/rexed user bob --ca ca.pem
```

指定证书模式固定完整 leaf 证书，客户端需要对应私钥。
TLS 校验有效期、签名和客户端用途，不能只提交一个公开证书就登录。
CA 模式验证证书链，并要求 leaf 的 URI SAN 为 `urn:rexed:user:bob`；
可用 `--ca-identity` 指定其他 URI。共享 CA 下也不能任意自报其他用户名。
证书应包含 `clientAuth` EKU；中间证书按 leaf 在前放入客户端 PEM。

```json
{
  "username": "bob",
  "authentication": {"method": "ca"},
  "tls": {
    "caFile": "/private/server-ca.pem",
    "certificateFile": "/private/bob-chain.pem",
    "privateKeyFile": "/private/bob-key.pem"
  }
}
```

固定证书连接将 `authentication.method` 改为 `certificate`。

## 权限与运维

| JSON 权限 | 用户命令参数 | 授权范围 |
| --- | --- | --- |
| execute | execute | 创建、停止和移除自己的任务 |
| filesRead | files-read | 读取、搜索、下载 |
| filesWrite | files-write | 修改、删除、上传 |
| ptyView | pty-view | 查询任务和附着查看 |
| ptyControl | pty-control | PTY 输入、尺寸和主控 |
| forward | forward | 建立远端转发字节流 |
| management | management | 独立网络运维入口的全局快照 |

允许执行命令的用户可以通过该操作系统账号访问文件和网络；文件/转发开关只约束 RE 对应 API。
本地私有 Unix 运维套接字按文件权限信任调用方。
网络运维入口使用独立的 TLS/WSS 监听，并要求认证与 `management` 权限。
服务端拒绝给启用认证的 Executor 启动明文网络运维入口。
`rexctl --target tls://re.example.test:45958 --connection-profile /private/admin.json --json`
使用同样的私有连接 JSON；该用户需要 `management` 权限。

## 相关文档

- [活动资源与最终回执](lifecycle.md)
- [加密性能测试](performance-encryption.md)
- [文档索引](README.md)
