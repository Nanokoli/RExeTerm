# 客户端配置布局

## 三方职责

| 角色 | 负责 | 不负责 |
| --- | --- | --- |
| `rexetermd`（宿主） | 维护到 rexed 的连接、把 rexed 的流持续接住、维护到 CLI 的 IPC | 不读取任何偏好文件；不解析 shell；不写项目目录 |
| `rexeterm` | 读取与写入 `.rexeterm/config-dir/preferences.json`、解析出完整 shell 命令并随任务下发 | 不管远端连接生命周期 |
| `rexed` | 远端执行、连续送流 | 不管客户端偏好 |

## 配置与绑定

配置只属于 CLI，固定在当前工作目录的 `.rexeterm/config-dir/`，不接受任何参数覆盖。
工作目录只能由外部决定：`cd`，或设 `REXETERM_CWD`（启动时切换，失败即退出 2）；
`--rcwd` 只影响远端工作目录和它对应的偏好层。

宿主是全机唯一的：binding 写在二进制旁 `.rexeterm-socket/binding`，`service create`
发现已有存活宿主时直接复用（输出 `reused: true`），不会再起第二个。宿主的私有状态
（`host/` 下的锁、日志、workspace）也在 `.rexeterm-socket/` 里，不落在任何项目中。
`--registry-dir` 改变这些运行记录与 binding 的位置。

因此：项目里只会出现 `.rexeterm/config-dir/`（CLI 的偏好），宿主与连接完全在项目之外。
CLI 在任意目录都能用同一宿主；偏好按目录各自读取，改文件立即生效（宿主不缓存）。

完整 App 只读取自身二进制旁的 `.rexeterm/config-dir/`，不跟随 cwd，不读取 APPDATA，
也不再用 `REXETERM_CONFIG_PATH` 选择其他工作区。MCP 独立入口默认采用程序旁配置，
保留其显式客户端配置文件参数。未开启持久化的嵌入 AppCore 仍可完全在内存运行。

## 按执行器分组

```text
.rexeterm-config/
  workspace.json                 # Host ID 索引、最后选择
  preferences.json               # 客户端全局默认值
  preferences.task-history.json  # 宿主有限任务结果，非输出日志
  executors/
    <SHA-256(执行器 ID)>/
      executor.json              # ID、名称、地址、起始目录、选用用户与认证配置路径
      connection.json            # 实际 SDK 连接选项及用户配置引用
      preferences.json           # 该执行器默认 shell、精确目录覆盖
      identities.json            # 此执行器各端点/RE 用户的客户端恢复凭据
      identities.lock
      management/               # GUI 独立运维认证，布局同 users/<hash>/authentication.json 与 keys/
        users/
          <SHA-256(RE 用户名)>/
            authentication.json
            keys/
      users/
        <SHA-256(RE 用户名)>/
          authentication.json    # 登录方法、密码或证书选项
          keys/
            ca.pem
            client-cert.pem
            client-key.pem
```

目录组件使用 ID 的完整 SHA-256，避免路径穿越、Windows 保留名称、大小写冲突和路径组件过长；
文件内保留原始执行器 ID 和用户名。执行器级配置与用户配置均属于客户端，
不修改远端 rexed 的用户、权限或认证政策。

保存 Host 时，将所选 connection profile 导入相应执行器/用户目录，
同时复制引用的 TLS CA、客户端证书和私钥；相对 TLS 路径相对于源 profile 文件解析。
原始外部 profile 和密钥不删除。后续连接使用导入副本；外部原文件改变后需重新导入。
普通偏好工具与工作区 API 不返回密码、私钥或恢复 secret。

配置以私有目录和原子文件替换保存：Unix 目录 0700、文件 0600；
Windows 使用当前用户的受保护 DACL。程序目录不可写时 App 明确报错，不静默改用 cwd。
删除 Host 仅从索引移除连接，保留该执行器的用户、密钥和偏好，避免重建 Host 时丢失认证数据。

## GUI 的 Host 运维设置

`executor.json` 的可选 `management` 对象保存 `target` 和 `connectionProfile`。
`target` 留空时对网络业务地址尝试端口 + 1，保留协议、IPv6 和 WS 路径；不能推导本地 socket。
该候选不代表服务端已监听；rexed 默认仍使用 Unix socket，网络管理监听需显式配置。
`connectionProfile` 留空时继承 Host 业务 profile 和用户名；指定时使用该文件自身的用户名及认证。

独立 profile 及证书/密钥导入 `executors/<hash>/management/users/<user-hash>/`，
同名用户的业务凭据不会被管理员凭据覆盖。设置随 Host 持久保存，重启可恢复；旧 Host 缺省无独立设置。
普通 HTTP/MCP 不增加全局运维查询入口。

## 兼容与迁移

同一显式配置根目录内的旧 workspace hosts 数组会转为 ID 索引和执行器文件。
旧偏好文件里的 executors 覆盖会迁入各执行器目录；全局文件保留客户端默认值。
旧集中 identities 文件在首次连接相应执行器时读取并复用原凭据，
新凭据写入执行器目录。旧凭据文件保留，避免其他尚未重连的执行器失去身份。

不自动搜索旧 APPDATA、任意工作目录或其他程序目录来合并配置。
旧配置需要在同一配置根目录内迁移，App 部署时应先将旧配置复制到程序旁的新目录。
显式宿主配置目录与 UUID64 运行注册表是两类存储；注册表仍按操作系统启动期隔离。

## 相关文档

- [Agent CLI](agent-cli.md)
- [客户端偏好](modules/preferences.md)
- [HTTP 宿主与绑定](http-api.md)
