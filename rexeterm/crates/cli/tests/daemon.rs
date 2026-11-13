#![cfg(all(feature = "daemon", feature = "local"))]
use rexeterm_cli::ipc_client::IpcClient;
use rexeterm_cli_host::Registry;
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
        Command::new(&self.binary)
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap()
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
        self.registry().read_binding(cwd).unwrap()
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
#[tokio::test]
async fn single_host_serves_every_directory_with_executable_adjacent_native_ipc() {
    let h = Installed::new();
    let cwd = h.root.path();
    h.ok(cwd, &["service", "create", "--local"]);
    let id = h.id(cwd);
    let client = h.client(&id);
    let info = client.call("info", json!({})).await.unwrap();
    assert_eq!(info["role"], "rexetermd");
    // One host per machine: the binding lives in the registry, not the project.
    assert_eq!(h.registry().read_binding(cwd).unwrap(), id);
    assert!(!cwd.join(".rexeterm").join("binding").exists());
    // Any other directory resolves the same host without its own binding file.
    assert!(h.ok(&h.other, &["info"]).contains(&id));
    assert!(!h.other.join(".rexeterm").exists());
    // A stale directory-local binding never overrides the registry one.
    std::fs::create_dir_all(h.other.join(".rexeterm")).unwrap();
    std::fs::write(h.other.join(".rexeterm").join("binding"), "invalid-id").unwrap();
    assert!(h.ok(&h.other, &["info"]).contains(&id));
    std::fs::remove_file(h.other.join(".rexeterm").join("binding")).unwrap();
    // Creating again reuses the running host instead of spawning a second one.
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
    let copy = cwd.join("copy.bin");
    let bytes = (0..1024 * 1024 + 17)
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    std::fs::write(&source, &bytes).unwrap();
    h.ok(
        cwd,
        &[
            "file",
            "upload",
            remote.to_str().unwrap(),
            source.to_str().unwrap(),
            "--title",
            "传输",
        ],
    );
    h.ok(
        cwd,
        &[
            "file",
            "download",
            remote.to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    assert!(!h
        .command(
            cwd,
            &[
                "file",
                "download",
                remote.to_str().unwrap(),
                copy.to_str().unwrap()
            ]
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
    // One host per machine: creating again reuses it and keeps the same UUID64.
    h.ok(&h.other, &["service", "create"]);
    let id2 = h.id(&h.other);
    assert_eq!(id, id2);
    assert!(h.ok(&h.other, &["info"]).contains(&id));
    // `bind` switches the machine binding, so both directories follow.
    h.ok(&h.other, &["service", "bind", &id]);
    assert_eq!(h.registry().read_binding(&h.other).unwrap(), id);
    assert_eq!(h.registry().read_binding(cwd).unwrap(), id);
    h.ok(&h.other, &["--service-id", &id2, "service", "stop"]);
    h.ok(cwd, &["service", "stop"]);
    assert_eq!(h.id(cwd), id);
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
    let out = h.command(cwd, &["service", "runasdaemon"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "Exit due to file [.rexeterm.disable]. Run in your project root, or set cwd to it, instead of the shared workspace.\n"
    );
    assert!(!h.binary.parent().unwrap().join(".rexeterm-socket").exists());
    h.ok(cwd, &["enable"]);
    let mut child = Command::new(&h.binary)
        .args(["--timeout-ms", "1", "service", "runasdaemon"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(id) = h.registry().read_binding(cwd) {
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
    let id = registry.read_binding(h.root.path()).unwrap();
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
