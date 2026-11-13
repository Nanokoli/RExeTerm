#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
check_tree() {
    local role="$1"
    shift
    local tree
    tree="$(cargo tree --locked -e normal --prefix none --format '{p}' "$@")"
    while IFS= read -r dependency; do
        case "$dependency" in
            remote_executor_server\ *|remote_executor_terminal\ *|portable-pty\ *) echo "Execution dependency in remote-only $role: $dependency" >&2; return 1 ;;
        esac
        case "$role:$dependency" in
            appcore:axum\ *|appcore:rexeterm-daemon\ *|appcore:rexeterm-mcp-*|appcore:rexeterm-cli\ *|appcore:clap\ *) echo "Host/adapter dependency in AppCore: $dependency" >&2; return 1 ;;
            application:axum\ *|application:tauri\ *|application:rexeterm-daemon\ *|application:rexeterm-mcp-*|application:rexeterm-http-adapt\ *|application:rexeterm-cli\ *) echo "Host/adapter dependency in Application: $dependency" >&2; return 1 ;;
            rest:rexeterm-daemon\ *|rest:tauri\ *|rest:rexeterm-mcp-*) echo "Host/MCP dependency in REST adapter: $dependency" >&2; return 1 ;;
            cli-client:rexe-client\ *|cli-client:rexeterm-appcore\ *|cli-client:rexeterm-daemon\ *|cli-client:remote_executor_*|cli-client:axum\ *|cli-client:reqwest\ *|cli-client:hyper\ *|cli-client:rexeterm-http-adapt\ *) echo "RE/core dependency in HTTP CLI: $dependency" >&2; return 1 ;;
            daemon:axum\ *|daemon:rexeterm-mcp-*|daemon:rexeterm-cli\ *) echo "Adapter dependency in daemon: $dependency" >&2; return 1 ;;
            mcp-stdio:axum\ *|mcp-stdio:rexeterm-daemon\ *|mcp-stdio:rexeterm-mcp-http-adapt\ *) echo "Host/HTTP dependency in stdio adapter: $dependency" >&2; return 1 ;;
            http:rexeterm-daemon\ *|http:rexeterm-mcp-stdio-adapt\ *) echo "Host/stdio dependency in HTTP adapter: $dependency" >&2; return 1 ;;
            cli:axum\ *|cli:rexeterm-mcp-*|cli:rexeterm-daemon\ *) echo "MCP/host dependency in CLI adapter: $dependency" >&2; return 1 ;;
            app:rexeterm-mcp-stdio-adapt\ *) echo "stdio adapter dependency in App host: $dependency" >&2; return 1 ;;
            stdio-product:axum\ *|stdio-product:rexeterm-app\ *|stdio-product:rexeterm-mcp-http-adapt\ *) echo "App/HTTP dependency in stdio product: $dependency" >&2; return 1 ;;
        esac
    done <<< "$tree"
    echo "Dependency boundary passed: $role $*"
}
check_tree appcore -p rexeterm-appcore --all-features
check_tree application -p rexeterm-application
check_tree rest -p rexeterm-http-adapt
check_tree cli-client -p rexeterm-cli --no-default-features --features ipc-client
check_tree daemon -p rexeterm-daemon
check_tree mcp-stdio -p rexeterm-mcp-stdio-adapt
check_tree http -p rexeterm-mcp-http-adapt
check_tree cli -p rexeterm-cli --no-default-features --features stdio,admin
check_tree cli-host-product -p rexeterm-cli --no-default-features --features daemon
cargo clippy --locked -p rexeterm-cli --no-default-features --features daemon --all-targets -- -D warnings
check_tree app -p rexeterm-app
check_tree app -p rexeterm-desktop --features desktop
check_tree stdio-product -p rexeterm --no-default-features --features mcp-stdio
for features in client stdio mcp-stdio stdio,mcp-stdio app http mcp-http admin stdio,mcp-stdio,kcp; do
    check_tree remote -p rexeterm --no-default-features --features "$features"
    cargo clippy --locked -p rexeterm --no-default-features --features "$features" --all-targets -- -D warnings
done
cargo build --locked -p rexeterm --no-default-features --features stdio,mcp-stdio,admin --bins
cargo test --locked -p rexeterm --no-default-features --features stdio,mcp-stdio --test remote_only
