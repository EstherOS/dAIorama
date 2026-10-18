//! Packaging pins (P5): the shipped unit and its runbook are contract, not prose.
//!
//! 09 names the channels and 10 pins the supervision shape, so the files an
//! operator actually installs are read back here — a directive or command that
//! drifts from the docs fails the build (the fixture/doc pin pattern).

const UNIT: &str = include_str!("../packaging/systemd/daiorama@.service");
const README: &str = include_str!("../packaging/README.md");
const STRINGS: &str = include_str!("../src/strings.rs");
/// 04's no-package rendering for a missing Hyprland; 09's table carries it verbatim.
const NO_PACKAGE: &str =
    "not packaged for this distribution; see the release notes for a COPR/upstream build";
const NOVNC_FETCH: &str = include_str!("../packaging/novnc/fetch.sh");
const NOVNC_NOTE: &str = include_str!("../p5/novnc/README.md");
const SPEC_06: &str = include_str!("../docs/06-sessions-views.md");
const SPEC_09: &str = include_str!("../docs/09-packaging.md");
const RELEASE_NOTES: &str = include_str!("../RELEASE-NOTES.md");
const IPC: &str = include_str!("../src/ipc/mod.rs");
const RELEASE_SOURCE: &str = include_str!("../src/release.rs");
const DASHBOARD_CLIENT: &str = include_str!("../dashboard/src/client.rs");
const DASHBOARD_MANIFEST: &str = include_str!("../dashboard/Cargo.toml");
const SPEC_10: &str = include_str!("../docs/10-operations.md");
const IMAGE_NOTE: &str = include_str!("../p5/image/README.md");
const ENTRYPOINT: &str = include_str!("../packaging/container/entrypoint.sh");
const INSTALL_COMMANDS: &str = include_str!("../packaging/install-commands.md");
const CORE_IMAGE: &str = include_str!("../packaging/container/Containerfile.core");
const DESKTOP_IMAGE: &str = include_str!("../packaging/container/Containerfile.desktop");
const IMAGE_VERIFY: &str = include_str!("../packaging/container/verify-image.sh");
const COREFILE: &str = include_str!("../packaging/container/Containerfile.core");
const COMPOSE_MANAGER_ONLY: &str = include_str!("../packaging/compose/manager-only.yml");
const COMPOSE_MANAGER_AND_BRIDGE: &str =
    include_str!("../packaging/compose/manager-and-bridge.yml");
const COMPOSE_BRIDGE_REMOTE: &str = include_str!("../packaging/compose/bridge-remote.yml");
const QUADLET: &str = include_str!("../packaging/quadlet/daiorama.container");

#[test]
fn unit_is_a_user_service_that_runs_serve_for_the_instance() {
    // 10: units always use `serve` explicitly, never the auto-estimate.
    assert!(
        UNIT.contains("ExecStart=%h/.local/bin/daiorama serve --instance %i"),
        "{UNIT}"
    );
    assert!(UNIT.contains("WantedBy=default.target"), "{UNIT}");
    assert!(!UNIT.contains("daemon start"), "{UNIT}");
}

#[test]
fn unit_sweeps_the_tree_and_delegates_the_cgroup() {
    // 10: a unit stop sweeps the whole tree; 06: limits need a delegated subtree.
    assert!(UNIT.contains("KillMode=control-group"), "{UNIT}");
    assert!(UNIT.contains("Delegate=yes"), "{UNIT}");
}

#[test]
fn the_runbook_mentions_linger_for_headless_hosts() {
    // 10: a user unit needs the user manager alive; on a headless box that is only
    // true after `loginctl enable-linger`, so the runbook must not omit it.
    assert!(
        README.contains("loginctl enable-linger"),
        "runbook omits linger"
    );
}

#[test]
fn unit_holds_no_secret_and_creates_no_directories() {
    // 08: the in-binary ensure is the only path that creates the data/runtime dirs.
    assert!(!UNIT.contains("ExecStartPre"), "{UNIT}");
    // 07/10: the token lives in its 0600 file, never in the unit or `systemctl show`.
    assert!(!UNIT.contains("DAIORAMA_TOKEN"), "{UNIT}");
}

#[test]
fn runbook_carries_the_operator_path() {
    for needle in [
        "cargo build --release",
        "install -Dm644 packaging/systemd/daiorama@.service ~/.config/systemd/user/daiorama@.service",
        "systemctl --user enable --now daiorama@default",
        "daiorama doctor",
        "daiorama config setup",
        "daiorama drain --wait",
        "daiorama daemon end --force",
        "systemctl --user restart daiorama@default",
    ] {
        assert!(README.contains(needle), "runbook lost: {needle}");
    }
}

#[test]
fn runbook_quotes_the_pinned_view_assets_refusal() {
    // The operator's 503 exit path must quote the string the manager prints.
    let pinned = "view assets not installed (expected <data>/novnc)";
    assert!(
        STRINGS.contains(pinned),
        "strings.rs no longer pins the view-assets refusal"
    );
    assert!(
        README.contains(pinned),
        "runbook must quote the pinned refusal"
    );
}

#[test]
fn novnc_installer_and_sourcing_note_carry_one_pin() {
    // Two records, one artifact: the sourcing note and the shipped installer must agree.
    let sha = "b1003a11b6e6e8d8f7f5e5586daae7f8ca651d8aee0aa155ff9ac841c48f52c6";
    assert!(NOVNC_FETCH.contains(sha), "installer pin drifted");
    assert!(
        NOVNC_NOTE.contains(sha),
        "note pin drifted from the installer"
    );
    assert!(NOVNC_FETCH.contains("refs/tags/v1.7.0"), "{NOVNC_FETCH}");
    // 09 assigns the install to packaging; doctor prints the same path (strings.rs).
    assert!(README.contains("packaging/novnc/fetch.sh"), "{README}");
    assert!(STRINGS.contains("packaging/novnc/fetch.sh"), "{STRINGS}");
    // Staging precedes the first run (10), so the target's parent may not exist yet: the
    // installer creates it rather than relying on a data root the binary has not made.
    assert!(
        NOVNC_FETCH.contains("mkdir -p \"$(dirname \"$DEST\")\""),
        "{NOVNC_FETCH}"
    );
}

#[test]
fn novnc_installer_refuses_unsafe_destinations_before_downloading() {
    // These refusals run before the pinned URL is touched, so the test needs no network.
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/novnc/fetch.sh");
    for dest in ["/", ".", "..", ""] {
        let out = std::process::Command::new("sh")
            .arg(&script)
            .arg(dest)
            .output()
            .expect("script runs");
        assert!(
            !out.status.success(),
            "dest {dest:?} must be refused: {out:?}"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("refusing"), "dest {dest:?}: {err}");
    }

    // An existing non-tree directory is refused with its contents intact.
    let tmp = tempfile::tempdir().expect("tempdir");
    let keep = tmp.path().join("keep");
    std::fs::create_dir_all(&keep).unwrap();
    std::fs::write(keep.join("keep.txt"), "x").unwrap();
    let out = std::process::Command::new("sh")
        .arg(&script)
        .arg(&keep)
        .output()
        .expect("script runs");
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a noVNC tree"));
    assert_eq!(std::fs::read_to_string(keep.join("keep.txt")).unwrap(), "x");
}

#[test]
fn the_installer_names_and_checks_its_prerequisites() {
    // N2: a missing tool fails up front with a named prerequisite, not a bare shell error.
    assert!(
        NOVNC_FETCH.contains("for tool in tar sha256sum"),
        "the fixed tools are named"
    );
    assert!(
        NOVNC_FETCH.contains("command -v \"$tool\""),
        "each named tool is checked"
    );
    assert!(
        NOVNC_FETCH.contains("refusing: curl is required to download"),
        "download path names curl"
    );
    assert!(
        README.contains("Installer prerequisites"),
        "runbook states the prerequisites"
    );
}

#[test]
fn image_split_is_pinned_across_the_docs_and_the_runbook() {
    // 09 pins the two artifacts and scopes the no-default-CMD rule to the core image.
    assert!(
        SPEC_09.contains("| `daiorama-desktop` |"),
        "09 lost the desktop artifact row"
    );
    assert!(
        SPEC_09.contains("documented exception"),
        "09 lost the scoped CMD exception"
    );
    assert!(
        SPEC_09.contains("/usr/share/daiorama/novnc"),
        "09 lost the seed path"
    );
    // 06 names the launcher instead of "a fixed path".
    assert!(
        SPEC_06.contains("/usr/local/bin/daiorama-browser"),
        "06 does not name the launcher path"
    );
    // 10's staging note must describe the seed copy, not a build-time stage into /data.
    assert!(
        SPEC_10.contains("pre-seeds the volume"),
        "10 still describes build-time staging"
    );
    assert!(
        !SPEC_10.contains("runs it at build time into the persisted"),
        "10 kept the impossible build-time stage"
    );
    // One entrypoint, named by the spec, the runbook and the pre-read.
    for doc in [SPEC_09, README, IMAGE_NOTE] {
        assert!(
            doc.contains("packaging/container/entrypoint.sh"),
            "a doc stopped naming the entrypoint"
        );
    }
}

/// Run the shipped entrypoint with a seeded tree and a data home, returning its output.
fn run_entrypoint(
    seed: &std::path::Path,
    data: &std::path::Path,
    argv: &[&str],
) -> std::process::Output {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/container/entrypoint.sh");
    std::process::Command::new("sh")
        .arg(&script)
        .args(argv)
        .env("DAIORAMA_NOVNC_SEED", seed)
        .env("XDG_DATA_HOME", data)
        .output()
        .expect("entrypoint runs")
}

#[test]
fn entrypoint_seeds_once_and_never_overwrites_a_staged_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let seed = tmp.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(seed.join("vnc.html"), "seed").unwrap();
    let data = tmp.path().join("data");
    let dest = data.join("daiorama/default/novnc");

    // Absent entry point: copied, and the command runs (its exit code proves the exec).
    let out = run_entrypoint(&seed, &data, &["sh", "-c", "echo ran; exit 7"]);
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ran"),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("vnc.html")).unwrap(),
        "seed"
    );

    // Present: untouched — an operator's restage or newer tree is not clobbered on recreate.
    std::fs::write(dest.join("vnc.html"), "operator").unwrap();
    let out = run_entrypoint(&seed, &data, &["true"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("vnc.html")).unwrap(),
        "operator"
    );

    // No seed in the image: one line, then exec — a missing view tree must not stop the manager.
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let out = run_entrypoint(&empty, &data, &["sh", "-c", "exit 3"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no seeded view assets"),
        "{out:?}"
    );
}

#[test]
fn entrypoint_refuses_to_run_without_a_command() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_entrypoint(tmp.path(), tmp.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("usage:"),
        "{out:?}"
    );
}

#[test]
fn the_entrypoint_creates_only_the_asset_tree() {
    // 08: directory creation stays in the binary; the seed copy is the one exception, and it
    // must not pre-create the runtime dir or a data tree beyond the asset path.
    assert_eq!(
        ENTRYPOINT.matches("mkdir").count(),
        1,
        "exactly one directory is created: the asset tree"
    );
    assert!(
        ENTRYPOINT.contains("mkdir -p \"$DEST_DIR\""),
        "the asset tree's parent is the one directory created"
    );
    assert!(
        !ENTRYPOINT.contains("XDG_RUNTIME_DIR"),
        "the entrypoint must not pre-create the runtime dir"
    );
    assert!(
        !ENTRYPOINT.contains("DAIORAMA_TOKEN"),
        "the entrypoint holds no secret"
    );
}

#[test]
fn entrypoint_repairs_an_interrupted_first_seed() {
    // N4: a partial tree (copied but missing the entry point) must be replaced wholesale,
    // not skipped forever by a presence check on a half-written directory.
    let tmp = tempfile::tempdir().expect("tempdir");
    let seed = tmp.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(seed.join("vnc.html"), "seed").unwrap();
    let data = tmp.path().join("data");
    let dest = data.join("daiorama/default/novnc");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("stale.js"), "partial").unwrap();

    let out = run_entrypoint(&seed, &data, &["true"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("vnc.html")).unwrap(),
        "seed"
    );
    assert!(
        !dest.join("stale.js").exists(),
        "the partial tree is replaced, not merged"
    );
}

/// Strip comment lines so an absence pin tests the file, not its prose.
fn code_only(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with('#') || trimmed.starts_with("//"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn core_image_is_a_substrate_with_no_default_command() {
    // 09: the no-default-CMD rule is scoped to the core image, and the image is substrate.
    let code = code_only(COREFILE);
    // F1: `CMD []` is the only command allowed — it cancels the base image's `CMD ["bash"]`.
    let cmds: Vec<&str> = code
        .lines()
        .filter(|line| line.trim_start().starts_with("CMD"))
        .collect();
    assert_eq!(
        cmds,
        vec!["CMD []"],
        "the core image carries a default command"
    );
    assert!(
        !code.contains("ENTRYPOINT"),
        "the core image carries an entrypoint"
    );
    // 08's container row: one path set for the manager, the CLI and the compose mounts.
    for pin in [
        "XDG_DATA_HOME=/data",
        "XDG_CONFIG_HOME=/config",
        "XDG_RUNTIME_DIR=/run",
    ] {
        assert!(COREFILE.contains(pin), "core image lost {pin}");
    }
}

#[test]
fn core_image_pins_its_base_by_required_digest() {
    // 09: the digest is a required build argument, never a checked-in default.
    assert!(
        COREFILE.contains("ARG DEBIAN_SID_DIGEST"),
        "the base digest is not a build argument"
    );
    let froms: Vec<&str> = COREFILE
        .lines()
        .filter(|line| line.trim_start().starts_with("FROM "))
        .collect();
    assert_eq!(
        froms.len(),
        2,
        "expected builder and runtime stages: {froms:?}"
    );
    for line in froms {
        assert!(
            line.contains("@${DEBIAN_SID_DIGEST}"),
            "unpinned base in {line}"
        );
    }
}

#[test]
fn core_image_builds_the_binary_from_source_and_ships_the_tool_set() {
    // 09's channel table: the container image builds the binary directly.
    assert!(
        COREFILE.contains("cargo build --release --locked -p daiorama"),
        "{COREFILE}"
    );
    assert!(
        COREFILE.contains("COPY --from=build /src/target/release/daiorama /usr/local/bin/daiorama"),
        "{COREFILE}"
    );
    let code = code_only(COREFILE);
    for package in [
        "hyprland",
        "mutter",
        "wayvnc",
        "grim",
        "wf-recorder",
        "libgbm1",
        "xwayland",
        "seatd",
    ] {
        assert!(code.contains(package), "core package set lost {package}");
    }
    for absent in ["chromium", "foot", "wtype", "wlrctl"] {
        assert!(!code.contains(absent), "the core image ships {absent}");
    }
}

#[test]
fn compose_shapes_pin_the_image_and_the_sandbox() {
    for (name, file) in [
        ("manager-only", COMPOSE_MANAGER_ONLY),
        ("manager-and-bridge", COMPOSE_MANAGER_AND_BRIDGE),
        ("bridge-remote", COMPOSE_BRIDGE_REMOTE),
    ] {
        // 09 C2: compose pins by digest — the reference is required, never defaulted.
        assert!(
            file.contains("${DAIORAMA_IMAGE:?"),
            "{name} does not require a pinned image"
        );
        assert!(file.contains("cap_drop"), "{name} lost cap_drop");
        assert!(
            file.contains("no-new-privileges"),
            "{name} lost no-new-privileges"
        );
    }
    for (name, file) in [
        ("manager-only", COMPOSE_MANAGER_ONLY),
        ("manager-and-bridge", COMPOSE_MANAGER_AND_BRIDGE),
    ] {
        assert!(
            file.contains("/dev/dri/renderD128"),
            "{name} lost the render node"
        );
        assert!(
            file.contains("/sys/fs/cgroup:/sys/fs/cgroup:rw"),
            "{name} lost the delegation mount"
        );
        assert!(
            file.contains("limits.mode off"),
            "{name} lost the off-mode escape"
        );
    }
    // The bridge shapes read their credentials from an env file (04, 07).
    for (name, file) in [
        ("manager-and-bridge", COMPOSE_MANAGER_AND_BRIDGE),
        ("bridge-remote", COMPOSE_BRIDGE_REMOTE),
    ] {
        assert!(file.contains("bridge.env"), "{name} names no env file");
        for var in ["DAIORAMA_TOKEN", "DAIORAMA_SCOPE", "DAIORAMA_SCOPE_TOKEN"] {
            assert!(file.contains(var), "{name} does not document {var}");
        }
    }
}

#[test]
fn quadlet_wraps_the_core_image_with_the_same_sandbox() {
    for needle in [
        "Image=localhost/daiorama:dev",
        "Exec=daiorama serve",
        "DropCapability=all",
        "NoNewPrivileges=true",
        "AddDevice=/dev/dri/renderD128",
        "/data:Z",
        "/config:Z",
        "PublishPort=127.0.0.1:8756:8756",
        "PodmanArgs=--cgroupns=host",
        "WantedBy=default.target",
    ] {
        assert!(QUADLET.contains(needle), "quadlet lost {needle}");
    }
    // 07/10: credentials live in the data volume, never in a unit file.
    assert!(!QUADLET.contains("DAIORAMA_TOKEN"), "{QUADLET}");
}

#[test]
fn docs_and_runbook_name_the_shipped_container_assets() {
    for needle in [
        "packaging/container/Containerfile.core",
        "packaging/compose/",
        "packaging/quadlet/daiorama.container",
        "ARG DEBIAN_SID_DIGEST",
        "XDG_CONFIG_HOME=/config",
    ] {
        assert!(SPEC_09.contains(needle), "09 does not name {needle}");
    }
    for needle in [
        "DEBIAN_SID_DIGEST",
        "podman build",
        "podman-compose",
        "packaging/quadlet/daiorama.container",
    ] {
        assert!(README.contains(needle), "runbook lost {needle}");
    }
}

#[test]
fn the_core_image_pins_an_empty_command_and_strips_hyprland_caps() {
    // F1/F2: the base ships `CMD ["bash"]` and Debian sets cap_sys_nice on Hyprland; both are
    // properties of the artifact, so the Containerfile fixes them and the verify script checks
    // the artifact itself.
    assert!(CORE_IMAGE.contains("CMD []"), "no explicit empty command");
    assert!(
        CORE_IMAGE.contains("setcap -r /usr/bin/Hyprland"),
        "Hyprland file capabilities are stripped"
    );
    assert!(
        IMAGE_VERIFY.contains("Config.Cmd") && IMAGE_VERIFY.contains("podman inspect"),
        "the verify script inspects the built artifact"
    );
}

#[test]
fn image_builds_stage_the_embedded_release_trust_list() {
    // `release.rs` embeds `packaging/release/trusted-keys` with include_str!, so both build
    // stages that compile the root crate must carry the file into the build context. Without
    // this the image build fails at `couldn't read src/../packaging/release/trusted-keys`.
    let copy = "COPY packaging/release/trusted-keys ./packaging/release/trusted-keys";
    assert!(CORE_IMAGE.contains(copy), "core image omits the trust list");
    assert!(DESKTOP_IMAGE.contains(copy), "desktop launcher stage omits the trust list");
}

#[test]
fn the_desktop_containerfile_builds_on_core_and_installs_the_launcher() {
    // 09: the desktop artifact is core + pinned Chromium + the launcher, and is the one image
    // with a default command. CORE_IMAGE is required, never defaulted.
    assert!(DESKTOP_IMAGE.contains("ARG CORE_IMAGE"), "{DESKTOP_IMAGE}");
    assert!(
        DESKTOP_IMAGE.contains("FROM ${CORE_IMAGE}"),
        "{DESKTOP_IMAGE}"
    );
    assert!(
        DESKTOP_IMAGE.contains("--bin daiorama-browser"),
        "the launcher is built in its own stage"
    );
    assert!(
        DESKTOP_IMAGE.contains("COPY --from=launcher /src/target/release/daiorama-browser /usr/local/bin/daiorama-browser"),
        "{DESKTOP_IMAGE}"
    );
    assert!(
        DESKTOP_IMAGE.contains("CMD [\"daiorama\", \"serve\"]"),
        "{DESKTOP_IMAGE}"
    );
    assert!(
        DESKTOP_IMAGE.contains("ENTRYPOINT [\"/usr/local/bin/daiorama-entrypoint\"]"),
        "the seed entrypoint is installed as the desktop entrypoint"
    );
    assert!(
        DESKTOP_IMAGE.contains("COPY packaging/container/entrypoint.sh"),
        "{DESKTOP_IMAGE}"
    );
    assert!(
        DESKTOP_IMAGE.contains("ARG NOVNC_TARBALL"),
        "{DESKTOP_IMAGE}"
    );
    assert!(README.contains("Build the desktop image"), "{README}");
    assert!(SPEC_09.contains("Containerfile.desktop"), "{SPEC_09}");
}

#[test]
fn install_table_is_the_maintained_c2_artifact() {
    // 09 C2: the table is distro/release -> exact command -> version, explicitly
    // "not packaged / out of range" where no supported package exists, and it is
    // re-checked per release (the date is part of the artifact, not prose).
    assert!(
        INSTALL_COMMANDS.contains("Checked **2026-10-18**"),
        "the table lost its checked date"
    );
    assert!(
        INSTALL_COMMANDS.contains("0.56.0–0.56.9"),
        "the range moved"
    );
    assert!(
        INSTALL_COMMANDS.contains("Re-check each release"),
        "the re-check rule is not stated"
    );
    // Every command doctor's in-binary rows can print appears here verbatim.
    for command in [
        "sudo pacman -S hyprland",
        "sudo pacman -S wayvnc",
        "sudo dnf install wayvnc",
        "sudo apt install hyprland",
        "sudo apt install wayvnc",
    ] {
        assert!(
            INSTALL_COMMANDS.contains(command),
            "table lost a command doctor prints: {command}"
        );
    }
    // The no-package rendering 04 pins is carried verbatim (the string lives in strings.rs).
    assert!(
        STRINGS.contains(NO_PACKAGE),
        "the pinned no-package string moved"
    );
    assert!(
        INSTALL_COMMANDS.contains(NO_PACKAGE),
        "the table does not carry the no-package rendering"
    );
}

#[test]
fn install_table_rows_name_a_version_or_a_missing_marker() {
    // A row without a version and without a no-package/out-of-range marker is an
    // unpinned instruction; every row says which kind it is.
    let mut rows = 0;
    for line in INSTALL_COMMANDS.lines() {
        let line = line.trim();
        if !line.starts_with("| ") || line.contains("---") || line.contains("| command |") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert!(cells.len() >= 3, "table row is missing a version: {line}");
        let version = cells[2];
        let pinned = version.chars().next().is_some_and(|c| c.is_ascii_digit())
            || version.contains("no-package")
            || version.contains("out of range")
            || version.contains("not packaged")
            || version.contains("none")
            || version.contains("packaged")
            || version.contains("unknown");
        assert!(
            !version.is_empty() && pinned,
            "row names neither a version nor a missing marker: {line}"
        );
        rows += 1;
    }
    assert!(rows >= 8, "the table lost rows ({rows} found)");
}

#[test]
fn spec_09_and_the_runbook_name_the_table() {
    // 09 pins the path; the runbook points the operator at it.
    assert!(
        SPEC_09.contains("packaging/install-commands.md"),
        "{SPEC_09}"
    );
    assert!(README.contains("install-commands.md"), "{README}");
}

/// Reads a `pub const NAME: &str = "…";` value out of a source file (the binary is
/// bin-only, so the constants are read as text — the doc-pin pattern).
fn const_str(source: &str, name: &str) -> String {
    let needle = format!("pub const {name}: &str = \"");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("{name} is not a string constant any more"));
    let rest = &source[start + needle.len()..];
    let end = rest.find('"').expect("an unterminated constant");
    rest[..end].to_string()
}

/// Reads a non-`pub` `const NAME: &str = "…";` (the dashboard module's own protocol floor).
fn private_const_str(source: &str, name: &str) -> String {
    let needle = format!("const {name}: &str = \"");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("{name} is not a string constant any more"));
    let rest = &source[start + needle.len()..];
    let end = rest.find('"').expect("an unterminated constant");
    rest[..end].to_string()
}

/// Reads a `pub const NAME: u64 = N;` value out of a source file (same doc-pin pattern).
fn const_u64(source: &str, name: &str) -> u64 {
    let needle = format!("pub const {name}: u64 = ");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("{name} is not a u64 constant any more"));
    let rest = &source[start + needle.len()..];
    let end = rest.find(';').expect("an unterminated constant");
    rest[..end].trim().parse().expect("a numeric constant")
}

/// The `MAJOR.MINOR.x` line a `MIN_SUPPORTED` version defines: the range a compat row means.
fn minor_range(version: &str) -> String {
    let mut parts = version.split('.');
    let major = parts.next().expect("a major version");
    let minor = parts.next().expect("a minor version");
    format!("{major}.{minor}.x")
}

/// One `## <version>` section of RELEASE-NOTES.md, up to the next `## ` heading.
fn release_notes_section<'a>(notes: &'a str, version: &str) -> &'a str {
    let heading = format!("## {version}");
    let occurrences = notes
        .lines()
        .filter(|line| line.trim_end() == heading)
        .count();
    assert_eq!(
        occurrences, 1,
        "RELEASE-NOTES.md must carry exactly one `{heading}` section, found {occurrences}"
    );
    let start = notes.find(&heading).unwrap_or_else(|| {
        panic!("RELEASE-NOTES.md has no `{heading}` section: a version bump needs notes")
    });
    let rest = &notes[start + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    &rest[..end]
}

/// A compat-table row by its first cell, with markdown backticks stripped so a
/// value compares to the code's constant directly.
fn notes_row(section: &str, surface: &str) -> Vec<String> {
    let needle = format!("| {surface} |");
    let line = section
        .lines()
        .find(|line| line.starts_with(&needle))
        .unwrap_or_else(|| panic!("the compat table lost its `{surface}` row:\n{section}"));
    line.trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().trim_matches('`').to_string())
        .collect()
}

#[test]
fn release_notes_carry_this_version_and_agree_with_the_code() {
    // 09: the compat table is the release's contract. A bump that forgets its
    // section, or a row that drifts from the build, fails here — the note is not
    // hand-copied text that ages silently.
    let version = env!("CARGO_PKG_VERSION");
    let section = release_notes_section(RELEASE_NOTES, version);

    let core = notes_row(section, "Core binary (`daiorama`)");
    assert_eq!(
        core[1], version,
        "the core row disagrees with Cargo.toml: {core:?}"
    );

    let protocol = const_str(IPC, "PROTOCOL_VERSION");
    let row = notes_row(section, "RPC protocol");
    assert_eq!(
        row[1], protocol,
        "the protocol row disagrees with ipc::PROTOCOL_VERSION: {row:?}"
    );

    // The dashboard builds as its own artifact, so its version comes from its own
    // manifest, not the core package.
    let dashboard_version = DASHBOARD_MANIFEST
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .map(|rest| rest.trim_end_matches('"'))
        .expect("dashboard/Cargo.toml names a version");
    let row = notes_row(section, "Dashboard module");
    assert_eq!(
        row[1], dashboard_version,
        "the dashboard row disagrees with dashboard/Cargo.toml: {row:?}"
    );

    // Compatible ranges are the MIN_SUPPORTED line, not hand-written prose.
    let min_supported = const_str(IPC, "MIN_SUPPORTED");
    let range = minor_range(&min_supported);
    for surface in ["Core binary (`daiorama`)", "RPC protocol"] {
        let row = notes_row(section, surface);
        assert_eq!(
            row[2], range,
            "the {surface} compatible range disagrees with MIN_SUPPORTED {min_supported}: {row:?}"
        );
    }
    // The dashboard is its own artifact with its own protocol floor; pinning it to the core's
    // would hide an independent bump.
    let dashboard_min = private_const_str(DASHBOARD_CLIENT, "MIN_SUPPORTED");
    let dashboard_range = minor_range(&dashboard_min);
    let row = notes_row(section, "Dashboard module");
    assert_eq!(
        row[2], dashboard_range,
        "the Dashboard module range disagrees with dashboard/src/client.rs MIN_SUPPORTED {dashboard_min}: {row:?}"
    );

    // The manifest row is the format constant, both the label and the value.
    let manifest_format = const_u64(RELEASE_SOURCE, "MANIFEST_FORMAT");
    let row = notes_row(section, "Release manifest");
    assert_eq!(row[1], format!("format: {manifest_format}"), "{row:?}");
    assert_eq!(row[2], manifest_format.to_string(), "{row:?}");

    // The pointer is two-way: the doc the test reads is the doc 09 names.
    assert!(SPEC_09.contains("RELEASE-NOTES.md"), "{SPEC_09}");
}
