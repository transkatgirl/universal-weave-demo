//! Saving and loading documents using `rkyv` with a versioned file header.

use std::{fs, path::Path};

use rkyv::{
    rancor::{self, Strategy},
    util::AlignedVec,
};
use universal_weave::versioning::VersionedBytes;

use crate::content::{DemoWeave, IndependentDemoWeave};
use crate::document::Document;

/// Magic bytes identifying files produced by this demo.
const FORMAT_IDENTIFIER: [u8; 24] = *b"UNIVERSAL-WEAVE-DEMO\0\0\0\0";
/// Format version for dependent (tree-based) documents.
const VERSION_DEPENDENT: u64 = 1;
/// Format version for independent (DAG-based) documents.
const VERSION_INDEPENDENT: u64 = 2;

/// Serializes a document to a file, prefixed with a [`VersionedBytes`] header.
///
/// The header's version field identifies which weave implementation the payload contains.
pub fn save_document(path: &Path, document: &Document) -> Result<(), String> {
    let (version, data) = match document {
        Document::Dependent(weave) => (
            VERSION_DEPENDENT,
            rkyv::to_bytes::<rancor::Error>(weave.as_weave())
                .map_err(|e| format!("serialization failed: {e}"))?,
        ),
        Document::Independent(weave) => (
            VERSION_INDEPENDENT,
            rkyv::to_bytes::<rancor::Error>(weave.as_weave())
                .map_err(|e| format!("serialization failed: {e}"))?,
        ),
    };

    let versioned = VersionedBytes {
        format_identifier: FORMAT_IDENTIFIER,
        version,
        data: &data,
    };

    let mut output: AlignedVec = AlignedVec::with_capacity(versioned.output_length());
    versioned
        .write(Strategy::<_, rancor::Error>::wrap(&mut output))
        .map_err(|e| format!("header write failed: {e}"))?;

    fs::write(path, output.as_slice()).map_err(|e| format!("cannot write file: {e}"))
}

/// Loads and validates a document from a file produced by [`save_document`].
///
/// The file contents are copied into an aligned buffer before deserialization, as `rkyv`
/// requires 16-byte alignment (the 32-byte header preserves it).
pub fn load_document(path: &Path) -> Result<Document, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read file: {e}"))?;

    let mut aligned: AlignedVec = AlignedVec::with_capacity(bytes.len());
    aligned.extend_from_slice(&bytes);

    let versioned = VersionedBytes::try_from_bytes(aligned.as_slice(), FORMAT_IDENTIFIER)
        .ok_or_else(|| "not a universal-weave demo file (bad magic bytes)".to_string())?;

    // `from_bytes` runs the weave's built-in validation (robust to untrusted inputs)
    // before deserializing.
    match versioned.version {
        VERSION_DEPENDENT => {
            let weave = rkyv::from_bytes::<DemoWeave, rancor::Error>(versioned.data)
                .map_err(|e| format!("file failed validation: {e}"))?;
            Ok(Document::new_dependent(weave))
        }
        VERSION_INDEPENDENT => {
            let weave = rkyv::from_bytes::<IndependentDemoWeave, rancor::Error>(versioned.data)
                .map_err(|e| format!("file failed validation: {e}"))?;
            Ok(Document::new_independent(weave))
        }
        other => Err(format!(
            "unsupported format version {other} (expected {VERSION_DEPENDENT} or {VERSION_INDEPENDENT})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{WeaveKind, seeded_dependent, seeded_independent};

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("universal-weave-demo-test-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{name}.uweave"))
    }

    #[test]
    fn dependent_roundtrip() {
        let document = seeded_dependent();
        let path = temp_file("dependent-roundtrip");

        save_document(&path, &document).unwrap();
        let loaded = load_document(&path).unwrap();

        fs::remove_file(&path).ok();
        fs::remove_dir(&path.parent().unwrap()).ok();

        let Document::Dependent(loaded) = loaded else {
            panic!("expected a dependent document");
        };
        let Document::Dependent(original) = document else {
            unreachable!()
        };
        assert_eq!(original.as_weave(), loaded.as_weave());
    }

    #[test]
    fn independent_roundtrip() {
        let document = seeded_independent();
        let path = temp_file("independent-roundtrip");

        save_document(&path, &document).unwrap();
        let loaded = load_document(&path).unwrap();

        fs::remove_file(&path).ok();
        fs::remove_dir(&path.parent().unwrap()).ok();

        let Document::Independent(loaded) = loaded else {
            panic!("expected an independent document");
        };
        let Document::Independent(original) = document else {
            unreachable!()
        };
        assert_eq!(original.as_weave(), loaded.as_weave());
        assert!(loaded.as_weave().validate());
    }

    #[test]
    fn empty_document_roundtrip() {
        for kind in [WeaveKind::Dependent, WeaveKind::Independent] {
            let document = Document::empty(kind);
            let path = temp_file(&format!("empty-{kind:?}"));

            save_document(&path, &document).unwrap();
            let loaded = load_document(&path).unwrap();

            fs::remove_file(&path).ok();
            fs::remove_dir(&path.parent().unwrap()).ok();

            assert_eq!(loaded.kind(), kind);
            assert_eq!(loaded.len(), 1);
            assert!(loaded.is_valid());
        }
    }

    #[test]
    fn load_rejects_bad_magic() {
        let path = temp_file("bad-magic");

        fs::write(&path, b"this is not a weave file, but it is long enough to pass the size check")
            .unwrap();
        let result = load_document(&path);

        fs::remove_file(&path).ok();
        fs::remove_dir(&path.parent().unwrap()).ok();

        assert!(result.is_err());
    }

    #[test]
    fn load_rejects_unknown_version() {
        let path = temp_file("unknown-version");

        let mut bytes = FORMAT_IDENTIFIER.to_vec();
        bytes.extend_from_slice(&99_u64.to_le_bytes());
        bytes.extend_from_slice(&[0_u8; 16]);
        fs::write(&path, bytes).unwrap();

        let result = load_document(&path);

        fs::remove_file(&path).ok();
        fs::remove_dir(&path.parent().unwrap()).ok();

        let error = result.err().unwrap();
        assert!(error.contains("unsupported format version 99"), "unexpected error: {error}");
    }
}
