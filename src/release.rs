//! Signed release manifests (09 C2): the verifier half. The manifest is JSON signed with
//! minisign; the binary verifies it with a pure-Rust crate (never a shelled-out `minisign`, which
//! would make the verifier trust the thing it is verifying), and only then trusts a digest from
//! it. The trust list is data (`packaging/release/trusted-keys`), embedded at build; empty until
//! the first operator-signed release provisions a key, in which case verification refuses rather
//! than trusting anything.
//!
//! Bounds are part of the contract. A manifest or signature is a small document, read only after
//! its size is checked; an artifact is hashed through the installer's bounded path (128 MiB),
//! because `--artifact` names a release download and a read side with no cap reads green until it
//! OOMs. Rows are validated after the signature verifies — a signed manifest that names an
//! unknown target, channel, kind or digest shape is still a broken release, and refusing it is
//! cheaper than letting an installer select a row the release lane never built.

use std::collections::BTreeSet;
use std::path::Path;

use minisign_verify::{PublicKey, Signature};
use serde_json::Value;

use crate::ipc::ErrorEnvelope;
use crate::strings;

/// Checked-in trust anchors: one minisign public key per entry (untrusted-comment line followed
/// by the base64 key). Rotation adds a line; removal is a deliberate release.
pub const TRUSTED_KEYS: &str = include_str!("../packaging/release/trusted-keys");

/// Manifest and signature documents are small; anything larger is refused before it is read.
pub const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;

/// The closed sets 09 pins. A new kind or channel is a manifest-format change, not a guess.
pub const KINDS: [&str; 3] = ["core", "dashboard", "image"];
pub const CHANNELS: [&str; 2] = ["stable", "beta"];
/// Image rows are named by platform, never by tag; these are the platforms the lane builds.
pub const IMAGE_TARGETS: [&str; 2] = ["linux/amd64", "linux/arm64"];

#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    pub version: String,
    pub protocol: String,
    pub key_id: String,
    pub artifact: Option<ArtifactRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRow {
    pub name: String,
    pub target: String,
    pub kind: String,
    pub sha256: String,
    pub size: u64,
}

struct TrustedKey {
    id: String,
    key: PublicKey,
}

/// Base64 decode (standard alphabet, padding optional) — key ids live inside the raw minisign
/// bytes, which the verification crate does not expose.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.trim().bytes() {
        if byte == b'=' {
            break;
        }
        let value = TABLE.iter().position(|entry| *entry == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// Minisign stores the 8-byte key id at bytes 2..10 of both public keys and signatures, displayed
/// most-significant byte first.
fn key_id(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 10 {
        return None;
    }
    Some(
        bytes[2..10]
            .iter()
            .rev()
            .map(|byte| format!("{byte:02X}"))
            .collect(),
    )
}

fn signature_key_id(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))?;
    key_id(&base64_decode(line)?)
}

fn parse_trusted(text: &str) -> Result<Vec<TrustedKey>, ErrorEnvelope> {
    let mut keys = Vec::new();
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with("untrusted comment:") {
            return Err(ErrorEnvelope::bad_request(format!(
                "trusted key file: expected an untrusted-comment line, got {line:?}"
            )));
        }
        let encoded = loop {
            match lines.next() {
                Some(value) if value.is_empty() || value.starts_with('#') => continue,
                Some(value) => break value,
                None => {
                    return Err(ErrorEnvelope::bad_request(
                        "trusted key file: public key line is missing",
                    ));
                }
            }
        };
        let key = PublicKey::from_base64(encoded).map_err(|error| {
            ErrorEnvelope::bad_request(format!("trusted key file: invalid public key: {error}"))
        })?;
        let id = key_id(&base64_decode(encoded).ok_or_else(|| {
            ErrorEnvelope::bad_request("trusted key file: invalid base64 in public key")
        })?)
        .ok_or_else(|| ErrorEnvelope::bad_request("trusted key file: public key is too short"))?;
        keys.push(TrustedKey { id, key });
    }
    Ok(keys)
}

/// Read a file after checking its size: a named manifest, signature or artifact is never loaded
/// on the strength of its name.
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ErrorEnvelope> {
    let meta = std::fs::metadata(path).map_err(|error| {
        ErrorEnvelope::bad_request(format!("cannot read {}: {error}", path.display()))
    })?;
    if !meta.is_file() {
        return Err(ErrorEnvelope::bad_request(format!(
            "cannot read {}: not a regular file",
            path.display()
        )));
    }
    if meta.len() > limit {
        return Err(ErrorEnvelope::bad_request(
            strings::release_document_too_large(
                &path.display().to_string(),
                meta.len(),
                limit / (1024 * 1024),
            ),
        ));
    }
    std::fs::read(path).map_err(|error| {
        ErrorEnvelope::bad_request(format!("cannot read {}: {error}", path.display()))
    })
}

/// Verify a manifest signature, validate its rows, then (optionally) an artifact against its
/// row. `trusted` is the parsed trust-list text; the CLI passes [`TRUSTED_KEYS`], tests pass a
/// fixture key. Order is the contract: the signature covers the bytes as shipped, so the JSON is
/// parsed — and any digest trusted — only after it verifies.
pub fn verify(
    manifest: &Path,
    signature: &Path,
    artifact: Option<&Path>,
    trusted: &str,
) -> Result<Verified, ErrorEnvelope> {
    let keys = parse_trusted(trusted)?;
    if keys.is_empty() {
        return Err(ErrorEnvelope::bad_request(strings::RELEASE_NO_TRUSTED_KEY));
    }
    let bytes = read_bounded(manifest, MAX_DOCUMENT_BYTES)?;
    let signature_text =
        String::from_utf8(read_bounded(signature, MAX_DOCUMENT_BYTES)?).map_err(|_| {
            ErrorEnvelope::bad_request(format!("cannot read {}: not UTF-8", signature.display()))
        })?;
    let signature = Signature::decode(&signature_text).map_err(|error| {
        ErrorEnvelope::bad_request(format!("invalid release signature: {error}"))
    })?;

    let mut signer = None;
    for key in &keys {
        if key.key.verify(&bytes, &signature, false).is_ok() {
            signer = Some(key);
            break;
        }
    }
    let signer = signer.ok_or_else(|| {
        let refusal = signature_key_id(&signature_text)
            .filter(|id| !keys.iter().any(|key| &key.id == id))
            .map(|id| strings::release_untrusted_key(&id))
            .unwrap_or_else(|| strings::RELEASE_SIGNATURE_INVALID.to_string());
        ErrorEnvelope::bad_request(refusal)
    })?;

    let manifest_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ErrorEnvelope::bad_request(format!("release manifest is not valid JSON: {error}"))
    })?;
    validate(&manifest_json)?;

    let version = field(&manifest_json, "version")?;
    let protocol = field(&manifest_json, "protocol")?;
    let matched = match artifact {
        None => None,
        Some(path) => Some(match_artifact(&manifest_json, path)?),
    };
    Ok(Verified {
        version,
        protocol,
        key_id: signer.id.clone(),
        artifact: matched,
    })
}

/// Validate a signed manifest against 09's pinned shape. The signature proved the bytes are the
/// release lane's; this proves the release lane did not ship a row nobody can install.
fn validate(manifest: &Value) -> Result<(), ErrorEnvelope> {
    let format = manifest.get("format").and_then(Value::as_u64);
    if format != Some(1) {
        return Err(ErrorEnvelope::bad_request(format!(
            "unsupported release manifest format {}",
            format
                .map(|value| value.to_string())
                .unwrap_or_else(|| "missing".to_string())
        )));
    }
    let version = field(manifest, "version")?;
    if !valid_version(&version) {
        return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
            "version", &version,
        )));
    }
    let protocol = field(manifest, "protocol")?;
    if !valid_version(&protocol) {
        return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
            "protocol", &protocol,
        )));
    }
    let channel = field(manifest, "channel")?;
    if !CHANNELS.contains(&channel.as_str()) {
        return Err(ErrorEnvelope::bad_request(
            strings::release_unknown_channel(&channel),
        ));
    }
    let released_at = field(manifest, "released_at")?;
    if !valid_rfc3339_utc(&released_at) {
        return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
            "released_at",
            &released_at,
        )));
    }

    let facts = manifest
        .get("facts")
        .filter(|value| value.is_object())
        .ok_or_else(|| ErrorEnvelope::bad_request(strings::release_missing_field("facts")))?;
    if facts.get("client_only").and_then(Value::as_bool).is_none() {
        return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
            "facts.client_only",
            &facts
                .get("client_only")
                .map(Value::to_string)
                .unwrap_or_default(),
        )));
    }
    for name in ["manager_platforms", "components"] {
        let values = facts
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| ErrorEnvelope::bad_request(strings::release_missing_field(name)))?;
        if values.is_empty()
            || values
                .iter()
                .any(|value| value.as_str().is_none_or(str::is_empty))
        {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
                name,
                &Value::Array(values.clone()).to_string(),
            )));
        }
    }

    let rows = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| ErrorEnvelope::bad_request(strings::release_missing_field("artifacts")))?;
    if rows.is_empty() {
        return Err(ErrorEnvelope::bad_request(strings::RELEASE_NO_ARTIFACTS));
    }
    let mut names = BTreeSet::new();
    let mut slots = BTreeSet::new();
    for row in rows {
        let name = field(row, "name")?;
        if !valid_artifact_name(&name) {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_name(
                &name,
            )));
        }
        if !names.insert(name.clone()) {
            return Err(ErrorEnvelope::bad_request(strings::release_duplicate_row(
                &name,
            )));
        }
        let kind = field(row, "kind")?;
        if !KINDS.contains(&kind.as_str()) {
            return Err(ErrorEnvelope::bad_request(strings::release_unknown_kind(
                &kind,
            )));
        }
        let target = field(row, "target")?;
        if !known_target(&kind, &target) {
            return Err(ErrorEnvelope::bad_request(strings::release_unknown_target(
                &kind, &target,
            )));
        }
        if !slots.insert((kind.clone(), target.clone())) {
            return Err(ErrorEnvelope::bad_request(strings::release_duplicate_row(
                &name,
            )));
        }
        let sha256 = field(row, "sha256")?;
        if !valid_sha256(&sha256) {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_sha(
                &name,
            )));
        }
        if row
            .get("size")
            .and_then(Value::as_u64)
            .is_none_or(|size| size == 0)
        {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
                "size", &name,
            )));
        }
    }

    let assets = manifest
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| ErrorEnvelope::bad_request(strings::release_missing_field("assets")))?;
    let mut asset_names = BTreeSet::new();
    for asset in assets {
        let name = field(asset, "name")?;
        if !valid_artifact_name(&name) {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_name(
                &name,
            )));
        }
        if !asset_names.insert(name.clone()) {
            return Err(ErrorEnvelope::bad_request(strings::release_duplicate_row(
                &name,
            )));
        }
        if !valid_sha256(&field(asset, "sha256")?) {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_sha(
                &name,
            )));
        }
        if asset
            .get("size")
            .and_then(Value::as_u64)
            .is_none_or(|size| size == 0)
        {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
                "size", &name,
            )));
        }
    }
    Ok(())
}

fn field(value: &Value, name: &str) -> Result<String, ErrorEnvelope> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ErrorEnvelope::bad_request(strings::release_missing_field(name)))
}

fn known_target(kind: &str, target: &str) -> bool {
    if kind == "image" {
        IMAGE_TARGETS.contains(&target)
    } else {
        crate::install::TARGET_TRIPLES.contains(&target)
    }
}

/// `MAJOR.MINOR.PATCH`, each a decimal run. Release and protocol versions are plain triples.
fn valid_version(raw: &str) -> bool {
    let parts: Vec<&str> = raw.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 9 && part.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// `YYYY-MM-DDTHH:MM:SSZ` — UTC, no offsets, no fractional seconds (09 pins the shape).
fn valid_rfc3339_utc(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let slice = raw.get(range)?;
        if !slice.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        slice.parse().ok()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        digits(0..4),
        digits(5..7),
        digits(8..10),
        digits(11..13),
        digits(14..16),
        digits(17..19),
    ) else {
        return false;
    };
    year >= 2000
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
}

/// True for 64 lowercase hex characters.
pub fn valid_sha256(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// True for a published file name: one path component, no traversal, no leading dash.
pub fn valid_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
}

fn match_artifact(manifest: &Value, artifact: &Path) -> Result<ArtifactRow, ErrorEnvelope> {
    let name = artifact
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ErrorEnvelope::bad_request("artifact path has no file name"))?;
    let rows = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| ErrorEnvelope::bad_request(strings::release_missing_field("artifacts")))?;
    let row = rows
        .iter()
        .find(|row| row.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| {
            ErrorEnvelope::bad_request(format!("artifact {name:?} is not in the manifest"))
        })?;
    let expected = field(row, "sha256")?;
    // The installer's bounded hash path: regular file, 128 MiB cap, checked before any read.
    let actual = crate::install::file_sha256(artifact)?;
    if actual != expected {
        return Err(ErrorEnvelope::bad_request(format!(
            "artifact {name:?} sha256 {actual} does not match the manifest row {expected}"
        )));
    }
    Ok(ArtifactRow {
        name: name.to_string(),
        target: field(row, "target")?,
        kind: field(row, "kind")?,
        sha256: actual,
        size: row.get("size").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// One artifact row for the generator: the file's basename becomes the manifest name, so the
/// operator's build output naming is the contract (09).
pub struct ArtifactInput {
    pub kind: String,
    pub target: String,
    pub path: std::path::PathBuf,
}

pub struct AssetInput {
    pub name: String,
    pub path: std::path::PathBuf,
}

fn quoted(value: &str) -> Result<String, ErrorEnvelope> {
    serde_json::to_string(value)
        .map_err(|error| ErrorEnvelope::bad_request(format!("cannot encode JSON string: {error}")))
}

/// Hash an artifact through the installer's bounded path and report its size from metadata.
fn hashed(path: &Path) -> Result<(String, u64), ErrorEnvelope> {
    let meta = std::fs::metadata(path).map_err(|error| {
        ErrorEnvelope::bad_request(format!("cannot read {}: {error}", path.display()))
    })?;
    let sha256 = crate::install::file_sha256(path)?;
    Ok((sha256, meta.len()))
}

/// Render a manifest with one artifact/asset object per line and no whitespace inside an object:
/// a POSIX installer reads a row with `grep -F '"name":"…"'` + `sed` and needs no `jq`.
/// Signing is operator-side and by hand; this never touches a key.
pub fn manifest(
    version: &str,
    protocol: &str,
    channel: &str,
    released_at: &str,
    artifacts: &[ArtifactInput],
    assets: &[AssetInput],
    client_only: bool,
) -> Result<String, ErrorEnvelope> {
    if version.is_empty() || protocol.is_empty() || channel.is_empty() || released_at.is_empty() {
        return Err(ErrorEnvelope::bad_request(
            "version, protocol, channel and released-at are required",
        ));
    }
    if !valid_version(version) {
        return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
            "version", version,
        )));
    }
    if !valid_version(protocol) {
        return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
            "protocol", protocol,
        )));
    }
    if !CHANNELS.contains(&channel) {
        return Err(ErrorEnvelope::bad_request(
            strings::release_unknown_channel(channel),
        ));
    }
    if !valid_rfc3339_utc(released_at) {
        return Err(ErrorEnvelope::bad_request(strings::release_invalid_field(
            "released_at",
            released_at,
        )));
    }
    if artifacts.is_empty() {
        return Err(ErrorEnvelope::bad_request(strings::RELEASE_NO_ARTIFACTS));
    }
    let mut names = BTreeSet::new();
    let mut rows = Vec::new();
    for artifact in artifacts {
        let name = artifact
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| ErrorEnvelope::bad_request("artifact path has no file name"))?;
        if !valid_artifact_name(name) {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_name(
                name,
            )));
        }
        if !names.insert(name.to_string()) {
            return Err(ErrorEnvelope::bad_request(strings::release_duplicate_row(
                name,
            )));
        }
        if !KINDS.contains(&artifact.kind.as_str()) {
            return Err(ErrorEnvelope::bad_request(strings::release_unknown_kind(
                &artifact.kind,
            )));
        }
        if !known_target(&artifact.kind, &artifact.target) {
            return Err(ErrorEnvelope::bad_request(strings::release_unknown_target(
                &artifact.kind,
                &artifact.target,
            )));
        }
        let (sha256, size) = hashed(&artifact.path)?;
        rows.push((name.to_string(), artifact, sha256, size));
    }
    let mut asset_rows = Vec::new();
    for asset in assets {
        if !valid_artifact_name(&asset.name) {
            return Err(ErrorEnvelope::bad_request(strings::release_invalid_name(
                &asset.name,
            )));
        }
        let (sha256, size) = hashed(&asset.path)?;
        asset_rows.push((asset.name.clone(), sha256, size));
    }
    let unique_assets = asset_rows.iter().map(|row| &row.0).collect::<BTreeSet<_>>();
    if unique_assets.len() != asset_rows.len() {
        return Err(ErrorEnvelope::bad_request("asset names must be unique"));
    }

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"format\": 1,\n");
    out.push_str(&format!("  \"version\": {},\n", quoted(version)?));
    out.push_str(&format!("  \"protocol\": {},\n", quoted(protocol)?));
    out.push_str(&format!("  \"channel\": {},\n", quoted(channel)?));
    out.push_str(&format!("  \"released_at\": {},\n", quoted(released_at)?));
    out.push_str("  \"artifacts\": [\n");
    for (index, (name, artifact, sha256, size)) in rows.iter().enumerate() {
        out.push_str(&format!(
            "    {{\"name\":{},\"target\":{},\"kind\":{},\"sha256\":{},\"size\":{}}}{}\n",
            quoted(name)?,
            quoted(&artifact.target)?,
            quoted(&artifact.kind)?,
            quoted(sha256)?,
            size,
            if index + 1 == rows.len() { "" } else { "," }
        ));
    }
    out.push_str("  ],\n");
    out.push_str("  \"assets\": [\n");
    for (index, (name, sha256, size)) in asset_rows.iter().enumerate() {
        out.push_str(&format!(
            "    {{\"name\":{},\"sha256\":{},\"size\":{}}}{}\n",
            quoted(name)?,
            quoted(sha256)?,
            size,
            if index + 1 == asset_rows.len() {
                ""
            } else {
                ","
            }
        ));
    }
    out.push_str("  ],\n");
    let components: Vec<String> = artifacts
        .iter()
        .map(|artifact| artifact.kind.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let component_json = components
        .iter()
        .map(|kind| quoted(kind))
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    out.push_str(&format!(
        "  \"facts\": {{\"client_only\":{client_only},\"manager_platforms\":[\"linux\"],\"components\":[{component_json}]}}\n"
    ));
    out.push_str("}\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PUB: &str = include_str!("../tests/fixtures/release/test.pub");
    const MANIFEST: &str = include_str!("../tests/fixtures/release/manifest.json");
    const SIGNATURE: &str = include_str!("../tests/fixtures/release/manifest.json.minisig");

    fn fixture_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("manifest.json"), MANIFEST).unwrap();
        std::fs::write(dir.path().join("manifest.json.minisig"), SIGNATURE).unwrap();
        dir
    }

    #[test]
    fn empty_trust_list_refuses_before_any_crypto() {
        let dir = fixture_dir();
        let error = verify(
            &dir.path().join("manifest.json"),
            &dir.path().join("manifest.json.minisig"),
            None,
            "# empty\n",
        )
        .unwrap_err();
        assert_eq!(error.message, strings::RELEASE_NO_TRUSTED_KEY);
    }

    #[test]
    fn the_fixture_signature_verifies_against_its_public_key() {
        let dir = fixture_dir();
        let verified = verify(
            &dir.path().join("manifest.json"),
            &dir.path().join("manifest.json.minisig"),
            None,
            TEST_PUB,
        )
        .unwrap();
        assert_eq!(verified.version, "0.1.0");
        assert_eq!(verified.protocol, "0.1.0");
        assert_eq!(verified.key_id, "504C963C3ABCA66B");
        assert!(verified.artifact.is_none());
    }

    #[test]
    fn tampering_with_the_manifest_breaks_the_signature() {
        let dir = fixture_dir();
        let path = dir.path().join("manifest.json");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text = text.replace("\"version\":\"0.1.0\"", "\"version\":\"0.1.1\"");
        std::fs::write(&path, text).unwrap();
        let error = verify(
            &path,
            &dir.path().join("manifest.json.minisig"),
            None,
            TEST_PUB,
        )
        .unwrap_err();
        assert_eq!(error.message, strings::RELEASE_SIGNATURE_INVALID);
    }

    #[test]
    fn a_signer_the_list_does_not_carry_is_named() {
        let dir = fixture_dir();
        // A real minisign public key from the measured recipe, with a different key id: the
        // signature's id is not in the list, so the refusal names it instead of reading as a bad
        // signature.
        let other_key = concat!(
            "untrusted comment: minisign public key 9FB9F776CC628D65\n",
            "RWRljWLMdve5n9FU4FW9rkVEkUO/wF8/+txpfWOENsb8LFbqfJMgWvsh\n"
        );
        let error = verify(
            &dir.path().join("manifest.json"),
            &dir.path().join("manifest.json.minisig"),
            None,
            other_key,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            strings::release_untrusted_key("504C963C3ABCA66B")
        );
    }

    #[test]
    fn manifest_documents_are_bounded_before_they_are_read() {
        let dir = fixture_dir();
        let big = dir.path().join("big.json");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_DOCUMENT_BYTES + 1).unwrap();
        let error = verify(
            &big,
            &dir.path().join("manifest.json.minisig"),
            None,
            TEST_PUB,
        )
        .unwrap_err();
        assert!(
            error.message.contains("release document too large"),
            "{:?}",
            error.message
        );
    }

    #[test]
    fn artifact_hashes_go_through_the_bounded_installer_path() {
        let dir = fixture_dir();
        let name = "daiorama-0.1.0-x86_64-unknown-linux-gnu";
        let artifact = dir.path().join(name);
        let file = std::fs::File::create(&artifact).unwrap();
        file.set_len(crate::install::MAX_ARTIFACT_BYTES + 1)
            .unwrap();
        let error = verify(
            &dir.path().join("manifest.json"),
            &dir.path().join("manifest.json.minisig"),
            Some(&artifact),
            TEST_PUB,
        )
        .unwrap_err();
        assert!(
            error.message.contains("artifact too large"),
            "{:?}",
            error.message
        );
    }

    #[test]
    fn artifact_rows_match_by_name_and_digest() {
        let dir = fixture_dir();
        std::fs::write(
            dir.path().join("daiorama-0.1.0-x86_64-unknown-linux-gnu"),
            b"x",
        )
        .unwrap();
        let error = verify(
            &dir.path().join("manifest.json"),
            &dir.path().join("manifest.json.minisig"),
            Some(&dir.path().join("daiorama-0.1.0-x86_64-unknown-linux-gnu")),
            TEST_PUB,
        )
        .unwrap_err();
        assert!(
            error.message.contains("does not match the manifest row"),
            "{error:?}"
        );

        let error = verify(
            &dir.path().join("manifest.json"),
            &dir.path().join("manifest.json.minisig"),
            Some(&dir.path().join("not-in-the-manifest")),
            TEST_PUB,
        )
        .unwrap_err();
        assert!(
            error.message.contains("is not in the manifest"),
            "{error:?}"
        );
    }

    #[test]
    fn schema_refusals_cover_the_pinned_sets() {
        let cases = [
            (
                MANIFEST.replace("\"channel\":\"stable\"", "\"channel\":\"nightly\""),
                "unknown release channel",
            ),
            (
                MANIFEST.replace(
                    "\"target\":\"x86_64-unknown-linux-gnu\"",
                    "\"target\":\"windows-386\"",
                ),
                "outside the release set",
            ),
            (
                MANIFEST.replace("\"kind\":\"core\"", "\"kind\":\"kernel\""),
                "unknown artifact kind",
            ),
            (
                MANIFEST.replace(
                    "\"sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\"",
                    "\"sha256\":\"ABC\"",
                ),
                "invalid sha256",
            ),
            (
                MANIFEST.replace(
                    "\"released_at\":\"2026-10-18T00:00:00Z\"",
                    "\"released_at\":\"yesterday\"",
                ),
                "invalid value",
            ),
        ];
        for (text, needle) in cases {
            let parsed: Value = serde_json::from_str(&text).unwrap();
            let error = validate(&parsed).unwrap_err();
            assert!(error.message.contains(needle), "{needle:?}: {error:?}");
        }
    }

    #[test]
    fn the_fixture_manifest_validates() {
        let parsed: Value = serde_json::from_str(MANIFEST).unwrap();
        validate(&parsed).expect("the fixture is a valid manifest");
    }

    #[test]
    fn generated_manifest_has_one_compact_object_per_line_and_real_digests() {
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("daiorama-0.1.0-x86_64-unknown-linux-gnu");
        std::fs::write(&core, b"core bytes").unwrap();
        let asset = dir.path().join("novnc.tar.gz");
        std::fs::write(&asset, b"asset bytes").unwrap();
        let text = manifest(
            "0.1.0",
            "0.1.0",
            "stable",
            "2026-10-18T00:00:00Z",
            &[ArtifactInput {
                kind: "core".into(),
                target: "x86_64-unknown-linux-gnu".into(),
                path: core,
            }],
            &[AssetInput {
                name: "novnc-1.7.0.tar.gz".into(),
                path: asset,
            }],
            false,
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let rows = parsed["artifacts"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["sha256"].as_str().unwrap(),
            crate::crypto::hex(&crate::crypto::sha256(b"core bytes"))
        );
        assert_eq!(rows[0]["size"].as_u64(), Some(10));
        assert_eq!(parsed["assets"][0]["name"], "novnc-1.7.0.tar.gz");
        let object_lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("{\"name\":"))
            .collect();
        assert_eq!(object_lines.len(), 2, "one artifact and one asset per line");
        for line in object_lines {
            assert!(
                !line.contains(": ") && !line.contains(", "),
                "no whitespace inside an object: {line}"
            );
        }
        // The installer's pinned extraction: grep -F '"name":"…"' then sed the sha256 group.
        let name = "daiorama-0.1.0-x86_64-unknown-linux-gnu";
        let row = text
            .lines()
            .find(|line| line.contains(&format!("\"name\":\"{name}\"")))
            .expect("grep -F finds the row");
        let digest = row
            .split("\"sha256\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("sed extracts the digest");
        assert_eq!(digest.len(), 64);
        assert!(text.contains("\"components\":[\"core\"]"));
    }

    #[test]
    fn the_generator_refuses_what_the_verifier_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("daiorama-0.1.0-x86_64-unknown-linux-gnu");
        std::fs::write(&core, b"x").unwrap();
        let cases = [
            (
                "nightly",
                "core",
                "x86_64-unknown-linux-gnu",
                "unknown release channel",
            ),
            (
                "stable",
                "kernel",
                "x86_64-unknown-linux-gnu",
                "unknown artifact kind",
            ),
            ("stable", "core", "windows-386", "outside the release set"),
        ];
        for (channel, kind, target, needle) in cases {
            let error = manifest(
                "0.1.0",
                "0.1.0",
                channel,
                "2026-10-18T00:00:00Z",
                &[ArtifactInput {
                    kind: kind.into(),
                    target: target.into(),
                    path: core.clone(),
                }],
                &[],
                false,
            )
            .unwrap_err();
            assert!(error.message.contains(needle), "{needle:?}: {error:?}");
        }
        let error = manifest(
            "0.1.0",
            "0.1.0",
            "stable",
            "2026-10-18T00:00:00Z",
            &[],
            &[],
            false,
        )
        .unwrap_err();
        assert_eq!(error.message, strings::RELEASE_NO_ARTIFACTS);
    }

    #[test]
    fn artifact_names_must_be_unique() {
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("same-name");
        std::fs::write(&core, b"x").unwrap();
        let error = manifest(
            "0.1.0",
            "0.1.0",
            "stable",
            "2026-10-18T00:00:00Z",
            &[
                ArtifactInput {
                    kind: "core".into(),
                    target: "x86_64-unknown-linux-gnu".into(),
                    path: core.clone(),
                },
                ArtifactInput {
                    kind: "core".into(),
                    target: "aarch64-unknown-linux-gnu".into(),
                    path: core,
                },
            ],
            &[],
            false,
        )
        .unwrap_err();
        assert!(error.message.contains("duplicate"), "{error:?}");
    }

    #[test]
    fn sha256_shape_is_the_documented_one() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256(&"a".repeat(63)));
        assert!(!valid_sha256(&"g".repeat(64)));
    }

    #[test]
    fn versions_and_timestamps_have_one_shape() {
        assert!(valid_version("0.1.0"));
        assert!(!valid_version("0.1"));
        assert!(!valid_version("v0.1.0"));
        assert!(valid_rfc3339_utc("2026-10-18T00:00:00Z"));
        assert!(!valid_rfc3339_utc("2026-10-18 00:00:00"));
        assert!(!valid_rfc3339_utc("2026-13-18T00:00:00Z"));
    }
}
