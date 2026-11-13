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
            rexeterm*\ *|axum\ *|crossterm\ *) echo "Application dependency in RE: $dependency" >&2; return 1 ;;
        esac
        case "$role:$dependency" in
            terminal:remote_executor_data\ *|terminal:remote_executor_protocol\ *|terminal:remote_executor_transport\ *|terminal:remote_executor_server\ *|terminal:rexe-client\ *|terminal:tokio-tungstenite\ *) echo "Protocol dependency in terminal: $dependency" >&2; return 1 ;;
            transport:remote_executor_data\ *|transport:remote_executor_protocol\ *|transport:rexe-client\ *|transport:remote_executor_server\ *|transport:remote_executor_terminal\ *|transport:serde_json\ *) echo "Data dependency in transport: $dependency" >&2; return 1 ;;
            data:rexe-client\ *|data:remote_executor_server\ *|data:remote_executor_terminal\ *|data:portable-pty\ *) echo "Execution dependency in data: $dependency" >&2; return 1 ;;
            client:remote_executor_server\ *|client:remote_executor_terminal\ *|client:portable-pty\ *|client:clap\ *|client:unicode-width\ *|client:diffy\ *|client:file-id\ *|client:globset\ *|client:grep-*|client:ignore\ *) echo "Execution/application dependency in RE client: $dependency" >&2; return 1 ;;
            terminal:vt100\ *|terminal:vte\ *|server:vt100\ *|server:vte\ *) echo "Terminal parser dependency in executor: $dependency" >&2; return 1 ;;
            server:rexe-client\ *) echo "Client dependency in server: $dependency" >&2; return 1 ;;
            protocol:remote_executor_data\ *|protocol:remote_executor_terminal\ *|protocol:remote_executor_transport\ *|protocol:remote_executor_server\ *|protocol:rexe-client\ *|protocol:tokio\ *) echo "Runtime dependency in protocol: $dependency" >&2; return 1 ;;
        esac
    done <<< "$tree"
    echo "Dependency boundary passed: $role $*"
}
check_tree transport -p remote_executor_transport
check_tree data -p remote_executor_data
check_tree terminal -p remote_executor_terminal
check_tree protocol -p remote_executor_protocol
check_tree client -p rexe-client
check_tree server -p remote_executor_server
for features in client server client,server client,kcp server,kcp; do
    cargo clippy --locked -p remote_executor --no-default-features --features "$features" --all-targets -- -D warnings
done
cargo clippy --locked -p remote_executor --no-default-features --all-targets -- -D warnings
cargo build --locked -p rexe-client
cargo build --locked -p remote_executor --no-default-features --features server --bin rexed
