#![cfg(feature = "daemon")]
use std::{
    path::Path,
    process::{Command, Output},
};
fn cli(cwd: &Path, args: &[&str]) -> Output {
    with_registry(cwd, &cwd.join("test-registry"), args)
}
fn with_registry(cwd: &Path, registry: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rexeterm"))
        .arg("--registry-dir")
        .arg(registry)
        .args(args)
        .env_remove("REXETERM_CWD")
        .current_dir(cwd)
        .output()
        .unwrap()
}
fn assert_storage_disabled(out: Output, option: &str) {
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(out.stdout.is_empty());
    assert!(
        stderr.contains("Exit due to file [") && stderr.contains(".rexeterm.disable"),
        "{stderr}"
    );
    assert!(stderr.contains(option), "{stderr}");
}
#[test]
fn local_marker_blocks_before_registry_access_and_enable_needs_no_host() {
    let root = tempfile::tempdir().unwrap();
    assert!(cli(root.path(), &["disable"]).status.success());
    let marker = root.path().join(".rexeterm.disable");
    assert!(marker.is_file());
    std::fs::write(&marker, "keep this content").unwrap();
    assert!(cli(root.path(), &["disable"]).status.success());
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "keep this content"
    );
    let registry = root.path().join("test-registry");
    for args in [vec!["info"], vec!["service", "list"], vec!["capabilities"]] {
        let out = cli(root.path(), &args);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert_eq!(
            String::from_utf8(out.stderr).unwrap().trim(),
            "Exit due to file [.rexeterm.disable]. Run in your project root, or set cwd to it, instead of the shared workspace."
        );
    }
    assert!(!registry.exists());
    let help = cli(root.path(), &["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .contains(".rexeterm.disable"));
    assert!(cli(root.path(), &["enable"]).status.success());
    assert!(!marker.exists());
    assert!(cli(root.path(), &["enable"]).status.success());
    let out = cli(root.path(), &["info"]);
    assert!(!String::from_utf8(out.stderr)
        .unwrap()
        .contains("Exit due to file"));
}
#[test]
fn marker_in_parent_directory_does_not_disable_child() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".rexeterm.disable"), "").unwrap();
    let child = root.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let out = with_registry(&child, Path::new(".rexeterm/runtime"), &["service", "list"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn parent_marker_blocks_shared_registry_from_child_before_discovery_or_startup() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".rexeterm.disable"), "").unwrap();
    let child = root.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let registry = Path::new("../.rexeterm-config");
    for args in [
        &["service", "list"][..],
        &["executor", "list"],
        &["service", "runasdaemon", "--service-id", "1234567890abcdef"],
    ] {
        assert_storage_disabled(with_registry(&child, registry, args), "--registry-dir");
    }
    assert!(!root.path().join(".rexeterm-config").exists());
    assert!(!child.join("RexETermHostUUID.txt").exists());
}
#[cfg(unix)]
#[test]
fn shared_registry_marker_is_checked_through_a_symlink() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    let shared = root.path().join(".rexeterm-config");
    std::fs::create_dir(&child).unwrap();
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(root.path().join(".rexeterm.disable"), "").unwrap();
    std::os::unix::fs::symlink(&shared, child.join("borrowed")).unwrap();
    let registry = Path::new("borrowed/runtime");
    assert_storage_disabled(
        with_registry(&child, registry, &["service", "list"]),
        "--registry-dir",
    );
    assert!(!shared.join("runtime").exists());
}
#[test]
fn private_registry_cannot_restart_a_service_with_disabled_state_storage() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    let registry = child.join(".rexeterm/runtime");
    let storage = root.path().join(".rexeterm-config");
    let state = storage.join("host");
    let history = rexeterm_cli_host::Registry::open(Some(registry.clone())).unwrap();
    let record = history.reserve(&state, false).unwrap();
    std::fs::write(root.path().join(".rexeterm.disable"), "").unwrap();

    let out = with_registry(
        &child,
        &registry,
        &["--service-id", &record.id, "service", "start"],
    );
    if out.status.success() {
        with_registry(
            &child,
            &registry,
            &["--service-id", &record.id, "service", "stop"],
        );
    }
    assert_storage_disabled(out, "--config-dir");
    assert!(!state.join("cli-host.log").exists());
    let new_state = storage.join("new-host");
    assert!(history.reserve(&new_state, false).is_err());
    assert!(!new_state.exists());
}
