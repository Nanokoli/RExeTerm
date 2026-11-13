#![cfg(feature = "daemon")]
use std::{
    path::Path,
    process::{Command, Output},
};
fn cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rexeterm"))
        .arg("--registry-dir")
        .arg(cwd.join("test-registry"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
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
    let out = cli(&child, &["info"]);
    assert!(!String::from_utf8(out.stderr)
        .unwrap()
        .contains("Exit due to file"));
}
