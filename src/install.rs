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
    let parent = paths.modules_dir().join("daiorama-dashboard");
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
    let staged =
        extract_artifact(artifact, &stage).and_then(|()| place(&parent, &stage, &version, force));
    if let Err(err) = staged {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(err);
    }

    // 4. Activate, and report the version and path only.
    let dir = parent.join(&version);
    activate(&parent, &version)?;
    Ok(Installed { version, dir })
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
        if path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(ErrorEnvelope::bad_request(strings::artifact_entry_refused(
                &path.display().to_string(),
            )));
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
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
    std::fs::rename(stage, &target).map_err(|err| {
        ErrorEnvelope::bad_request(format!("cannot install {}: {err}", target.display()))
    })?;
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
}
