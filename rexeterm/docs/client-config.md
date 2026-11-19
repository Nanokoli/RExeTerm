# 客户端配置布局

## CLI 的项目目录

从当前工作目录读取配置；可用 `cd` 或 `REXETERM_CWD` 选择本机目录，`--rcwd` 指远端目录。

```text
<项目>/
  RexETermHostUUID.txt                      # 当前项目选中的宿主 ID
  .rexeterm/config-dir/
    preferences.json                      # 客户端全局 shell 默认值
    executors/
      RB5/
        executor.json                     # 一个保存的执行器：ID、名称、地址、浏览起点
        preferences.json                  # 此执行器的可选 shell 覆盖
    credentials/
      rb5-codex.json -> /private/HostNet/RB5/profile.json
  .rexeterm-config/                        # daemon 的私有状态；与 CLI 保存条目独立
```

`-e -list_local`（`executor host list`）和 `-cr -list_local`（`credential list`）显示保存目录及条目位置。`-e -list_online` 显示选中宿主的活动连接。`-ec rb5-codex@RB5`（`executor connect rb5-codex@RB5`）使用 `@` 后保存的主机名 `RB5` 作为在线 ID。加上 `as rb5_codex_exe` 时，后续命令使用这个在线别名；本地条目仍叫 `RB5`。

凭证 profile 私下保存 `username`、`authentication`、`tls`；下面是证书认证的结构示例：

```json
{
  "username": "client-user",
  "authentication": {"method": "certificate"},
  "tls": {
    "caFile": "server-cert.pem",
    "certificateFile": "client-cert.pem",
    "privateKeyFile": "client-key.pem"
  }
}
```

相对 PEM 路径以实际 profile 文件的目录为准（支持符号链接）。带 `-cr` 时从所选凭证读取用户名、认证和 TLS，连接地址、执行器 ID 及其它选项来自执行器条目。旧条目的 `connectionProfile` 是可选默认凭证来源；其中的 `id`、`url` 等可由条目覆盖。profile 自身的 `connectionProfile` 不会再递归读取。普通 CLI 输出不含认证对象、密码或私钥文件字段；`credential list` 只读文件元数据和链接目标。连接失败时可按提示核对具体字段和本机文件路径。

已保存的旧 `executors/<ID>.json` 仍可读。`executor host edit` 为可移植 ID 写入新目录，优先读取新条目；原文件留作回退。修改时用 `--options` 设置公开配置字段，不把密码或 TLS 对象传入命令行。`executor host remove` 删除对应配置条目，保留独立凭证。旧哈希目录下的 CLI 偏好仍能读取，后续修改写入新目录。

宿主运行记录、私有 IPC token 与套接字由 `--registry-dir`（默认程序旁 `.rexeterm-socket/`）管理。固定相同的 registry 才能解释 `RexETermHostUUID.txt` 中的 ID。详情见 [CLI 本机 IPC](cli-ipc.md)。

## App / MCP 的配置

完整 App 和 MCP 独立入口默认使用程序旁 `.rexeterm-config/`，MCP 仍可显式传客户端配置文件。App 与 MCP 不使用 CLI 的绑定或凭证名。同一配置根下，非凭证数据与凭证分别保存：

```text
.rexeterm-config/
  workspace.json                          # Host ID 索引和最后选择
  preferences.json                        # 全局偏好
  preferences.task-history.json           # 有限的任务结果
  executors/<SHA-256(执行器 ID)>/
    executor.json                         # 保存的 Host 与外部 profile 路径
    connection.json                       # 已准备的连接设置，凭证只保留引用
    preferences.json                      # 此执行器的 shell 偏好
  credentials/<SHA-256(执行器 ID)>/
    identities.json                       # RE 会话恢复凭据
    identities.lock
    users/<SHA-256(RE 用户名)>/
      authentication.json                # 仅保存直接传给 AppCore 的内联认证配置
```

哈希将每个 ID 映射到一个目录，避免路径穿越、Windows 保留名和大小写碰撞；同一物理机器的不同 ID 仍分别保存。在 App 的 Host 与运维设置中选择外部 profile 路径，连接时读取原文件并解析 TLS 路径。这样可以直接指向 HostNet，也可在自己维护的 `credentials/` 中放私有 profile；证书和私钥不再自动复制。GUI 的独立运维 profile 保持和业务 profile 分开；没有独立 profile 时沿用业务 profile。

已有 Host 指向旧的 `executors/<hash>/users/…/authentication.json` 时仍可连接。新会话恢复凭据写入 `credentials/`，优先复用旧的执行器或全局身份记录。运行时不会删除旧目录，也不会按 URL 自动合并别名。保存或删除 Host 不会删除外部 profile。私有配置使用原子替换，Unix 目录 0700、文件 0600；Windows 使用当前用户 ACL。
