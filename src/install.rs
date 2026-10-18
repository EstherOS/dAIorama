//! The dashboard module installer (`daiorama dashboard install`, 09; `p5/dashboard` D5).
//!
//! Offline in v1: the release tooling downloads the per-triple asset, the manager installs
//! bytes it can verify. Order is the contract — checksum first (nothing under `modules/` is
//! created before it verifies), then a staging directory beside the module tree, then one
//! rename into place and one atomic symlink swap for activation. A refused install leaves the
//! module tree exactly as it was.

use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::config::{Paths, valid_module_version};
use crate::crypto::{hex, sha256};
use crate::ipc::ErrorEnvelope;
use crate::strings;

/// Release asset naming (09): `daiorama-dashboard-<version>-<target-triple>.tar.gz`.
pub const ASSET_PREFIX: &str = "daiorama-dashboard-";
pub const ASSET_SUFFIX: &str = ".tar.gz";

/// The target triples the release manifest names (09, C2). An asset name outside this set
/// needs `--version`: the boundary between version and triple is ambiguous otherwise.
pub const TARGET_TRIPLES: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];

/// Largest artifact accepted; checked before the bytes are read for hashing.
pub const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
/// Defense in depth: the digest is the trust boundary, but an artifact that inflates far past
/// its compressed size is refused before it can fill the data volume.
pub const MAX_UNPACKED_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;

/// The module binary inside an artifact — the path the core CLI execs (04, D1).
pub const MODULE_BINARY: &str = "daiorama-dashboard";

/// An installed (or re-activated) module version.
pub struct Installed {
    pub version: String,
    pub dir: PathBuf,
}

/// `<version>` out of `daiorama-dashboard-<version>-<target-triple>.tar.gz`.
///
/// The version itself may contain `-` (`0.2.0-rc1`), so the split is anchored on the known
/// triples rather than on the first dash.
pub fn version_from_asset_name(name: &str) -> Option<String> {
    let rest = name
        .strip_prefix(ASSET_PREFIX)?
        .strip_suffix(ASSET_SUFFIX)?;
    TARGET_TRIPLES.iter().find_map(|triple| {
        rest.strip_suffix(&format!("-{triple}"))
            .filter(|version| valid_module_version(version))
            .map(str::to_string)
    })
}

/// SHA-256 of a file, lowercase hex.
pub fn file_sha256(path: &Path) -> Result<String, ErrorEnvelope> {
    let meta = std::fs::metadata(path).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot read artifact {}: {err}", path.display()))
    })?;
    if !meta.is_file() {
        return Err(ErrorEnvelope::bad_request(format!(
            "artifact {} is not a regular file",
            path.display()
        )));
    }
    if meta.len() > MAX_ARTIFACT_BYTES {
        return Err(ErrorEnvelope::bad_request(strings::artifact_too_large(
            meta.len(),
            MAX_ARTIFACT_BYTES / (1024 * 1024),
        )));
    }
    let bytes = std::fs::read(path).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot read artifact {}: {err}", path.display()))
    })?;
    Ok(hex(&sha256(&bytes)))
}

/// Point `active` at `version` with one rename: the link either names the old version or the
/// new one, never a missing path. The target is a single slug component (D1's read rule).
fn activate(parent: &Path, version: &str) -> Result<PathBuf, ErrorEnvelope> {
    let tmp = parent.join(format!(".active.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(version, &tmp).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot create {}: {err}", tmp.display()))
    })?;
    let link = parent.join("active");
    std::fs::rename(&tmp, &link).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        ErrorEnvelope::bad_request(format!("cannot activate {}: {err}", link.display()))
    })?;
    Ok(link)
}

/// Re-activate an already-installed version (`--version <old>`, 09: deterministic revert).
pub fn activate_existing(paths: &Paths, version: &str) -> Result<Installed, ErrorEnvelope> {
    if !valid_module_version(version) {
        return Err(ErrorEnvelope::bad_request(strings::invalid_module_version(
            version,
        )));
    }
    let parent = paths.modules_dir().join("daiorama-dashboard");
    let dir = parent.join(version);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(ErrorEnvelope::bad_request(
                strings::module_version_not_a_directory(&dir.display().to_string()),
            ));
        }
        Err(_) => {
            return Err(ErrorEnvelope::bad_request(
                strings::module_version_not_installed(version, &dir.display().to_string()),
            ));
        }
    }
    activate(&parent, version)?;
    Ok(Installed {
        version: version.to_string(),
        dir,
    })
}

/// Verify, stage, move into place, activate. `version` is read from the asset name when
/// `None`.
pub fn install(
    paths: &Paths,
    artifact: &Path,
    sha256_hex: &str,
    version: Option<&str>,
    force: bool,
) -> Result<Installed, ErrorEnvelope> {
    // 1. The checksum gate runs before anything under `modules/` is created.
    let expected = sha256_hex.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ErrorEnvelope::bad_request(strings::invalid_sha256(
            sha256_hex,
        )));
    }
    let actual = file_sha256(artifact)?;
    if actual != expected {
        return Err(ErrorEnvelope::bad_request(strings::checksum_mismatch(
            &expected, &actual,
        )));
    }

    // 2. Version: explicit, or the asset name's, and only ever a directory name.
    let asset_name = artifact
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let version = match version {
        Some(raw) => {
            if !valid_module_version(raw) {
                return Err(ErrorEnvelope::bad_request(strings::invalid_module_version(
                    raw,
                )));
            }
            raw.to_string()
        }
        None => version_from_asset_name(asset_name).ok_or_else(|| {
            ErrorEnvelope::bad_request(strings::unknown_module_version(asset_name))
        })?,
    };

    // 3. Stage beside the module tree; the tree itself is untouched until the rename.
    let modules = paths.modules_dir();
    let modules_existed = modules.is_dir();
    let parent = modules.join("daiorama-dashboard");
    let parent_existed = parent.is_dir();
    std::fs::create_dir_all(&parent).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot create {}: {err}", parent.display()))
    })?;
    let stage = parent.join(format!(".stage-{}", std::process::id()));
    if stage.exists() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    std::fs::create_dir(&stage).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot create {}: {err}", stage.display()))
    })?;
    // 3b. Replacing the active version: remove the link first. `active` is then absent
    // (D1 reads "not installed") rather than dangling at a directory that is being renamed
    // away; the swap below recreates it.
    let active_slug = std::fs::read_link(parent.join("active"))
        .ok()
        .and_then(|target| {
            target
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
    let mut removed_active = None;
    if force
        && active_slug.as_deref() == Some(version.as_str())
        && std::fs::remove_file(parent.join("active")).is_ok()
    {
        removed_active = active_slug;
    }

    let staged =
        extract_artifact(artifact, &stage).and_then(|()| place(&parent, &stage, &version, force));
    if let Err(err) = staged {
        let _ = std::fs::remove_dir_all(&stage);
        // N2: the link was cleared above; a failed install puts it back so the module tree
        // is exactly what it was, not "not installed" beside a version directory.
        if let Some(slug) = removed_active
            && parent.join(&slug).is_dir()
        {
            let _ = activate(&parent, &slug);
        }
        // A first install that refused leaves no module tree behind: `remove_dir` only
        // succeeds on the directory it created and only while it is empty.
        if !parent_existed {
            let _ = std::fs::remove_dir(&parent);
        }
        // N1: the level above it too — `remove_dir` only succeeds while empty, so a modules
        // directory that held other things (or existed before) is untouched.
        if !modules_existed {
            let _ = std::fs::remove_dir(&modules);
        }
        return Err(err);
    }

    // 4. Activate, and report the version and path only. A failure here cannot roll the
    // bytes back (the previous tree is gone); name the state so the operator knows a
    // re-run is the recovery.
    let dir = parent.join(&version);
    activate(&parent, &version).map_err(|err| {
        ErrorEnvelope::bad_request(format!(
            "{}; version {version} is installed but active is absent — re-run install to activate it",
            err.message
        ))
    })?;
    Ok(Installed { version, dir })
}

/// A member path is refused when it is absolute or climbs (`..`) — only relative normal
/// components (and `.`) are allowed inside the stage.
fn entry_path_refused(path: &Path) -> bool {
    path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
}

/// A member kind is refused unless it is a regular file or a directory: links, devices and
/// other special members never land in a module tree.
fn entry_kind_refused(kind: tar::EntryType) -> bool {
    !(kind.is_file() || kind.is_dir())
}

/// Add one member's declared size to the running total, refusing past the cap.
fn budget(total: u64, size: u64) -> Result<u64, ErrorEnvelope> {
    let next = total.saturating_add(size);
    if next > MAX_UNPACKED_BYTES {
        return Err(ErrorEnvelope::bad_request(
            strings::artifact_expands_too_large(MAX_UNPACKED_BYTES / (1024 * 1024)),
        ));
    }
    Ok(next)
}

/// Unpack the gzipped tar into `stage` and check the one file that matters.
fn extract_artifact(artifact: &Path, stage: &Path) -> Result<(), ErrorEnvelope> {
    let file = std::fs::File::open(artifact).map_err(|err| {
        ErrorEnvelope::bad_request(format!(
            "cannot read artifact {}: {err}",
            artifact.display()
        ))
    })?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|err| {
        ErrorEnvelope::bad_request(format!(
            "cannot read artifact {}: {err}",
            artifact.display()
        ))
    })?;
    let mut total: u64 = 0;
    let mut count: usize = 0;
    for entry in entries {
        let mut entry = entry.map_err(|err| {
            ErrorEnvelope::bad_request(format!(
                "cannot read artifact {}: {err}",
                artifact.display()
            ))
        })?;
        let path = entry
            .path()
            .map_err(|err| {
                ErrorEnvelope::bad_request(format!(
                    "cannot read artifact {}: {err}",
                    artifact.display()
                ))
            })?
            .into_owned();
        // A module artifact is files and directories only, all of them relative and inside
        // the staging directory: links, devices and `..` are refusals, not surprises.
        if entry_path_refused(&path) {
            return Err(ErrorEnvelope::bad_request(strings::artifact_entry_refused(
                &path.display().to_string(),
            )));
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(ErrorEnvelope::bad_request(
                strings::artifact_too_many_entries(MAX_ENTRIES),
            ));
        }
        total = budget(total, entry.size())?;
        let kind = entry.header().entry_type();
        if entry_kind_refused(kind) {
            return Err(ErrorEnvelope::bad_request(strings::artifact_entry_refused(
                &path.display().to_string(),
            )));
        }
        let target = stage.join(
            path.components()
                .filter(|part| matches!(part, Component::Normal(_)))
                .collect::<PathBuf>(),
        );
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(|err| {
                ErrorEnvelope::bad_request(format!("cannot create {}: {err}", dir.display()))
            })?;
        }
        entry.unpack(&target).map_err(|err| {
            ErrorEnvelope::bad_request(format!("cannot unpack {}: {err}", target.display()))
        })?;
    }
    let binary = stage.join(MODULE_BINARY);
    match std::fs::metadata(&binary) {
        Ok(meta) if meta.is_file() => {}
        _ => return Err(ErrorEnvelope::bad_request(strings::ARTIFACT_MISSING_BINARY)),
    }
    // The core CLI execs this file; an artifact that lost its mode bit is repaired here
    // rather than failing later as a permission error.
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot set {}: {err}", binary.display()))
    })?;
    Ok(())
}

/// Move the staged tree to `<parent>/<version>`, honouring the replace rules: a real
/// directory needs `--force`; a symlink or any other non-directory is never replaced.
fn place(parent: &Path, stage: &Path, version: &str, force: bool) -> Result<(), ErrorEnvelope> {
    let target = parent.join(version);
    let mut previous = None;
    match std::fs::symlink_metadata(&target) {
        // `symlink_metadata` describes the link itself: a symlink is not a directory here,
        // so it falls into this arm and is never replaced.
        Ok(meta) if !meta.is_dir() => {
            return Err(ErrorEnvelope::bad_request(
                strings::module_version_not_a_directory(&target.display().to_string()),
            ));
        }
        Ok(_) if !force => {
            return Err(ErrorEnvelope::bad_request(
                strings::module_already_installed(version, &target.display().to_string()),
            ));
        }
        Ok(_) => {
            let aside = parent.join(format!(".old-{}", std::process::id()));
            if aside.exists() {
                let _ = std::fs::remove_dir_all(&aside);
            }
            std::fs::rename(&target, &aside).map_err(|err| {
                ErrorEnvelope::bad_request(format!("cannot replace {}: {err}", target.display()))
            })?;
            previous = Some(aside);
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(ErrorEnvelope::bad_request(format!(
                "cannot inspect {}: {err}",
                target.display()
            )));
        }
    }
    if let Err(err) = std::fs::rename(stage, &target) {
        // N2: the old version was moved aside before this rename; restore it so a failed
        // install leaves the tree exactly as it found it instead of a missing version path.
        if let Some(aside) = &previous {
            let _ = std::fs::rename(aside, &target);
        }
        return Err(ErrorEnvelope::bad_request(format!(
            "cannot install {}: {err}",
            target.display()
        )));
    }
    if let Some(aside) = previous {
        let _ = std::fs::remove_dir_all(&aside);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comes_from_the_asset_name_between_the_prefix_and_the_triple() {
        assert_eq!(
            version_from_asset_name("daiorama-dashboard-2026.10.0-x86_64-unknown-linux-gnu.tar.gz")
                .as_deref(),
            Some("2026.10.0")
        );
        assert_eq!(
            version_from_asset_name(
                "daiorama-dashboard-0.2.0-rc1-aarch64-unknown-linux-musl.tar.gz"
            )
            .as_deref(),
            Some("0.2.0-rc1")
        );
    }

    #[test]
    fn an_unknown_shape_or_triple_yields_no_version() {
        assert_eq!(
            version_from_asset_name("daiorama-dashboard-0.2.0.zip"),
            None
        );
        assert_eq!(
            version_from_asset_name("daiorama-dashboard-0.2.0-riscv64-unknown-linux-gnu.tar.gz"),
            None
        );
        assert_eq!(version_from_asset_name("something-else.tar.gz"), None);
    }

    #[test]
    fn entry_paths_and_kinds_are_refused_by_the_filter() {
        use std::path::Path;
        assert!(entry_path_refused(Path::new("../evil")));
        assert!(entry_path_refused(Path::new("/etc/passwd")));
        assert!(!entry_path_refused(Path::new("./daiorama-dashboard")));
        assert!(!entry_path_refused(Path::new("ui/index.html")));
        assert!(entry_kind_refused(tar::EntryType::Symlink));
        assert!(entry_kind_refused(tar::EntryType::Link));
        assert!(!entry_kind_refused(tar::EntryType::Regular));
        assert!(!entry_kind_refused(tar::EntryType::Directory));
    }

    #[test]
    fn the_unpacked_budget_refuses_past_the_cap() {
        let ok = budget(MAX_UNPACKED_BYTES - 1, 1).expect("exactly at the cap");
        assert_eq!(ok, MAX_UNPACKED_BYTES);
        let err = budget(MAX_UNPACKED_BYTES - 1, 2).expect_err("one byte over");
        assert!(err.message.contains("expands beyond"), "{err:?}");
    }

    fn artifact_with(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "daiorama-dashboard", content.as_bytes())
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn forcing_the_active_version_leaves_active_resolving() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_env(
            Some(&root.path().join("config")),
            Some(&root.path().join("data")),
            Some(root.path()),
            "t",
        );
        let name = "daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz";
        let first = artifact_with(root.path(), name, "first");
        let sha_first = file_sha256(&first).unwrap();
        install(&paths, &first, &sha_first, None, false).expect("first install");

        let second = artifact_with(root.path(), name, "second");
        let sha_second = file_sha256(&second).unwrap();
        let installed =
            install(&paths, &second, &sha_second, Some("0.1.0"), true).expect("forced reinstall");
        let link = std::fs::read_link(installed.dir.join("..").join("active")).unwrap();
        assert_eq!(link.file_name().unwrap(), "0.1.0");
        assert_eq!(
            std::fs::read_to_string(installed.dir.join("daiorama-dashboard")).unwrap(),
            "second"
        );
    }

    #[test]
    fn a_failed_stage_rename_restores_the_previous_version() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("modules").join("daiorama-dashboard");
        std::fs::create_dir_all(parent.join("0.1.0")).unwrap();
        std::fs::write(parent.join("0.1.0").join("daiorama-dashboard"), "old").unwrap();
        // No stage directory: the rename fails after the old version was moved aside.
        let missing = parent.join(".stage-missing");
        let err = place(&parent, &missing, "0.1.0", true).expect_err("rename must fail");
        assert!(err.message.contains("cannot install"), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(parent.join("0.1.0").join("daiorama-dashboard")).unwrap(),
            "old",
            "the previous version is restored, not lost"
        );
    }

    fn artifact_without_binary(dir: &Path, name: &str) -> std::path::PathBuf {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(4);
        header.set_cksum();
        builder
            .append_data(&mut header, "assets/index.html", &b"html"[..])
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn a_failed_forced_install_puts_the_active_link_back() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_env(
            Some(&root.path().join("config")),
            Some(&root.path().join("data")),
            Some(root.path()),
            "t",
        );
        let name = "daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz";
        let first = artifact_with(root.path(), name, "first");
        let sha_first = file_sha256(&first).unwrap();
        let installed = install(&paths, &first, &sha_first, None, false).expect("first install");

        // Same version, forced, and the unpack fails after the link was cleared: the tree
        // must come back exactly as it was, `active` included.
        let broken = artifact_without_binary(root.path(), name);
        let sha_broken = file_sha256(&broken).unwrap();
        let err = match install(&paths, &broken, &sha_broken, Some("0.1.0"), true) {
            Ok(_) => panic!("an artifact without the module binary must be refused"),
            Err(err) => err,
        };
        assert!(err.message.contains("does not contain"), "{err:?}");
        let link = std::fs::read_link(installed.dir.join("..").join("active")).unwrap();
        assert_eq!(link.file_name().unwrap(), "0.1.0");
        assert_eq!(
            std::fs::read_to_string(installed.dir.join("daiorama-dashboard")).unwrap(),
            "first"
        );
    }

    #[test]
    fn a_refused_first_install_leaves_no_module_tree() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_env(
            Some(&root.path().join("config")),
            Some(&root.path().join("data")),
            Some(root.path()),
            "t",
        );
        let broken = artifact_without_binary(
            root.path(),
            "daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz",
        );
        let sha = file_sha256(&broken).unwrap();
        let err = match install(&paths, &broken, &sha, None, false) {
            Ok(_) => panic!("an artifact without the module binary must be refused"),
            Err(err) => err,
        };
        assert!(err.message.contains("does not contain"), "{err:?}");
        assert!(
            !paths.modules_dir().join("daiorama-dashboard").exists(),
            "the refused install created the module tree anyway"
        );
        assert!(
            !paths.modules_dir().exists(),
            "the refused install left the modules directory behind (N1)"
        );
    }
}
