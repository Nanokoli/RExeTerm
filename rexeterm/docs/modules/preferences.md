# 客户端偏好

## 范围

RexETerm appcore 保存 shell 偏好，按执行器 ID 和目录选择有效配置；Executor 解析远端候选程序并执行。
独立终端的启动默认值保留在执行器，运行期间不扫描目录配置、不写回配置、不提供重载工具。

## 内部结构

### 配置模型

`ClientSettings` 定义客户端通用值、执行器覆盖和目录覆盖。Shell profile 及请求中的 `ShellCommand` 定义在共享契约中。

输入与输出：本地 JSON → `ClientSettings`；选定 profile → 候选程序和已展开参数。

实现位置：[schema.rs](../../crates/appcore/src/preferences/schema.rs)、[shell.rs](../../../RemoteExecutor/crates/protocol/src/shell.rs)。

### 配置分层

按“客户端通用值 → 执行器默认值 → 该执行器的目录覆盖”应用配置。`default`、`interactive` 按字段覆盖，`profiles` 按名称替换完整 profile。

目录键是请求顶层 `directory` 的远端路径字符串，精确匹配。客户端不访问该目录、不解析符号链接，也不自动继承父目录、规范化斜杠或大小写。建议调用方统一使用绝对路径；Exbash 的 `workdir` 只改变子进程目录，不改变配置键。
同一路径在不同执行器下可使用不同配置。

实现位置：`ClientSettings::effective`。

### 本地持久化

`ClientSettingsStore` 保存本地文件路径、有效快照与最近重载错误。克隆共享快照；新建的独立 store 不共享状态。
`set_default_shell` 先验证配置，再通过同目录临时文件替换持久化文件；保存失败时内存不变。
只验证 profile 配置，不要求客户端装有远端 shell；程序存在性由 Executor 在执行时检查。

实现位置：[store.rs](../../crates/appcore/src/preferences/store.rs)。

### 显式重载

`request_reload` 重新读取整个客户端配置文件；成功时替换快照，失败时保留上次有效配置并记录错误。`list_shells` 的 `settingsError` 可读取该错误。
磁盘修改不会自动生效。文件不存在时使用内置默认值；内存 store 的重载成功返回当前配置。
请求中的执行器与目录只选择返回的有效视图，不限制重载文件的范围。

### 客户端工具

`list_shells` 列出客户端配置的 profile，不探测远端已安装的 shell。
`set_default_shell` 修改选定执行器的默认值；传入 `directory` 时只修改该目录覆盖。省略执行器时使用当前默认节点；尚无默认节点且不传目录时修改通用值。显式 ID 可在节点离线时配置；目录覆盖要求有执行器 ID。

三个工具均在 AppCore 本地完成，返回 `scope: "client"`、执行器、目录、有效 profile、`settingsPath` 及 `settingsError`。执行器不再接收这些管理工具。

实现位置：[tools.rs](../../crates/appcore/src/preferences/tools.rs)、[AppCore](../../../RemoteExecutor/crates/client/src/lib.rs)。

### 命令规格准备

Client 为 `exbash mode: "shell"` 选择显式或默认 profile。`auto` 使用目标节点的系统选择 Windows 的 `powershell` 或其他系统的 `bash`；系统未知时要求显式选择。
Client 展开 `{command}`，没有占位符时将命令追加到参数末尾，再发送 `shellCommand`。
Executor 按候选顺序，在最终工作目录或自己的 PATH 中解析程序，生成 `CommandSpec`。
相对候选不会相对于客户端配置文件解释。

实现位置：`ClientSettingsStore::shell_command`、[run.rs](../../../RemoteExecutor/crates/server/src/tools/exbash/run.rs)。

### 执行器启动默认值

`--settings` 指定执行器启动文件，默认是可执行文件旁的 `.re-setting.json`。`SettingsStore` 只在构造时读取并持有不可变快照，供初始交互终端和未携带 `shellCommand` 的直接调用使用。
这些调用先从工作目录查找相对候选，再从启动配置目录查找；裸名称使用执行器 PATH。
已移除工作目录配置扫描、目录缓存、配置写回和服务器 reload API。

实现位置：[server store.rs](../../../RemoteExecutor/crates/server/src/settings/store.rs)、[shell.rs](../../../RemoteExecutor/crates/server/src/settings/shell.rs)。

## 配置示例

```json
{
  "version": 1,
  "shells": { "default": "auto" },
  "executors": {
    "linux-box": {
      "shells": { "default": "bash" },
      "directories": {
        "/workspace/project": {
          "default": "python",
          "profiles": {
            "python": {
              "candidates": [".venv/bin/python", "python3", "python"],
              "commandArgs": ["-c", "{command}"],
              "interactiveArgs": []
            }
          }
        }
      }
    }
  }
}
```

内置 profile 为 `bash`、`python`、`node`、`powershell`。客户端 `interactive` 字段只保留配置表达，不会修改已经启动的终端；独立执行器的初始终端使用其启动默认值。

原始 stdio 请求：

```json
{"id":1,"tool":"set_default_shell","executor":"linux-box","directory":"/workspace/project","params":{"shell":"python"}}
```

MCP 使用相同的 `shell`、`directory`，目标字段为 `targetExecutor`。

## 启动与迁移

CLI 宿主默认使用 cwd 下 `.rexeterm/config-dir`；App 使用自身二进制旁同名目录。
独立 MCP 入口保留显式 `--client-settings`，默认在程序旁。全局与执行器配置分文件保存，
见 [客户端配置布局](../client-config.md)。
Rust 库默认使用独立内存 store；持久化需显式提供 `DaemonOptions.client_settings_path` 或 `AppCore::with_settings_store`。
`--no-local` 关闭本地执行环境，仍会读取客户端偏好文件。

旧执行器和工作目录 `.re-setting.json` 中的偏好需手动迁入上述 `executors` 结构。上层节点注册配置仍由宿主负责。
新版节点通过 `clientShellCommand` 能力声明支持。未声明的节点仍可使用其他工具；Client 的 shell 调用返回升级提示，避免忽略客户端选择。

## 相关文档

- [AppCore](../../../RemoteExecutor/docs/modules/client.md)
- [Executor](../../../RemoteExecutor/docs/modules/executor.md)
- [构建与 API 迁移](../../../RemoteExecutor/docs/build-and-api.md)
- [文档索引](../../../RemoteExecutor/docs/README.md)
