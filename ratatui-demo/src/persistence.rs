//! Versioned `.uweave` persistence compatible with the eframe demo.

use std::{fs, path::Path};

use rkyv::{
    rancor::{self, Strategy},
    util::AlignedVec,
};
use universal_weave::versioning::VersionedBytes;

use crate::content::{DemoWeave, IndependentDemoWeave};
use crate::document::Document;

const FORMAT_IDENTIFIER: [u8; 24] = *b"UNIVERSAL-WEAVE-DEMO\0\0\0\0";
const VERSION_DEPENDENT: u64 = 1;
const VERSION_INDEPENDENT: u64 = 2;
const VERSION_DEPENDENT_LORO: u64 = 3;

pub fn save_document(path: &Path, document: &Document) -> Result<(), String> {
    let (version, data) = match document {
        Document::Dependent(weave) => (
            VERSION_DEPENDENT,
            rkyv::to_bytes::<rancor::Error>(weave.as_weave())
                .map_err(|error| format!("serialization failed: {error}"))?
                .into_vec(),
        ),
        Document::Independent(weave) => (
            VERSION_INDEPENDENT,
            rkyv::to_bytes::<rancor::Error>(weave.as_weave())
                .map_err(|error| format!("serialization failed: {error}"))?
                .into_vec(),
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
        .map_err(|error| format!("header write failed: {error}"))?;

    fs::write(path, output.as_slice()).map_err(|error| format!("cannot write file: {error}"))
}

pub fn load_document(path: &Path) -> Result<Document, String> {
    let bytes = fs::read(path).map_err(|error| format!("cannot read file: {error}"))?;
    let mut aligned: AlignedVec = AlignedVec::with_capacity(bytes.len());
    aligned.extend_from_slice(&bytes);

    let versioned = VersionedBytes::try_from_bytes(aligned.as_slice(), FORMAT_IDENTIFIER)
        .ok_or_else(|| "not a universal-weave demo file (bad magic bytes)".to_owned())?;

    match versioned.version {
        VERSION_DEPENDENT => rkyv::from_bytes::<DemoWeave, rancor::Error>(versioned.data)
            .map(Document::new_dependent)
            .map_err(|error| format!("file failed validation: {error}")),
        VERSION_INDEPENDENT => {
            rkyv::from_bytes::<IndependentDemoWeave, rancor::Error>(versioned.data)
                .map(Document::new_independent)
                .map_err(|error| format!("file failed validation: {error}"))
        }
        VERSION_DEPENDENT_LORO => {
            Err("Loro v3 documents are not supported by this terminal demo".to_owned())
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

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "universal-weave-ratatui-{name}-{}.uweave",
            std::process::id()
        ))
    }

    #[test]
    fn both_document_kinds_roundtrip() {
        for (name, document) in [
            ("dependent", seeded_dependent()),
            ("independent", seeded_independent()),
        ] {
            let path = temp_path(&format!("roundtrip-{name}"));
            save_document(&path, &document).unwrap();
            let loaded = load_document(&path).unwrap();
            fs::remove_file(path).ok();
            assert_eq!(loaded.kind(), document.kind());
            assert_eq!(loaded.len(), document.len());
            assert!(loaded.is_valid());
        }
    }

    #[test]
    fn empty_documents_roundtrip() {
        for kind in [WeaveKind::Dependent, WeaveKind::Independent] {
            let path = temp_path(&format!("empty-{}", kind.short_label()));
            save_document(&path, &Document::empty(kind)).unwrap();
            let loaded = load_document(&path).unwrap();
            fs::remove_file(path).ok();
            assert_eq!(loaded.kind(), kind);
            assert!(loaded.is_valid());
        }
    }

    #[test]
    fn bad_magic_and_unsupported_versions_are_rejected() {
        let path = temp_path("bad-magic");
        fs::write(
            &path,
            b"not a weave file but deliberately longer than the header",
        )
        .unwrap();
        assert!(load_document(&path).err().unwrap().contains("bad magic"));

        let mut bytes = FORMAT_IDENTIFIER.to_vec();
        bytes.extend_from_slice(&VERSION_DEPENDENT_LORO.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        fs::write(&path, bytes).unwrap();
        assert!(load_document(&path).err().unwrap().contains("Loro v3"));
        fs::remove_file(path).ok();
    }
}
