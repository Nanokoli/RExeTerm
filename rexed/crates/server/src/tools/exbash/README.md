# Exbash Tool

Tool name: `exbash`

`exbash` is PTY-backed and uses `mode` to select the operation. It is not wrapped by the small-tool timeout. Run modes use `timeout` for total process lifetime and `read_timeout` for how long to wait before returning current observed output. `attach` uses `read_timeout`; if only `timeout` is present, it is treated as the read wait capped at 10000 ms, and if both are present `read_timeout` wins.

## Input

```json
{
  "mode": "run",
  "command": "echo hello",
  "title": "Example task",
  "description": "optional display text",
  "timeout": 30000,
  "read_timeout": 1000,
  "asyncID": "rex-...",
  "text": "input to write",
  "filePath": "input.txt",
  "workdir": "subdir",
  "showRawPretty": false,
  "shell": "bash"
}
```

Both stdio and PTY tasks keep the same `title` and `description` throughout creation,
status, list, attach and stop responses. They are also included in program inventories
and management snapshots. A PTY uses its task metadata rather than a separate label.
Task metadata follows the bounded final receipt lifetime; RE keeps no persistent task history.

Modes:

| `mode` | Required fields | Behavior |
|---|---|---|
| `run` | `command` | Runs `command` directly, without shell wrapping. |
| `shell` | `command` | Runs `command` through a configured shell profile. |
| `attach` | `asyncID` | Writes optional input and returns bytes observed during this call. |
| `list` | none | Lists exbash runs. `asyncID` may filter to one run. |
| `stop` | `asyncID` | Stops a run and returns status without historical output. |
| `remove` | `asyncID` | Removes a run, stopping it first if needed. |

Common fields:

| Field | Notes |
|---|---|
| `command` | Command input, limited to 4096 bytes. |
| `title` | Task title, at most 40 Unicode characters. Omitted or blank titles become `未命名`. |
| `description` | Optional task description, at most 200 Unicode characters; defaults to an empty string. |
| `timeout` | Total runtime in ms for `run` and `shell`. Omit, `0`, or `-1` means no total timeout. For `attach`, this is accepted as a legacy read wait when `read_timeout` is absent, capped at 10000 ms. |
| `read_timeout` | Read wait in ms. Defaults to `10000`. `attach` prefers this field when both `timeout` and `read_timeout` are present. |
| `asyncID` | Run id for `list`, `attach`, `stop`, and `remove`. |
| `text` | Text input for `attach`, limited to 4096 bytes after escape parsing. Interactive programs may require control bytes; use `\n` for Enter/newline and `\u0003` for Ctrl-C/control bytes. |
| `filePath` | File input for `attach`, limited to 4096 bytes. |
| `workdir` | Optional cwd for `run` and `shell`. Relative paths resolve against the top-level RPC `directory`. |
| `showRawPretty` | Adds `rawPretty` to attach metadata. |
| `shell` | Shell profile for `mode: "shell"`. Empty or omitted uses client preferences when routed through Client. Relative candidates resolve on the Executor, not on the client. `shellCommand` is the internal prepared invocation; direct legacy calls without it use immutable server startup defaults. |

## Mode `run`

Completed request:

```json
{
  "id": 20,
  "tool": "exbash",
  "params": {
    "mode": "run",
    "command": "printf hello",
    "read_timeout": 1000
  }
}
```

Completed response:

```json
{
  "id": 20,
  "ok": true,
  "result": {
    "metadata": {
      "exitCode": 0,
      "output": "hello"
    },
    "output": { "message": "", "text": "hello", "info": "" }
  },
  "executor": "local"
}
```

Detached request:

```json
{
  "id": 30,
  "tool": "exbash",
  "params": {
    "mode": "run",
    "command": "bash -lc 'printf detached-text; sleep 10'",
    "read_timeout": 200
  }
}
```

Detached response:

```json
{
  "id": 30,
  "ok": true,
  "result": {
    "metadata": {
      "asyncID": "rex-1780467386041-1",
      "command": "bash -lc 'printf detached-text; sleep 10'",
      "cwd": "/runtime/workspaces/maintainer/RemoteExecutor",
      "description": "bash -lc 'printf detached-text; sleep 10'",
      "detached": true,
      "pid": 57687,
      "startedAt": 1780467386042,
      "state": "running",
      "timeout": null,
      "totalOutput": 13
    },
    "output": { "message": "rex-1780467386041-1 detached", "text": "detached-text", "info": "" }
  },
  "executor": "local"
}
```

## Mode `shell`

Request:

```json
{
  "id": 40,
  "tool": "exbash",
  "params": {
    "mode": "shell",
    "command": "printf shell",
    "read_timeout": 1000
  }
}
```

Response:

```json
{
  "id": 40,
  "ok": true,
  "result": {
    "metadata": {
      "exitCode": 0,
      "output": "shell"
    },
    "output": { "message": "", "text": "shell", "info": "" }
  },
  "executor": "local"
}
```

## Mode `attach`

Request:

```json
{
  "id": 24,
  "tool": "exbash",
  "params": {
    "mode": "attach",
    "asyncID": "rex-1780467225438-2",
    "text": "doc input\\n",
    "read_timeout": 100
  }
}
```

Response:

```json
{
  "id": 24,
  "ok": true,
  "result": {
    "metadata": {
      "asyncID": "rex-1780467225438-2",
      "outputBytes": 22,
      "source": "text",
      "wrote": 10
    },
    "output": { "message": "", "text": "doc input\ndoc input", "info": "" }
  },
  "executor": "local"
}
```

Stopped attach responses use the same JSON wrapper and include `state` and `exitCode` in `result.metadata`; the status text is in `result.output.message`.

## Mode `list`

Request:

```json
{
  "id": 22,
  "tool": "exbash",
  "params": {
    "mode": "list"
  }
}
```

Response:

```json
{
  "id": 22,
  "ok": true,
  "result": {
    "metadata": {
      "runs": [
        {
          "asyncID": "rex-1780467225438-2",
          "command": "bash -lc read line; echo $line; sleep 5",
          "cwd": "/runtime/workspaces/maintainer/RemoteExecutor",
          "description": "bash -lc read line; echo $line; sleep 5",
          "pid": 55303,
          "startedAt": 1780467225440,
          "state": "running",
          "timeout": null,
          "totalOutput": 0
        }
      ]
    },
    "output": { "message": "", "text": "rex-1780467225438-2 running totalOutput=0 command=bash -lc read line; echo $line; sleep 5", "info": "" }
  },
  "executor": "local"
}
```

Stopped runs include `exitCode` and `endedAt`. `error` is omitted unless an error is recorded.

## Mode `stop`

Request:

```json
{
  "id": 31,
  "tool": "exbash",
  "params": {
    "mode": "stop",
    "asyncID": "rex-1780467386041-1"
  }
}
```

Response:

```json
{
  "id": 31,
  "ok": true,
  "result": {
    "metadata": {
      "asyncID": "rex-1780467386041-1",
      "command": "sleep 10",
      "cwd": "/runtime/workspaces/maintainer/RemoteExecutor",
      "description": "sleep 10",
      "endedAt": 1780467404893,
      "exitCode": "stopped",
      "pid": 57687,
      "startedAt": 1780467386042,
      "state": "stopped",
      "timeout": null,
      "totalOutput": 0
    },
    "output": { "message": "", "text": "", "info": "" }
  },
  "executor": "local"
}
```

## Mode `remove`

Request:

```json
{
  "id": 26,
  "tool": "exbash",
  "params": {
    "mode": "remove",
    "asyncID": "rex-1780467225438-2"
  }
}
```

Response:

```json
{
  "id": 26,
  "ok": true,
  "result": {
    "metadata": {
      "ok": true
    },
    "output": { "message": "", "text": "ok", "info": "" }
  },
  "executor": "local"
}
```

## Raw terminal output

rexed forwards raw PTY bytes without a VT parser or historical output buffer. New observers only receive output after subscribing. Each viewer has a 4 MiB byte-bounded in-flight queue that coalesces already available bytes into at most 64 KiB chunks; no fixed batching delay is added. Slow viewers fail independently. Exbash collects bytes only for its current request window; subsequent attach/stop cannot recover earlier output. Rendering belongs to the client, and screen recovery is outside this throughput prototype.

Exbash responses declare `metadata.terminalOutput.format: raw-pty-v1` and terminal dimensions. `output.text` is a lossy UTF-8 projection retaining terminal escapes. RexETerm AppCore renders observation results for MCP/CLI; completed runs keep their collected transcript (`view: stream`).

A known PTY ID permits same-RE-username clients to observe without input. Inventories remain client-scoped. Cross-client viewers cannot write, resize, request control, or become a controller on disconnect. App-embedded MCP shares the App client identity.

## stdio 与 PTY

Exbash 接受 `io: "stdio" | "pty"`，默认 `pty`。`run/shell` 与 I/O 选择独立。
stdio 的 stdin/stdout/stderr 使用管道，首次支持 `stdin` 和 `keepStdin`，
后续 attach 支持 `closeStdin`。完整行为、容量和生命周期见
[Exbash 模块](../../../../../docs/modules/exbash.md)。
