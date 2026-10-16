//! `daiorama doctor` end-to-end against the real binary: the sections render, exit is 0 (the
//! report is an answer), and `--json` parses. Portable by construction: `PATH` points at a
//! temp dir of fake tools, so presence and versions are controlled; the containment verdict
//! is box-dependent and asserted structurally only.

use std::path::Path;
use std::process::{Command, Output};

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_daiorama"))
}

fn fake_tool(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_with_path(tmp: &Path, path: &Path, args: &[&str]) -> Output {
    let conf = tmp.join("conf");
    let data = tmp.join("data");
    let run = tmp.join("run");
    std::fs::create_dir_all(&conf).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    let mut command = Command::new(binary());
    command
        .env("PATH", path)
        .env("XDG_CONFIG_HOME", conf)
        .env("XDG_DATA_HOME", data)
        .env("XDG_RUNTIME_DIR", run)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DAIORAMA_MODE")
        .env_remove("DAIORAMA_TOKEN");
    command.args(args).output().expect("binary runs")
}

#[test]
fn doctor_reports_missing_tools_and_stays_an_answer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let empty = tmp.path().join("bin");
    std::fs::create_dir_all(&empty).unwrap();
    let out = run_with_path(tmp.path(), &empty, &["doctor"]);
    assert!(out.status.success(), "doctor exits 0: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.lines().next(),
        Some("daiorama doctor — environment check"),
        "{text}"
    );
    assert!(text.contains("\ntools:\n"), "{text}");
    assert!(text.contains("Hyprland not found. Install it: "), "{text}");
    assert!(text.contains("or use the container image."), "{text}");
    assert!(text.contains("wayvnc: missing — install it: "), "{text}");
    assert!(text.contains("\nwayland:\n"), "{text}");
    assert!(text.contains("WAYLAND_DISPLAY: unset"), "{text}");
    assert!(text.contains("\ncontainment:\n"), "{text}");
}

#[test]
fn doctor_reads_fake_tool_versions_both_human_and_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_tool(
        &bin,
        "Hyprland",
        "#!/bin/sh\necho 'Tag: v0.99.9-1-gdeadbeef'\n",
    );
    fake_tool(&bin, "hyprctl", "#!/bin/sh\necho 'Hyprland 0.99.9'\n");
    fake_tool(&bin, "wayvnc", "#!/bin/sh\necho 'wayvnc: version 0.9.1'\n");

    let out = run_with_path(tmp.path(), &bin, &["doctor"]);
    assert!(out.status.success(), "doctor exits 0: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Hyprland 0.99.9 is outside the supported range "),
        "{text}"
    );
    assert!(text.contains("wayvnc: 0.9.1 ("), "{text}");

    let out = run_with_path(tmp.path(), &bin, &["doctor", "--json"]);
    assert!(out.status.success(), "doctor --json exits 0: {out:?}");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("payload parses");
    assert_eq!(payload["tools"]["hyprland"]["version"], "0.99.9");
    assert_eq!(payload["tools"]["hyprland"]["supported"], false);
    assert_eq!(payload["tools"]["wayvnc"]["version"], "0.9.1");
    assert!(payload["containment"]["ok"].is_boolean());
}
