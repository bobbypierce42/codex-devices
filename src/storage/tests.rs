use super::*;
use serde::ser::{SerializeSeq, Serializer};
use std::cell::Cell;

#[cfg(unix)]
fn scratch() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/security-backups")
        .join(format!("{}-{stamp}-{sequence}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    root
}

struct Chunks<'a> {
    chunk: &'a str,
    visits: &'a Cell<usize>,
}

impl Serialize for Chunks<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(8))?;
        for _ in 0..8 {
            self.visits.set(self.visits.get() + 1);
            sequence.serialize_element(self.chunk)?;
        }
        sequence.end()
    }
}

#[test]
fn bounded_json_stops_visiting_input_when_output_exceeds_limit() {
    let chunk = "x".repeat(1024 * 1024);
    let visits = Cell::new(0);
    let Err(error) = json_bytes(&Chunks {
        chunk: &chunk,
        visits: &visits,
    }) else {
        panic!("Oversized JSON must be refused during serialization");
    };
    assert_eq!(error.kind, "refused");
    assert!(visits.get() < 8, "Serialization continued past the limit");
}

#[test]
fn bounded_json_accepts_exact_limit_with_unchanged_bytes() {
    let value = "x".repeat(MAX_BYTES as usize - 2);
    let encoded = json_bytes(&value).unwrap();
    assert_eq!(encoded.len(), MAX_BYTES as usize);
    assert!(encoded == serde_json::to_vec_pretty(&value).unwrap());
}

#[test]
fn bounded_json_refuses_one_byte_over_limit() {
    let value = "x".repeat(MAX_BYTES as usize - 1);
    let Err(error) = json_bytes(&value) else {
        panic!("Output exceeds limit");
    };
    assert_eq!(error.kind, "refused");
}

#[test]
fn streaming_inventory_digest_preserves_legacy_pretty_bytes_above_artifact_limit() {
    let value = ("prefix", "x".repeat(MAX_BYTES as usize));
    let legacy = serde_json::to_vec_pretty(&value).unwrap();
    assert!(legacy.len() > MAX_BYTES as usize);
    assert_eq!(
        crate::encoding::json_digest(&value).unwrap(),
        sha256(&legacy)
    );
}

#[cfg(unix)]
#[test]
fn backup_verification_reads_the_held_directory_after_path_replacement() {
    let root = scratch();
    let path = root.join("backup");
    let receipt = backup(crate::DEMO, &path).unwrap();
    let held = Directory::open(&path).unwrap();
    fs::rename(&path, root.join("saved-backup")).unwrap();
    let substituted = Directory::create(&path).unwrap();
    substituted.write_new("manifest.json", b"{}").unwrap();
    assert_eq!(
        verify_directory(&held, Some(&receipt.manifest_sha256))
            .unwrap()
            .integrity,
        "verified"
    );
    assert!(verify(&path, Some(&receipt.manifest_sha256)).is_err());
}

#[test]
fn invalid_manifest_digest_is_rejected_before_opening_a_directory() {
    let Err(error) = verify(Path::new("missing-for-invalid-digest"), Some("invalid")) else {
        panic!("Invalid digest must be refused before file access");
    };
    assert_eq!(error.kind, "usage");
}

#[cfg(unix)]
#[test]
fn writable_artifact_parent_is_refused_before_destination_creation() {
    use std::os::unix::fs::PermissionsExt;
    let parent = scratch();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o702)).unwrap();
    let path = parent.join("backup");
    let result = Directory::create(&path);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    let Err(error) = result else {
        panic!("An untrusted writable parent must be refused")
    };
    assert_eq!(error.kind, "refused");
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn directory_members_cannot_escape_the_held_root() {
    let root = scratch();
    let directory = Directory::create(&root.join("inside")).unwrap();
    let outside = root.join("outside");
    fs::write(&outside, b"unchanged synthetic sentinel").unwrap();
    for bad in ["", ".", "..", "../outside", "child/file"] {
        assert!(directory.read(bad).is_err());
        assert!(directory.write_new(bad, b"unapproved").is_err());
        assert!(directory.create_child(bad).is_err());
        assert!(directory.replace("missing", bad).is_err());
    }
    assert!(directory.read(&outside).is_err());
    assert!(directory.write_new(&outside, b"unapproved").is_err());
    assert_eq!(fs::read(outside).unwrap(), b"unchanged synthetic sentinel");
    assert_eq!(fs::read_dir(root.join("inside")).unwrap().count(), 0);
}
