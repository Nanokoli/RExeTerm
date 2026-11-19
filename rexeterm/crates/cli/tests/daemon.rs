#![cfg(all(feature = "daemon", feature = "local"))]
use rexeterm_cli::ipc_client::IpcClient;
use rexeterm_cli_host::{read_binding, Registry, BINDING_FILE};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
struct Installed {
    root: tempfile::TempDir,
    binary: PathBuf,
    other: PathBuf,
}
impl Installed {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("install");
        std::fs::create_dir(&install).unwrap();
        let binary = install.join(if cfg!(windows) {
            "rexeterm.exe"
        } else {
            "rexeterm"
        });
        // One program: the host role is spawned from this same file.
        std::fs::copy(env!("CARGO_BIN_EXE_rexeterm"), &binary).unwrap();
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        Self {
            root,
            binary,
            other,
        }
    }
    fn command(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .env_remove("REXETERM_CWD")
            .current_dir(cwd);
        for attempt in 0..10 {
            match command.output() {
                // A parallel test can fork while the copied executable is open
                // for writing and briefly hold that descriptor before exec.
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 9 =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                output => return output.unwrap(),
            }
        }
        unreachable!()
    }
    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.command(cwd, args);
        assert!(
            out.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn registry(&self) -> Registry {
        Registry::open(Some(self.binary.parent().unwrap().join(".rexeterm-socket"))).unwrap()
    }
    fn id(&self, cwd: &Path) -> String {
        read_binding(cwd).unwrap().unwrap().0
    }
    fn client(&self, id: &str) -> IpcClient {
        let record = self.registry().resolve(id).unwrap();
        IpcClient::new(
            record.target.unwrap(),
            std::fs::read_to_string(record.token_file.unwrap()).unwrap(),
            id,
        )
        .unwrap()
    }
}
impl Drop for Installed {
    fn drop(&mut self) {
        if let Ok(records) = self.registry().records() {
            for record in records {
                let _ = self.command(
                    &self.other,
                    &["--service-id", &record.id, "service", "stop"],
                );
            }
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn pty_screen_and_png_keep_the_menu_after_incremental_input() {
    let h = Installed::new();
    let cwd = h.root.path();
    h.ok(cwd, &["service", "create", "--local"]);
    let command = r#"stty -echo; printf '\033[?1049h\033[2J\033[1;1HMenu header\033[2;1H\033[7mFirst\033[0m\033[3;1HSecond\033[4;1HQuit'; IFS= read -r key; printf '\033[2;1HFirst\033[3;1H\033[7mSecond\033[0m'; IFS= read -r key; printf '\033[?1049l\377END'; exit 7"#;
    let initial = b"\x1b[?1049h\x1b[2J\x1b[1;1HMenu header\x1b[2;1H\x1b[7mFirst\x1b[0m\x1b[3;1HSecond\x1b[4;1HQuit";
    let delta = b"\x1b[2;1HFirst\x1b[3;1H\x1b[7mSecond\x1b[0m";
    let start = h.ok(
        cwd,
        &[
            "--pty",
            "30x5",
            "-t",
            "1000",
            "--cmd",
            command,
            "--png",
            "before.png",
            "--raw",
            "before.bin",
        ],
    );
    let id = start
        .lines()
        .find_map(|line| line.strip_prefix(">asyncID: "))
        .unwrap()
        .trim_matches('`');
    assert!(
        start.contains("Menu header\nFirst\nSecond\nQuit"),
        "{start}"
    );
    assert_eq!(std::fs::read(cwd.join("before.bin")).unwrap(), initial);
    let updated = h.ok(
        cwd,
        &[
            "-A",
            id,
            "--text",
            "next\n",
            "-t",
            "500",
            "--png",
            "after.png",
            "--raw",
            "after.bin",
        ],
    );
    assert!(
        updated.contains("Menu header\nFirst\nSecond\nQuit"),
        "{updated}"
    );
    let expected = [initial.as_slice(), delta.as_slice()].concat();
    assert_eq!(std::fs::read(cwd.join("after.bin")).unwrap(), expected);
    // The next CLI process sees the same selected row without consuming the screen.
    let tui = h.ok(cwd, &["-A", id, "--tui"]);
    assert!(
        tui.contains("Menu header\nFirst\n\x1b[0;7mSecond\x1b[0m\nQuit"),
        "{tui:?}"
    );
    h.ok(
        cwd,
        &[
            "-A",
            id,
            "--since",
            &initial.len().to_string(),
            "--raw",
            "delta.bin",
        ],
    );
    assert_eq!(std::fs::read(cwd.join("delta.bin")).unwrap(), delta);

    let pixels = |path: &str| {
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(cwd.join(path)).unwrap(),
        ));
        let mut reader = decoder.read_info().unwrap();
        let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut bytes).unwrap();
        (info.width as usize, info.height as usize, bytes)
    };
    let (width, height, before) = pixels("before.png");
    let (_, _, after) = pixels("after.png");
    let row_height = (height - 12) / 5;
    let header = (6 + row_height) * width * 4;
    assert_eq!(&before[..header], &after[..header]);
    let row_sample = |row: usize| ((6 + row * row_height) * width + 6) * 4;
    assert_eq!(
        before[row_sample(1)],
        0xe6,
        "initial selection uses reverse-video"
    );
    assert_eq!(
        after[row_sample(1)],
        0x10,
        "initial row is no longer selected"
    );
    assert_eq!(
        after[row_sample(2)],
        0xe6,
        "incremental selection makes it to PNG"
    );

    let exit = h.ok(cwd, &["-A", id, "--text", "quit\n", "-t", "1000"]);
    assert!(exit.contains(">exitCode: 7"), "{exit}");
    h.ok(cwd, &["-A", id, "--read", "--raw", "complete.bin"]);
    assert_eq!(
        std::fs::read(cwd.join("complete.bin")).unwrap(),
        [expected.as_slice(), b"\x1b[?1049l\xffEND"].concat()
    );
}

#[tokio::test]
async fn project_binding_selects_the_service_and_transfers_use_the_calling_directory() {
    let h = Installed::new();
    let cwd = h.root.path();
    h.ok(cwd, &["service", "create", "--local"]);
    let id = h.id(cwd);
    let client = h.client(&id);
    let info = client.call("info", json!({})).await.unwrap();
    assert_eq!(info["role"], "rexetermd");
    assert_eq!(read_binding(cwd).unwrap().unwrap().0, id);
    // The old shared binding and the parent's binding are both ignored.
    std::fs::write(h.registry().base_directory().join("binding"), &id).unwrap();
    assert!(h.ok(&h.other, &["info"]).contains("bindingSource**: none"));
    assert!(read_binding(&h.other).unwrap().is_none());
    // An explicit ID does not even read a malformed local binding.
    let binding = h.other.join(BINDING_FILE);
    std::fs::write(&binding, "invalid-id").unwrap();
    assert!(h
        .ok(&h.other, &["--service-id", &id, "info"])
        .contains("bindingSource**: explicit"));
    assert_eq!(std::fs::read_to_string(&binding).unwrap(), "invalid-id");
    h.ok(&h.other, &["service", "bind", &id]);
    assert!(h
        .ok(&h.other, &["-e", "-list_online"])
        .contains("id**: local"));
    assert!(h
        .ok(cwd, &["executor", "list"])
        .contains("default**: local"));
    assert!(h
        .ok(
            cwd,
            &[
                "api",
                "rpc",
                "--body",
                r#"{"tool":"list_executor","params":{}}"#
            ]
        )
        .contains("id**: local"));
    h.ok(cwd, &["service", "create", "--local"]);
    assert_eq!(h.id(cwd), id);
    let record = h.registry().resolve(&id).unwrap();
    #[cfg(unix)]
    {
        assert!(record.target.as_ref().unwrap().starts_with("unix://"));
        assert!(Path::new(
            record
                .target
                .as_ref()
                .unwrap()
                .trim_start_matches("unix://")
        )
        .starts_with(h.binary.parent().unwrap().join(".rexeterm-socket")));
    }
    #[cfg(windows)]
    assert!(record.target.as_ref().unwrap().starts_with("pipe://"));
    let wrong = IpcClient::new(record.target.clone().unwrap(), "0".repeat(64), &id).unwrap();
    assert!(wrong.call("shutdown", json!({})).await.is_err());
    client.verify_service().await.unwrap();
    let other_id = "1234567890abcdef";
    let wrong = IpcClient::new(
        record.target.clone().unwrap(),
        std::fs::read_to_string(record.token_file.clone().unwrap()).unwrap(),
        other_id,
    )
    .unwrap();
    assert!(wrong.call("info", json!({})).await.is_err());
    assert!(h
        .ok(
            cwd,
            &["-e", "local", "-t", "2000", "-cmd", "echo single-binary"]
        )
        .contains("single-binary"));
    let source = cwd.join("source.bin");
    let remote = cwd.join("remote.bin");
    let copy = h.other.join("copy.bin");
    let bytes = (0..1024 * 1024 + 17)
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    std::fs::write(&source, &bytes).unwrap();
    let digest = "f4bf9b8dec6e3e28b2ec33266145d244fe97a6bce1ec054ace053bab67d2ef9a";
    h.ok(
        cwd,
        &[
            "file",
            "upload",
            remote.to_str().unwrap(),
            source.to_str().unwrap(),
            "--title",
            "传输",
            "--sha256",
            digest,
        ],
    );
    std::fs::write(&copy, b"previous destination").unwrap();
    let out = h.command(
        &h.other,
        &[
            "file",
            "download",
            remote.to_str().unwrap(),
            "copy.bin",
            "--overwrite",
            "--sha256",
            &"0".repeat(64),
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("download SHA-256 mismatch"));
    assert_eq!(std::fs::read(&copy).unwrap(), b"previous destination");
    h.ok(
        &h.other,
        &[
            "file",
            "download",
            remote.to_str().unwrap(),
            "copy.bin",
            "--overwrite",
            "--sha256",
            &format!("sha256:{digest}"),
        ],
    );
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    assert!(!cwd.join("copy.bin").exists());
    assert!(!h
        .command(
            &h.other,
            &["file", "download", remote.to_str().unwrap(), "copy.bin"]
        )
        .status
        .success());
    // Staged copies still support create/put/show/get and retain binary bytes.
    let second = cwd.join("staged-remote.bin");
    let stage = client
        .call(
            "transfers.create",
            json!({"host":"local","options":{"mode":"upload","path":second,"size":bytes.len()}}),
        )
        .await
        .unwrap();
    let staged = stage["id"].as_str().unwrap();
    client
        .call("transfers.put", json!({"id":staged,"localPath":source}))
        .await
        .unwrap();
    client.wait_transfer(staged).await.unwrap();
    let stage = client
        .call(
            "transfers.create",
            json!({"host":"local","options":{"mode":"download","path":second}}),
        )
        .await
        .unwrap();
    let staged = stage["id"].as_str().unwrap();
    client.wait_transfer(staged).await.unwrap();
    let output = cwd.join("staged-copy.bin");
    client
        .save_content(staged, output.clone(), false)
        .await
        .unwrap();
    assert_eq!(std::fs::read(output).unwrap(), bytes);
    // A second project can run its own daemon in this registry. Binding it
    // back to the first service does not change the first project's binding.
    h.ok(&h.other, &["service", "create"]);
    let id2 = h.id(&h.other);
    assert_ne!(id, id2);
    h.ok(&h.other, &["--service-id", &id2, "service", "stop"]);
    h.ok(&h.other, &["service", "bind", &id]);
    assert_eq!(h.id(&h.other), id);
    assert_eq!(h.id(cwd), id);
    h.ok(cwd, &["service", "stop"]);
    assert!(h.ok(cwd, &["service", "info"]).contains("running**: false"));
    assert!(h.registry().resolve(&id).unwrap().target.is_none());
    h.ok(&h.other, &["service", "start"]);
    assert_eq!(h.id(cwd), id);
    h.client(&id).verify_service().await.unwrap();
    // Reused explicit keys never dispatch a second mutation.
    let params = json!({"id":"duplicate","name":"dedup","url":"tcp://127.0.0.1:1","root":"/"});
    let client = h.client(&id);
    client
        .call_with_key("hosts.save", params.clone(), Some("save-once".into()))
        .await
        .unwrap();
    client
        .call_with_key("hosts.save", params, Some("save-once".into()))
        .await
        .unwrap();
    assert!(client
        .call_with_key(
            "hosts.remove",
            json!({"host":"duplicate"}),
            Some("save-once".into())
        )
        .await
        .is_err());
    #[cfg(target_os = "linux")]
    {
        let pid = h.registry().resolve(&id).unwrap().pid.unwrap();
        assert_eq!(
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .unwrap()
                .trim(),
            "rexetermd"
        );
        let status = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        h.ok(cwd, &["service", "start"]);
        assert_eq!(h.id(cwd), id);
        h.client(&id).verify_service().await.unwrap();
    }
}
#[tokio::test]
async fn rexetermd_runs_without_cli_deadline_and_directory_disable_still_precedes_startup() {
    let h = Installed::new();
    let cwd = h.root.path();
    h.ok(cwd, &["disable"]);
    let out = h.command(cwd, &["--daemon"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "Exit due to file [.rexeterm.disable]. Run in your project root, or set cwd to it, instead of the shared workspace.\n"
    );
    assert!(!h.binary.parent().unwrap().join(".rexeterm-socket").exists());
    h.ok(cwd, &["enable"]);
    let mut child = Command::new(&h.binary)
        .args(["--timeout-ms", "1", "daemon", "--local"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(Some((id, _))) = read_binding(cwd) {
                if let Ok(record) = h.registry().resolve(&id) {
                    if record.target.is_some() {
                        return id;
                    }
                }
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "daemon exited before ready"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    if ready.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("foreground daemon did not become ready");
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(child.try_wait().unwrap().is_none());
    h.ok(cwd, &["service", "stop"]);
    assert!(child.wait().unwrap().success());
}
#[tokio::test]
async fn run_restarts_a_stopped_host() {
    let h = Installed::new();
    let cwd = h.root.path();
    h.ok(cwd, &["service", "create", "--local"]);
    let id = h.id(cwd);
    let first = h.registry().resolve(&id).unwrap().pid;
    h.ok(cwd, &["service", "stop"]);
    assert!(h.registry().resolve(&id).unwrap().target.is_none());
    // No socket: the CLI starts the host role itself and then runs the command.
    let out = h.ok(
        cwd,
        &["-e", "local", "-t", "2000", "-cmd", "echo auto-started"],
    );
    assert!(out.contains("auto-started"), "{out}");
    let second = h.registry().resolve(&id).unwrap();
    assert!(second.target.is_some());
    assert!(second.pid.is_some());
    assert_ne!(first, second.pid);
    assert_eq!(h.id(cwd), id);
    h.ok(cwd, &["service", "stop"]);
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn long_install_path_keeps_the_socket_beside_the_binary() {
    let mut h = Installed::new();
    let long = h.binary.parent().unwrap().join("directory".repeat(16));
    std::fs::create_dir(&long).unwrap();
    let binary = long.join("rexeterm");
    std::fs::copy(&h.binary, &binary).unwrap();
    h.binary = binary.clone();
    let out = Command::new(&binary)
        .args(["service", "create"])
        .current_dir(h.root.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let registry = Registry::open(Some(long.join(".rexeterm-socket"))).unwrap();
    let id = h.id(h.root.path());
    let record = registry.resolve(&id).unwrap();
    assert!(record.target.unwrap().len() > 108);
    h.ok(&h.other, &["service", "bind", &id]);
    let out = Command::new(&binary)
        .arg("info")
        .current_dir(&h.other)
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = Command::new(&binary)
        .args(["service", "stop"])
        .current_dir(&h.other)
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn saved_entries_and_profiles_are_inspected_without_starting_or_leaking() {
    let h = Installed::new();
    let cwd = h.root.path();
    let saved = cwd.join(".rexeterm/config-dir/executors");
    std::fs::create_dir_all(&saved).unwrap();
    let legacy = saved.join("old.json");
    std::fs::write(&legacy, r#"{"id":"old","url":"tls://10.1.2.3:9000","authentication":{"method":"password","password":"private-test-value"},"tls":{"privateKeyFile":"do-not-print.pem"}}"#).unwrap();
    let out = h.ok(cwd, &["-e", "-list_local"]);
    assert!(out.contains("id**: old") && out.contains("directory**:"));
    assert!(!out.contains("private-test-value") && !out.contains("do-not-print.pem"));
    let out = h.ok(
        cwd,
        &[
            "-e",
            "old",
            "executor",
            "host",
            "edit",
            "--options",
            r#"{"name":"Migrated"}"#,
        ],
    );
    assert!(!out.contains("private-test-value") && !out.contains("do-not-print.pem"));
    assert!(saved.join("old/executor.json").is_file());
    assert!(h.ok(cwd, &["-cr", "-list_local"]).contains("credentials"));
    assert!(!h.ok(cwd, &["-e", "-list_online"]).contains("Migrated"));
    assert!(h.ok(cwd, &["preferences", "show"]).contains("shells"));
    let out = h.command(cwd, &["-F", "invalid-id"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid forward"));
    let out = h.command(
        cwd,
        &[
            "file",
            "upload",
            "/remote/file",
            "local.file",
            "--sha256",
            "invalid",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("64 hexadecimal"));
    assert!(!h.binary.parent().unwrap().join(".rexeterm-socket").exists());
    assert!(!cwd.join(BINDING_FILE).exists());
}

#[cfg(unix)]
#[test]
fn selected_credential_uses_the_link_targets_directory_before_ipc() {
    let h = Installed::new();
    let cwd = h.root.path();
    let profile_dir = h.other.join("HostNet");
    std::fs::create_dir(&profile_dir).unwrap();
    let profile = profile_dir.join("profile.json");
    std::fs::write(&profile, r#"{"id":"ignored","url":"ignored","username":"test","connectionProfile":"profile.json","authentication":{"method":"password","password":"never-show-this"},"tls":{"caFile":"ca.pem"}}"#).unwrap();
    h.ok(
        cwd,
        &[
            "-e",
            "node",
            "executor",
            "host",
            "add",
            "tls://127.0.0.1:9000",
            "--root",
            "/",
        ],
    );
    let link = h.ok(
        cwd,
        &["credential", "link", "test", profile.to_str().unwrap()],
    );
    assert!(!link.contains("never-show-this"));
    let links = h.ok(cwd, &["-cr", "-list_local"]);
    assert!(links.contains("available**: true") && !links.contains("never-show-this"));
    let out = h.command(
        cwd,
        &["executor", "connect", "test@node", "as", "live_node"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&*profile_dir.join("ca.pem").to_string_lossy())
            && !err.contains("never-show-this"),
        "{err}"
    );
    std::fs::write(
        profile_dir.join("ca.pem"),
        b"fixture, not used by a network connection",
    )
    .unwrap();
    for args in [
        vec!["executor", "connect", "test@node", "as", "live_node"],
        vec!["-e", "node", "-cr", "test", "executor", "connect"],
    ] {
        let out = h.command(cwd, &args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("no CLI host binding"), "{err}");
    }
    h.ok(cwd, &["credential", "unlink", "test"]);
    assert!(profile.is_file());
    assert!(!h.binary.parent().unwrap().join(".rexeterm-socket").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_hostname_is_the_default_live_id() {
    use rexeterm_daemon::server::{start_shared_executor_ws, Executor, ShellManager};

    let h = Installed::new();
    let cwd = h.root.path();
    let addr = start_shared_executor_ws(
        "127.0.0.1:0",
        Executor::local("server-id"),
        ShellManager::default_shell(80, 24),
    )
    .unwrap();
    let url = format!("ws://{addr}");
    h.ok(
        cwd,
        &[
            "-e",
            "RB5",
            "executor",
            "host",
            "add",
            &url,
            "--root",
            cwd.to_str().unwrap(),
            "--name",
            "Display name",
        ],
    );
    let credentials = cwd.join(".rexeterm/config-dir/credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    std::fs::write(
        credentials.join("rb5-codex.json"),
        r#"{"id":"profile-id","url":"ws://unused.example.test:80","tls":{}}"#,
    )
    .unwrap();
    h.ok(cwd, &["service", "create"]);

    for args in [
        &["executor", "connect", "rb5-codex@RB5"][..],
        &["-ec", "rb5-codex@RB5"],
    ] {
        h.ok(cwd, args);
        assert!(h.ok(cwd, &["-e", "-list_online"]).contains("id**: RB5"));
        h.ok(cwd, &["-e", "RB5", "executor", "disconnect"]);
    }

    h.ok(
        cwd,
        &[
            "--service-id",
            &h.id(cwd),
            "-ec",
            "rb5-codex@RB5",
            "as",
            "rb5_codex_exe",
        ],
    );
    let online = h.ok(cwd, &["-e", "-list_online"]);
    assert!(online.contains("id**: rb5_codex_exe") && !online.contains("id**: RB5"));
    let saved = h.ok(cwd, &["-e", "-list_local"]);
    assert!(saved.contains("id**: RB5") && !saved.contains("rb5_codex_exe"));
}

// The address in the saved entry is only usable through this hop's remote opener.
struct RoutedTarget {
    target: String,
    actual: std::net::SocketAddr,
}

impl rexeterm_appcore::data::StreamOpener for RoutedTarget {
    fn open<'a>(
        &'a self,
        target: &'a rexeterm_appcore::protocol::forward::StreamTarget,
    ) -> rexeterm_appcore::transport::LinkFuture<'a, Box<dyn rexeterm_appcore::data::ByteStream>>
    {
        Box::pin(async move {
            anyhow::ensure!(
                target.kind == "tcp" && target.address == self.target,
                "unknown test destination"
            );
            Ok(Box::new(tokio::net::TcpStream::connect(self.actual).await?)
                as Box<dyn rexeterm_appcore::data::ByteStream>)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn jump_chain_forwards_over_each_hop_and_releases_its_resources() {
    use rexeterm_daemon::server::{
        auth::{hash_password, Authentication, ConfigDirectory, Permissions, UserConfig},
        start_executor, Executor, ShellManager,
    };
    use std::{net::SocketAddr, sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        time::timeout,
    };

    let h = Installed::new();
    let cwd = h.root.path();
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    let echo_task = tokio::spawn(async move {
        let (mut socket, _) = echo.accept().await.unwrap();
        let mut bytes = vec![];
        socket.read_to_end(&mut bytes).await.unwrap();
        socket.write_all(&bytes).await.unwrap();
        socket.shutdown().await.unwrap();
    });

    let tls = ConfigDirectory::open(Some(h.other.join("test-tls"))).unwrap();
    tls.initialize(vec!["hop-c.example.invalid".into()])
        .unwrap();
    tls.save_user(
        "test-c",
        &UserConfig {
            password_hash: Some(hash_password("fixture-password").unwrap()),
            permissions: Permissions::all(),
            ..Default::default()
        },
    )
    .unwrap();
    let c = start_executor(
        "wss://127.0.0.1:0/re",
        Executor::local("server-C")
            .with_authentication(Authentication::load(&tls).unwrap())
            .with_stream_opener(Arc::new(RoutedTarget {
                target: format!("[2001:db8::9]:{}", echo_addr.port()),
                actual: echo_addr,
            })),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let c: SocketAddr = c
        .strip_prefix("wss://")
        .unwrap()
        .strip_suffix("/re")
        .unwrap()
        .parse()
        .unwrap();
    let c_target = format!("hop-c.example.invalid:{}", c.port());
    let b = start_executor(
        "ws://127.0.0.1:0/re",
        Executor::local("server-B").with_stream_opener(Arc::new(RoutedTarget {
            target: c_target.clone(),
            actual: c,
        })),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let b: SocketAddr = b
        .strip_prefix("ws://")
        .unwrap()
        .strip_suffix("/re")
        .unwrap()
        .parse()
        .unwrap();
    let b_target = format!("hop-b.example.invalid:{}", b.port());
    let a = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("server-A").with_stream_opener(Arc::new(RoutedTarget {
            target: b_target.clone(),
            actual: b,
        })),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();

    for (host, url) in [
        ("HostA", a),
        ("HostB", format!("ws://{b_target}/re")),
        ("Host-C", format!("wss://{c_target}/re")),
    ] {
        h.ok(
            cwd,
            &["-e", host, "executor", "host", "add", &url, "--root", "/"],
        );
    }
    let credentials = cwd.join(".rexeterm/config-dir/credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    let c_profile = json!({
        "username":"test-c",
        "authentication":{"method":"password","password":"fixture-password"},
        "tls":{"caFile":tls.path().join("server-cert.pem")}
    });
    let mut wrong_profile = c_profile.clone();
    wrong_profile["tls"]["serverName"] = json!("wrong.example.invalid");
    for (name, profile) in [
        ("Sk-a", json!({"tls":{}})),
        ("Sk-b", json!({"tls":{}})),
        ("Sk-c", c_profile),
        ("Sk-wrong", wrong_profile),
    ] {
        std::fs::write(
            credentials.join(format!("{name}.json")),
            profile.to_string(),
        )
        .unwrap();
    }

    h.ok(cwd, &["service", "create"]);
    let ipc = h.client(&h.id(cwd));
    let chain = "Sk-a@HostA:Sk-b@HostB:Sk-c@Host-C";
    h.ok(cwd, &["-f"]);
    h.ok(cwd, &["-ec", chain, "as", "Cfba"]);
    let online = h.ok(cwd, &["-e", "-list_online"]);
    for id in ["Cfba_HostA_node1", "Cfba_HostB_node2", "Cfba"] {
        assert!(online.contains(&format!("id**: {id}")), "{online}");
    }
    assert!(h.ok(cwd, &["info"]).contains("defaultExecutor**: Cfba"));
    assert!(!h.ok(cwd, &["-e", "-list_local"]).contains("Cfba"));

    let duplicate = h.command(cwd, &["-ec", chain, "--as", "Cfba"]);
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already connected"));

    // A forward with @Cfba must select C even when another executor is the default.
    ipc.call("rpc", json!({"tool":"set_default_executor","executor":"Cfba_HostA_node1","params":{"id":"Cfba_HostA_node1"}})).await.unwrap();
    let spec = format!("0:[2001:db8::9]@Cfba:{}", echo_addr.port());
    h.ok(cwd, &["-f", &spec]);
    let listed = h.ok(cwd, &["-F"]);
    let forwards = ipc.call("forwards.list", json!({})).await.unwrap();
    let forwards = forwards.as_array().unwrap();
    assert_eq!(forwards.len(), 3);
    for (id, exit, target) in [
        (
            "Cfba_HostA_node1_Forward_node2",
            "Cfba_HostA_node1",
            &b_target,
        ),
        (
            "Cfba_HostB_node2_Forward_node3",
            "Cfba_HostB_node2",
            &c_target,
        ),
    ] {
        assert!(listed.contains(id));
        assert!(forwards.iter().any(|f| f["forwardID"] == id
            && f["executor"] == exit
            && f["target"]["address"] == *target));
    }
    let echo_forward = forwards.iter().find(|f| f["executor"] == "Cfba").unwrap();
    let address = echo_forward["listen"]
        .as_str()
        .unwrap()
        .strip_prefix("tcp://")
        .unwrap();
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket.write_all(b"via three hops").await.unwrap();
    socket.shutdown().await.unwrap();
    let mut bytes = vec![];
    timeout(Duration::from_secs(10), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bytes, b"via three hops");
    echo_task.await.unwrap();
    h.ok(
        cwd,
        &["-f", echo_forward["forwardID"].as_str().unwrap(), "--stop"],
    );
    assert!(TcpStream::connect(address).await.is_err());

    h.ok(cwd, &["-e", "Cfba", "executor", "disconnect"]);
    assert_eq!(
        ipc.call("forwards.list", json!({})).await.unwrap(),
        json!([])
    );
    for forward in forwards {
        let addr = forward["listen"]
            .as_str()
            .unwrap()
            .strip_prefix("tcp://")
            .unwrap();
        assert!(TcpStream::connect(addr).await.is_err());
    }
    assert!(!h.ok(cwd, &["-e", "-list_online"]).contains("Cfba"));

    // Prove a failed chain on the same entry host does not close an existing live session.
    h.ok(cwd, &["-ec", "Sk-a@HostA", "as", "direct-A"]);
    let failed = h.command(
        cwd,
        &[
            "-ec",
            "Sk-a@HostA:Sk-b@HostB:Sk-wrong@Host-C",
            "as",
            "broken",
        ],
    );
    assert!(!failed.status.success());
    let error = String::from_utf8_lossy(&failed.stderr);
    assert!(
        error.contains("hop 3 (Host-C)") && !error.contains("fixture-password"),
        "{error}"
    );
    let online = h.ok(cwd, &["-e", "-list_online"]);
    assert!(
        !online.contains("broken") && online.contains("id**: direct-A"),
        "{online}"
    );
    assert_eq!(
        ipc.call("forwards.list", json!({})).await.unwrap(),
        json!([])
    );
    h.ok(
        cwd,
        &["-e", "direct-A", "file", "stat", cwd.to_str().unwrap()],
    );
    h.ok(cwd, &["-e", "direct-A", "executor", "disconnect"]);

    // The long spelling supports multi-hop and the last saved hostname is the default alias.
    h.ok(cwd, &["executor", "connect", "Sk-a@HostA:Sk-b@HostB"]);
    assert!(h.ok(cwd, &["-e", "-list_online"]).contains("id**: HostB"));
    h.ok(cwd, &["-f", "HostB_HostA_node1_Forward_node2", "--stop"]);
    assert!(!h.ok(cwd, &["-e", "-list_online"]).contains("HostB"));
    assert_eq!(
        ipc.call("forwards.list", json!({})).await.unwrap(),
        json!([])
    );
}

#[tokio::test]
async fn a_failed_output_sink_reports_the_started_task_instead_of_success() {
    let h = Installed::new();
    let cwd = h.root.path();
    h.ok(cwd, &["service", "create", "--local"]);
    std::fs::write(cwd.join("not-a-directory"), b"occupied").unwrap();
    let out = h.command(
        cwd,
        &[
            "-e",
            "local",
            "--cmd",
            "echo payload",
            "-t",
            "1000",
            "--sink",
            "file",
            "--out-dir",
            "not-a-directory",
        ],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("cannot save output for asyncID") && err.contains("not-a-directory"),
        "{err}"
    );
}
