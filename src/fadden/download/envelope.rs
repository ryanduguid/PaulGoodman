// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `decode_envelope` from `fadden/download.py`: the Register can answer an
//! EPUB request with a JSON envelope holding the archive as base64.
use super::{MAX_DOWNLOAD_BYTES, MAX_EPUB_BYTES};
use crate::pyjson::{self, Json, PyStr};

const ENVELOPE_KEYS: [&str; 7] = [
    "registerId",
    "fileName",
    "sizeInBytes",
    "isAuthorised",
    "compilationNumber",
    "mimeType",
    "extension",
];

/// The value of one base64 digit.
fn digit(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// `base64.b64decode(text, validate=True)` over an ASCII base64 string.
pub fn decode_base64_strict(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let (chunks, _) = bytes.as_chunks::<4>();
    let last_chunk = chunks.len() - 1;
    for (idx, chunk) in chunks.iter().enumerate() {
        let is_last = idx == last_chunk;
        let a = digit(chunk[0])?;
        let b = digit(chunk[1])?;
        if chunk[2] == b'=' {
            if !is_last || chunk[3] != b'=' {
                return None;
            }
            out.push((a << 2) | (b >> 4));
        } else if chunk[3] == b'=' {
            if !is_last {
                return None;
            }
            let c = digit(chunk[2])?;
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (c >> 2));
        } else {
            let c = digit(chunk[2])?;
            let d = digit(chunk[3])?;
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (c >> 2));
            out.push((c << 6) | d);
        }
    }
    Some(out)
}

/// Parsed JSON envelope result.
#[derive(Debug, Clone, PartialEq)]
pub enum Envelope {
    Valid(Json),
    Invalid,
}

/// The envelope document at `path`, if it is one within the size limits.
fn read_envelope(path: &str) -> Option<Json> {
    if std::fs::metadata(path).ok()?.len() > MAX_DOWNLOAD_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let doc = pyjson::loads_standard(&text).ok()?;
    matches!(doc, Json::Object(_)).then_some(doc)
}

/// Replace `path` with the decoded EPUB if it is a valid JSON envelope.
pub fn decode_envelope(path: &str) -> Envelope {
    let Some(doc) = read_envelope(path) else {
        return Envelope::Invalid;
    };
    let Some(Json::Str(PyStr::Text(b64))) = doc.get("bytes") else {
        return Envelope::Invalid;
    };
    let max_b64_len = 4 * ((MAX_EPUB_BYTES as usize).div_ceil(3));
    if b64.is_empty() || !b64.is_ascii() || b64.len() > max_b64_len {
        return Envelope::Invalid;
    }
    let Some(decoded) = decode_base64_strict(b64) else {
        return Envelope::Invalid;
    };
    if decoded.len() as u64 > MAX_EPUB_BYTES || std::fs::write(path, &decoded).is_err() {
        return Envelope::Invalid;
    }
    let pairs: Vec<(PyStr, Json)> = ENVELOPE_KEYS
        .iter()
        .map(|&k| {
            (
                PyStr::Text(k.to_string()),
                doc.get(k).cloned().unwrap_or(Json::Null),
            )
        })
        .collect();
    Envelope::Valid(Json::Object(pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_envelope_size_and_write_refusals() {
        assert_eq!(decode_base64_strict("//8="), Some(vec![0xff, 0xff]));
        assert_eq!(decode_base64_strict("AAA=AAAA"), None);

        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.epub");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_DOWNLOAD_BYTES + 1).unwrap();
        drop(f);
        assert_eq!(decode_envelope(&big.to_string_lossy()), Envelope::Invalid);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ro = dir.path().join("ro.epub");
            std::fs::write(&ro, r#"{"bytes":"//8="}"#).unwrap();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o444)).unwrap();
            assert_eq!(decode_envelope(&ro.to_string_lossy()), Envelope::Invalid);
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }
}
