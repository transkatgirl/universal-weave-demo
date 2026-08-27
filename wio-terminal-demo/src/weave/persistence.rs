use alloc::format;
use alloc::string::{String, ToString};

use rkyv::{api::high::to_bytes_in, rancor, util::AlignedVec};
use universal_weave::versioning::VersionedBytes;

use super::content::{DemoWeave, IndependentDemoWeave};
use super::document::Document;

pub const HEADER_BYTES: usize = 32;
pub const FORMAT_IDENTIFIER: [u8; 24] = *b"UNIVERSAL-WEAVE-DEMO\0\0\0\0";
pub const VERSION_DEPENDENT: u64 = 1;
pub const VERSION_INDEPENDENT: u64 = 2;
pub const VERSION_DEPENDENT_LORO: u64 = 3;

/// Encode the header and archive in one aligned allocation. The 32-byte
/// header preserves the archive's required 16-byte alignment.
pub fn encode_document(document: &Document) -> Result<AlignedVec, String> {
    let (version, dependent, independent) = match document {
        Document::Dependent(weave) => (VERSION_DEPENDENT, Some(weave.as_weave()), None),
        Document::Independent(weave) => (VERSION_INDEPENDENT, None, Some(weave.as_weave())),
    };
    let mut output = AlignedVec::with_capacity(HEADER_BYTES);
    output.extend_from_slice(&FORMAT_IDENTIFIER);
    output.extend_from_slice(&version.to_le_bytes());
    if let Some(weave) = dependent {
        to_bytes_in::<_, rancor::Error>(weave, output)
            .map_err(|error| format!("serialization failed: {error}"))
    } else {
        to_bytes_in::<_, rancor::Error>(independent.expect("document kind"), output)
            .map_err(|error| format!("serialization failed: {error}"))
    }
}

/// Decode a Universal Weave demo document from an aligned SD read buffer.
pub fn decode_document(bytes: &[u8]) -> Result<Document, String> {
    let versioned = VersionedBytes::try_from_bytes(bytes, FORMAT_IDENTIFIER)
        .ok_or_else(|| "not a Universal Weave demo file (bad or truncated header)".to_string())?;
    match versioned.version {
        VERSION_DEPENDENT => rkyv::from_bytes::<DemoWeave, rancor::Error>(versioned.data)
            .map(Document::new_dependent)
            .map_err(|error| format!("file failed validation: {error}")),
        VERSION_INDEPENDENT =>
            rkyv::from_bytes::<IndependentDemoWeave, rancor::Error>(versioned.data)
                .map(Document::new_independent)
                .map_err(|error| format!("file failed validation: {error}")),
        VERSION_DEPENDENT_LORO =>
            Err("Loro v3 documents are not supported by this firmware".to_string()),
        other => Err(format!(
            "unsupported format version {other} (expected {VERSION_DEPENDENT} or {VERSION_INDEPENDENT})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weave::document::{seeded_dependent, seeded_independent, WeaveKind};

    #[test]
    fn both_versions_round_trip_in_one_aligned_buffer() {
        for document in [seeded_dependent(), seeded_independent()] {
            let kind = document.kind();
            let encoded = encode_document(&document).unwrap();
            assert_eq!(&encoded[..24], &FORMAT_IDENTIFIER);
            assert_eq!(encoded.as_ptr() as usize % 16, 0);
            let decoded = decode_document(encoded.as_slice()).unwrap();
            assert_eq!(decoded.kind(), kind);
            assert_eq!(decoded.len(), document.len());
            assert!(decoded.is_valid());
        }
    }

    #[test]
    fn empty_documents_round_trip() {
        for kind in [WeaveKind::Dependent, WeaveKind::Independent] {
            let encoded = encode_document(&Document::empty(kind)).unwrap();
            let decoded = decode_document(encoded.as_slice()).unwrap();
            assert_eq!(decoded.kind(), kind);
            assert!(decoded.is_valid());
        }
    }

    #[test]
    fn desktop_reference_v1_fixture_is_compatible() {
        let hex = include_str!("../../tests/fixtures/reference_v1.hex");
        let digits: alloc::vec::Vec<u8> = hex
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        let mut bytes: AlignedVec = AlignedVec::with_capacity(digits.len() / 2);
        for pair in digits.chunks_exact(2) {
            let high = (pair[0] as char).to_digit(16).unwrap() as u8;
            let low = (pair[1] as char).to_digit(16).unwrap() as u8;
            bytes.push((high << 4) | low);
        }
        let document = decode_document(bytes.as_slice()).unwrap();
        assert_eq!(document.kind(), WeaveKind::Dependent);
        assert_eq!(document.len(), 5);
        assert_eq!(document.metadata(), "The Lighthouse Letter");
        assert!(document.is_valid());
    }

    #[test]
    fn malformed_unknown_and_loro_files_are_explained() {
        assert!(decode_document(b"").err().unwrap().contains("header"));
        assert!(decode_document(&[0; 32]).err().unwrap().contains("header"));

        let versioned = |version: u64| -> AlignedVec {
            let mut bytes = AlignedVec::new();
            bytes.extend_from_slice(&FORMAT_IDENTIFIER);
            bytes.extend_from_slice(&version.to_le_bytes());
            bytes
        };
        assert!(
            decode_document(versioned(VERSION_DEPENDENT_LORO).as_slice())
                .err()
                .unwrap()
                .contains("Loro v3")
        );
        assert!(decode_document(versioned(99).as_slice())
            .err()
            .unwrap()
            .contains("unsupported format version 99"));

        let mut invalid = versioned(VERSION_DEPENDENT);
        invalid.extend_from_slice(&[0; 16]);
        assert!(decode_document(invalid.as_slice())
            .err()
            .unwrap()
            .contains("validation"));
    }
}
