pub(crate) use crate::encoding::json_bytes;
use crate::{AuditReport, Error, MAX_BYTES, Result, Source, audit, sha256};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) mod directory;
use directory::Directory;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    kind: String,
    created_at_unix_seconds: u64,
    source: Source,
    files: Vec<Entry>,
}

#[derive(Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    operation: String,
    source: Source,
    catalog_sha256: String,
    actions: Vec<String>,
    protected_environment_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct BackupReceipt {
    pub schema_version: u32,
    pub operation: &'static str,
    pub mode: &'static str,
    pub backup_directory: String,
    pub manifest_sha256: String,
    pub verification: Verification,
}

#[derive(Serialize)]
pub struct Verification {
    pub schema_version: u32,
    pub operation: &'static str,
    pub mode: &'static str,
    pub integrity: &'static str,
    pub manifest_anchor: &'static str,
    pub manifest_sha256: String,
    pub record_count: usize,
    pub private_permissions: &'static str,
    pub live_state_verified: bool,
    pub restore_supported: bool,
}

pub(crate) fn local_path(path: &Path) -> Result<()> {
    let text = path.as_os_str().to_string_lossy();
    if text.contains("://")
        || text.starts_with("//")
        || text.starts_with("\\\\")
        || path.components().any(|c| c == Component::ParentDir)
    {
        return Err(Error::refused(
            "Use an explicit local path without a URL, UNC prefix, or parent traversal.",
        ));
    }
    Ok(())
}

pub(crate) fn no_symlinks(path: &Path) -> Result<()> {
    local_path(path)?;
    let mut part = PathBuf::new();
    for component in path.components() {
        part.push(component.as_os_str());
        if fs::symlink_metadata(&part)?.file_type().is_symlink() {
            return Err(Error::refused(
                "Symlinks in input or backup paths are not supported.",
            ));
        }
    }
    Ok(())
}

pub fn read_input(path: &Path) -> Result<Vec<u8>> {
    no_symlinks(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(Error::refused(
            "Input must be a regular local file no larger than 4 MiB.",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::refused("Input grew beyond the 4 MiB limit."));
    }
    Ok(bytes)
}

fn plan_for(report: &AuditReport, digest: String) -> Plan {
    Plan {
        schema_version: 1,
        operation: "none".into(),
        source: report.source.clone(),
        catalog_sha256: digest,
        actions: Vec::new(),
        protected_environment_ids: report.records.iter().map(|r| r.selector.clone()).collect(),
    }
}

pub fn backup(bytes: &[u8], directory: &Path) -> Result<BackupReceipt> {
    backup_with(bytes, directory, || Directory::create(directory))
}

pub(crate) fn backup_in(
    bytes: &[u8],
    parent: &Directory,
    display_path: &Path,
) -> Result<BackupReceipt> {
    backup_with(bytes, display_path, || parent.create_child("backup"))
}

fn backup_with(
    bytes: &[u8],
    display_path: &Path,
    create: impl FnOnce() -> Result<Directory>,
) -> Result<BackupReceipt> {
    let report = audit(bytes)?;
    if !report.complete {
        return Err(Error::refused(
            "An incomplete inventory cannot become a verified backup.",
        ));
    }
    let plan = json_bytes(&plan_for(&report, sha256(bytes)))?;
    let manifest = Manifest {
        schema_version: 1,
        kind: "offline-synthetic-backup".into(),
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::new("io", "Clock is before the Unix epoch.", 1))?
            .as_secs(),
        source: report.source,
        files: vec![
            Entry {
                path: "catalog.json".into(),
                bytes: bytes.len() as u64,
                sha256: sha256(bytes),
            },
            Entry {
                path: "plan.json".into(),
                bytes: plan.len() as u64,
                sha256: sha256(&plan),
            },
        ],
    };
    let manifest = json_bytes(&manifest)?;
    let manifest_sha256 = sha256(&manifest);
    let directory = create()?;
    directory.write_new("catalog.json", bytes)?;
    directory.write_new("plan.json", &plan)?;
    // The manifest is the completion marker; interrupted writes stay unverified.
    directory.write_new("manifest.json", &manifest)?;
    directory.sync()?;
    let verification = verify_directory(&directory, Some(&manifest_sha256))?;
    Ok(BackupReceipt {
        schema_version: 1,
        operation: "backup",
        mode: "offline-synthetic",
        backup_directory: display_path.to_string_lossy().into_owned(),
        manifest_sha256,
        verification,
    })
}

#[cfg(test)]
mod tests;

pub fn verify(directory: &Path, expected_manifest: Option<&str>) -> Result<Verification> {
    check_manifest_digest(expected_manifest)?;
    verify_directory(&Directory::open(directory)?, expected_manifest)
}

fn check_manifest_digest(expected_manifest: Option<&str>) -> Result<()> {
    if let Some(expected) = expected_manifest
        && (expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Error::new(
            "usage",
            "Expected manifest SHA-256 must contain exactly 64 hexadecimal digits.",
            2,
        ));
    }
    Ok(())
}

pub(crate) fn verify_directory(
    directory: &Directory,
    expected_manifest: Option<&str>,
) -> Result<Verification> {
    check_manifest_digest(expected_manifest)?;
    for name in ["catalog.json", "plan.json", "manifest.json"] {
        directory.check_file(name)?;
    }
    directory.check_members(&["catalog.json", "plan.json", "manifest.json"], None)?;
    let manifest_bytes = directory.read("manifest.json")?;
    let manifest_sha256 = sha256(&manifest_bytes);
    if expected_manifest.is_some_and(|expected| !expected.eq_ignore_ascii_case(&manifest_sha256)) {
        return Err(Error::integrity(
            "Manifest does not match the trusted digest.",
        ));
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| Error::integrity("Invalid backup manifest."))?;
    if manifest.schema_version != 1
        || manifest.kind != "offline-synthetic-backup"
        || manifest.files.len() != 2
        || manifest.files[0].path != "catalog.json"
        || manifest.files[1].path != "plan.json"
    {
        return Err(Error::integrity(
            "Unsupported manifest schema or member paths.",
        ));
    }
    let catalog = directory.read("catalog.json")?;
    let plan_bytes = directory.read("plan.json")?;
    for (entry, bytes) in manifest.files.iter().zip([&catalog, &plan_bytes]) {
        if entry.bytes != bytes.len() as u64 || entry.sha256 != sha256(bytes) {
            return Err(Error::integrity(
                "Backup member checksum or byte count does not match.",
            ));
        }
    }
    let report =
        audit(&catalog).map_err(|_| Error::integrity("Stored synthetic catalog is invalid."))?;
    if !report.complete || report.source != manifest.source {
        return Err(Error::integrity(
            "Stored inventory is incomplete or its source binding changed.",
        ));
    }
    let plan: Plan = serde_json::from_slice(&plan_bytes)
        .map_err(|_| Error::integrity("Invalid offline plan."))?;
    if plan != plan_for(&report, sha256(&catalog)) {
        return Err(Error::integrity(
            "Plan is not the expected no-mutation snapshot plan.",
        ));
    }
    Ok(Verification {
        schema_version: 1,
        operation: "verify",
        mode: "offline-synthetic",
        integrity: "verified",
        manifest_anchor: if expected_manifest.is_some() {
            "matched"
        } else {
            "not_supplied"
        },
        manifest_sha256,
        record_count: report.counts.total,
        private_permissions: "posix_mode_checked",
        live_state_verified: false,
        restore_supported: false,
    })
}
