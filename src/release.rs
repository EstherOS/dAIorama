//! Signed release manifests (09 C2): the verifier half. The manifest is JSON signed with
//! minisign; the binary verifies it with a pure-Rust crate (never a shelled-out `minisign`, which
//! would make the verifier trust the thing it is verifying), and only then trusts a digest from
//! it. The trust list is data (`packaging/release/trusted-keys`), embedded at build; empty until
//! the first operator-signed release provisions a key, in which case verification refuses rather
//! than trusting anything.

use std::path::Path;

use minisign_verify::{PublicKey, Signature};
use serde_json::Value;

use crate::ipc::ErrorEnvelope;

/// Checked-in trust anchors: one minisign public key per entry (untrusted-comment line followed
/// by the base64 key). Rotation adds a line; removal is a deliberate release.
pub const TRUSTED_KEYS: &str = include_str!("../packaging/release/trusted-keys");

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

/// Minisign stores the 8-byte key id at bytes 2..10 of both public keys and signatures.
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
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with("untrusted comment:") {
            return Err(ErrorEnvelope::bad_request(format!(
                "trusted key file: expected an untrusted-comment line, got {line:?}"
            )));
        }
        let encoded = lines
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ErrorEnvelope::bad_request("trusted key file: public key line is missing")
            })?;
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

/// Verify a manifest signature, then (optionally) an artifact against its row. `trusted` is the
/// parsed trust-list text; the CLI passes [`TRUSTED_KEYS`], tests pass a fixture key.
pub fn verify(
    manifest: &Path,
    signature: &Path,
    artifact: Option<&Path>,
    trusted: &str,
) -> Result<Verified, ErrorEnvelope> {
    let keys = parse_trusted(trusted)?;
    if keys.is_empty() {
        return Err(ErrorEnvelope::bad_request(
            "no trusted release key provisioned",
        ));
    }
    let bytes = std::fs::read(manifest).map_err(|error| {
        ErrorEnvelope::bad_request(format!("cannot read {}: {error}", manifest.display()))
    })?;
    let signature_text = std::fs::read_to_string(signature).map_err(|error| {
        ErrorEnvelope::bad_request(format!("cannot read {}: {error}", signature.display()))
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
        let untrusted = signature_key_id(&signature_text)
            .filter(|id| !keys.iter().any(|key| &key.id == id))
            .map(|id| format!("release signed by an untrusted key {id}"))
            .unwrap_or_else(|| "release signature verification failed".to_string());
        ErrorEnvelope::bad_request(untrusted)
    })?;

    let manifest_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ErrorEnvelope::bad_request(format!("release manifest is not valid JSON: {error}"))
    })?;
    let format = manifest_json.get("format").and_then(Value::as_u64);
    if format != Some(1) {
        return Err(ErrorEnvelope::bad_request(format!(
            "unsupported release manifest format {}",
            format
                .map(|value| value.to_string())
                .unwrap_or_else(|| "missing".to_string())
        )));
    }
    let field = |name: &str| {
        manifest_json
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                ErrorEnvelope::bad_request(format!("release manifest is missing {name:?}"))
            })
    };
    let version = field("version")?;
    let protocol = field("protocol")?;

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

fn match_artifact(manifest: &Value, artifact: &Path) -> Result<ArtifactRow, ErrorEnvelope> {
    let name = artifact
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ErrorEnvelope::bad_request("artifact path has no file name"))?;
    let rows = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| ErrorEnvelope::bad_request("release manifest has no artifacts list"))?;
    let row = rows
        .iter()
        .find(|row| row.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| {
            ErrorEnvelope::bad_request(format!("artifact {name:?} is not in the manifest"))
        })?;
    let read = |key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                ErrorEnvelope::bad_request(format!("manifest row {name:?} is missing {key:?}"))
            })
    };
    let expected = read("sha256")?;
    let bytes = std::fs::read(artifact).map_err(|error| {
        ErrorEnvelope::bad_request(format!("cannot read {}: {error}", artifact.display()))
    })?;
    let actual = crate::crypto::hex(&crate::crypto::sha256(&bytes));
    if actual != expected {
        return Err(ErrorEnvelope::bad_request(format!(
            "artifact {name:?} sha256 {actual} does not match the manifest row {expected}"
        )));
    }
    Ok(ArtifactRow {
        name: name.to_string(),
        target: read("target")?,
        kind: read("kind")?,
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

fn hashed(path: &Path) -> Result<(String, u64), ErrorEnvelope> {
    let bytes = std::fs::read(path).map_err(|error| {
        ErrorEnvelope::bad_request(format!("cannot read {}: {error}", path.display()))
    })?;
    Ok((
        crate::crypto::hex(&crate::crypto::sha256(&bytes)),
        bytes.len() as u64,
    ))
}

/// Render a manifest with one artifact object per line (a POSIX installer can read a row with
/// grep/sed without `jq`). Signing is operator-side and by hand; this never touches a key.
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
    let mut names = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for artifact in artifacts {
        let name = artifact
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| ErrorEnvelope::bad_request("artifact path has no file name"))?;
        if !names.insert(name.to_string()) {
            return Err(ErrorEnvelope::bad_request(format!(
                "artifact {name:?} appears twice"
            )));
        }
        let (sha256, size) = hashed(&artifact.path)?;
        rows.push((name.to_string(), artifact, sha256, size));
    }
    let mut asset_rows = Vec::new();
    for asset in assets {
        let (sha256, size) = hashed(&asset.path)?;
        asset_rows.push((asset.name.clone(), sha256, size));
    }
    let unique_assets = asset_rows
        .iter()
        .map(|row| &row.0)
        .collect::<std::collections::BTreeSet<_>>();
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
            "    {{\"name\": {},\"target\": {},\"kind\": {},\"sha256\": {},\"size\": {}}}{}\n",
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
            "    {{\"name\": {},\"sha256\": {},\"size\": {}}}{}\n",
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
    let mut components: Vec<String> = artifacts
        .iter()
        .map(|artifact| artifact.kind.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    components.dedup();
    let component_json = components
        .iter()
        .map(|kind| quoted(kind))
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    out.push_str(&format!(
        "  \"facts\": {{\"client_only\": {client_only},\"manager_platforms\": [\"linux\"],\"components\": [{component_json}]}}\n"
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
        assert!(
            error.message.contains("no trusted release key provisioned"),
            "{error:?}"
        );
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
        assert!(
            error.message.contains("signature verification failed"),
            "{error:?}"
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
    fn generated_manifest_has_one_artifact_object_per_line_and_real_digests() {
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
        let object_lines = text
            .lines()
            .filter(|line| line.trim_start().starts_with("{\"name\""))
            .count();
        assert_eq!(
            object_lines, 2,
            "one artifact and one asset object per line"
        );
        assert!(text.contains("\"components\": [\"core\"]"));
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
                    target: "a".into(),
                    path: core.clone(),
                },
                ArtifactInput {
                    kind: "dashboard".into(),
                    target: "a".into(),
                    path: core,
                },
            ],
            &[],
            false,
        )
        .unwrap_err();
        assert!(error.message.contains("appears twice"), "{error:?}");
    }
}
