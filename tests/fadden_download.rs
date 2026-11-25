// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden download`: ported from `tests/corpus/test_download_paths.py`,
//! `test_security_remediation.py` (`ArchiveBudgetTests`, resumed transfers, budget
//! limits, envelope pre-checks), `test_regressions.py` (`ArchiveLifecycleTests`,
//! `DownloadManifestWriteTests`, `DownloadValidationTests`),
//! `test_audit_regressions.py` (`test_cached_download_preserves_authorisation_and_retrieval`),
//! and `test_output_security.py` (download paths/dates).
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};

use serde_json::{Value, json};
use tax_radar_au::fadden::Run;
use tax_radar_au::fadden::download::{
    self, MAX_DOWNLOAD_BYTES, MAX_EPUB_BYTES, Snapshot, decode_base64_strict, decode_envelope,
    diagnostic, discard_snapshots, fetch, rollback_snapshots, snapshot_paths, sniff,
};
use tax_radar_au::fadden::epub;
use tax_radar_au::fadden::http::{Failure, Http, Request, Transfer, net_download};
use tax_radar_au::pyzlib::crc32;

const RID: &str = "C2099A00001";
const NAME: &[u8] = b"document_1.xhtml";

fn b64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn zip_bytes(members: &[(&str, u16, &[u8], &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for &(name, method, data, plain) in members {
        let name = name.as_bytes();
        let sizes = [crc32(plain, 0), data.len() as u32, plain.len() as u32];
        let offset = out.len() as u32;
        out.extend(b"PK\x03\x04");
        for v in [20u16, 0, method, 0, 33] {
            out.extend(v.to_le_bytes());
        }
        sizes.iter().for_each(|v| out.extend(v.to_le_bytes()));
        for v in [name.len() as u16, 0] {
            out.extend(v.to_le_bytes());
        }
        out.extend(name);
        out.extend(data);
        central.extend(b"PK\x01\x02");
        for v in [20u16, 20, 0, method, 0, 33] {
            central.extend(v.to_le_bytes());
        }
        sizes.iter().for_each(|v| central.extend(v.to_le_bytes()));
        for v in [name.len() as u16, 0, 0, 0, 0] {
            central.extend(v.to_le_bytes());
        }
        for v in [0u32, offset] {
            central.extend(v.to_le_bytes());
        }
        central.extend(name);
    }
    let (start, size, count) = (out.len() as u32, central.len() as u32, members.len() as u16);
    out.extend(central);
    out.extend(b"PK\x05\x06");
    for v in [0u16, 0, count, count] {
        out.extend(v.to_le_bytes());
    }
    for v in [size, start] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out
}

fn epub_bytes(bodies: &[&str]) -> Vec<u8> {
    let names: Vec<String> = (1..=bodies.len())
        .map(|i| format!("document_{i}.xhtml"))
        .collect();
    let entries: Vec<(&str, u16, &[u8], &[u8])> = names
        .iter()
        .zip(bodies.iter())
        .map(|(n, b)| (n.as_str(), 0, b.as_bytes(), b.as_bytes()))
        .collect();
    zip_bytes(&entries)
}

type DownloadFn = Box<dyn FnMut(&str, &str, u64, Option<u64>, bool) -> Result<Transfer, Failure>>;

#[derive(Default)]
struct DownloadScript {
    actions: Vec<DownloadFn>,
    events: Vec<String>,
    now: String,
}

impl Http for DownloadScript {
    fn get(
        &mut self,
        url: &str,
        _limit: Option<usize>,
        _request: &mut Request,
    ) -> Result<tax_radar_au::fadden::http::Response, Failure> {
        Err(Failure::Transport(format!("unexpected get: {url}")))
    }

    fn download(
        &mut self,
        url: &str,
        part: &str,
        max_bytes: u64,
        range_start: Option<u64>,
        https_only: bool,
        _request: &mut Request,
    ) -> Result<Transfer, Failure> {
        self.events.push(format!("GET {url} range={range_start:?}"));
        if self.actions.is_empty() {
            return Err(Failure::Transport("URLError".into()));
        }
        let mut act = self.actions.remove(0);
        act(url, part, max_bytes, range_start, https_only)
    }

    fn sleep(&mut self, seconds: f64) {
        self.events.push(format!("sleep {seconds}"));
    }

    fn today(&mut self) -> String {
        "2099-02-01".into()
    }

    fn utc_now(&mut self) -> String {
        if self.now.is_empty() {
            "2099-01-01T00:00:00Z".into()
        } else {
            self.now.clone()
        }
    }
}

struct Checkout {
    _dir: tempfile::TempDir,
    scratch: PathBuf,
    epub: PathBuf,
}

fn checkout() -> Checkout {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("proj");
    let scratch = proj.join("fadden");
    let epub = proj.join("corpus").join("epub");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&epub).unwrap();
    std::fs::write(proj.join("pyproject.toml"), "x\n").unwrap();
    std::fs::write(proj.join("VERSION"), "x\n").unwrap();
    Checkout {
        _dir: dir,
        scratch,
        epub,
    }
}

#[test]
fn empty_and_ordinary_epubs_are_admitted() {
    let empty = zip_bytes(&[]);
    assert!(epub::valid_zip_bytes(empty));
    let raw = epub_bytes(&["<p class=\"ActHead4\">1 Title</p><p>Body</p>"]);
    assert!(epub::valid_zip_bytes(raw.clone()));
    let parsed = epub::from_bytes(raw).unwrap();
    assert_eq!(
        parsed.read("document_1.xhtml").unwrap(),
        b"<p class=\"ActHead4\">1 Title</p><p>Body</p>"
    );
}

#[test]
fn hostile_zip_directories_and_members_are_refused() {
    let raw = epub_bytes(&["one", "two"]);
    let dir_pos = raw.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let eocd = raw.len() - 22;

    let mut bad_sig = raw.clone();
    bad_sig[dir_pos + 3] = 0x09;
    let err = epub::from_bytes(bad_sig).err().unwrap().to_string();
    assert!(err.contains("invalid ZIP directory member"), "{err}");

    let mut overlong = raw.clone();
    overlong[dir_pos + 28..dir_pos + 30].copy_from_slice(&999u16.to_le_bytes());
    let err = epub::from_bytes(overlong).err().unwrap().to_string();
    assert!(err.contains("invalid ZIP directory extent"), "{err}");

    let mut overcounted = raw.clone();
    overcounted[eocd + 8..eocd + 10].copy_from_slice(&3u16.to_le_bytes());
    overcounted[eocd + 10..eocd + 12].copy_from_slice(&3u16.to_le_bytes());
    let err = epub::from_bytes(overcounted).err().unwrap().to_string();
    assert!(err.contains("member count is inconsistent"), "{err}");

    let second = dir_pos
        + 1
        + raw[dir_pos + 1..]
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .unwrap();
    let mut shared = raw.clone();
    shared[second + 42..second + 46].copy_from_slice(&0u32.to_le_bytes());
    let err = epub::from_bytes(shared).err().unwrap().to_string();
    assert!(err.contains("share a local header"), "{err}");

    let dup = zip_bytes(&[
        ("document_1.xhtml", 0, b"", b""),
        ("document_1.xhtml", 0, b"<p>diff</p>", b"<p>diff</p>"),
    ]);
    let err = epub::from_bytes(dup).err().unwrap().to_string();
    assert!(err.contains("duplicate"), "{err}");

    // Stored member length mismatch and truncated data.
    let single = zip_bytes(&[("document_1.xhtml", 0, b"abcde", b"abcde")]);
    let mut mismatch = single.clone();
    let dpos = mismatch
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .unwrap();
    mismatch[dpos + 20..dpos + 24].copy_from_slice(&4u32.to_le_bytes());
    let parsed = epub::from_bytes(mismatch).unwrap();
    let err = parsed.read("document_1.xhtml").err().unwrap().to_string();
    assert!(
        err.contains("stored member has inconsistent lengths"),
        "{err}"
    );

    let mut unsupported = zip_bytes(&[("document_1.xhtml", 12, b"body", b"body")]);
    let err = epub::from_bytes(std::mem::take(&mut unsupported))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("unsupported compression"), "{err}");

    let mut encrypted = zip_bytes(&[("document_1.xhtml", 0, b"body", b"body")]);
    encrypted[6] |= 1;
    let cd = encrypted
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .unwrap();
    encrypted[cd + 8] |= 1;
    assert!(!epub::valid_zip_bytes(encrypted));

    // Local header invalid signature and corrupted filename.
    let mut bad_local = single.clone();
    bad_local[3] = 0x09;
    assert!(!epub::valid_zip_bytes(bad_local));

    let mut renamed = single;
    renamed[30] = b'D';
    assert!(!epub::valid_zip_bytes(renamed));
    let _ = NAME;
}

#[test]
fn sniff_and_envelope_validation_enforce_limits_and_strict_base64() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("response");
    let pstr = path.to_string_lossy();
    assert_eq!(sniff(&pstr), None);

    for (label, body) in [
        ("not json", &b"{not json"[..]),
        ("a list", &b"[1]"[..]),
        ("null bytes", br#"{"bytes":null}"#),
        ("int bytes", br#"{"bytes":17}"#),
        ("empty bytes", br#"{"bytes":""}"#),
        ("bad base64", br#"{"bytes":"%%%NOT_BASE64%%%"}"#),
        ("non-ascii", "{\"bytes\":\"A\u{00e9}==\"}".as_bytes()),
    ] {
        std::fs::write(&path, body).unwrap();
        assert_eq!(
            decode_envelope(&pstr),
            download::Envelope::Invalid,
            "{label}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), body);
    }

    assert!(decode_base64_strict("AB==").is_some());
    assert!(decode_base64_strict("AA=A").is_none());
    assert!(decode_base64_strict("AA\nAA").is_none());
    assert!(decode_base64_strict("AAAA=").is_none());

    let valid_zip = epub_bytes(&["<p>Act</p>"]);
    let env = json!({
        "bytes": b64(&valid_zip),
        "registerId": "C2026C00001",
        "isAuthorised": true,
    });
    std::fs::write(&path, env.to_string()).unwrap();
    match decode_envelope(&pstr) {
        download::Envelope::Valid(meta) => {
            assert_eq!(
                meta.get("registerId").unwrap().as_str(),
                Some("C2026C00001")
            );
            assert_eq!(
                meta.get("isAuthorised"),
                Some(&tax_radar_au::pyjson::Json::Bool(true))
            );
        }
        other => panic!("expected valid envelope: {other:?}"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), valid_zip);
    assert_eq!(sniff(&pstr), Some(b'P'));
}

#[test]
fn diagnostic_collapses_control_chars_and_whitespace() {
    assert_eq!(
        diagnostic("403|text/html\r\nX:\tval\x00", 80),
        "403|text/html X: val"
    );
    assert_eq!(diagnostic("\x00\x01  \n", 80), "?");
    assert_eq!(diagnostic("abcdef", 3), "abc");
}

#[test]
fn snapshot_and_rollback_edge_cases_preserve_or_restore_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("a.epub");
    let second = dir.path().join("a.epub.meta.json");
    std::fs::write(&first, "epub").unwrap();
    std::fs::write(&second, "{}").unwrap();
    let fstr = first.to_string_lossy().into_owned();
    let sstr = second.to_string_lossy().into_owned();

    // Unfinished rollback blocks a fresh snapshot.
    std::fs::write(format!("{fstr}.rollback"), "earlier").unwrap();
    let err = snapshot_paths(&[&fstr]).unwrap_err().to_string();
    assert!(err.contains("unfinished rollback file"), "{err}");
    std::fs::remove_file(format!("{fstr}.rollback")).unwrap();

    // Missing backup stops rollback.
    let snap = snapshot_paths(&[&fstr]).unwrap();
    std::fs::remove_file(format!("{fstr}.rollback")).unwrap();
    let err = rollback_snapshots(&[snap]).unwrap_err().to_string();
    assert!(err.contains("missing rollback file"), "{err}");

    // New pair written stays intact across rollback (resumable first build),
    // while partial new pair deletes incomplete file.
    let new_a = dir.path().join("new_a.epub");
    let new_b = dir.path().join("new_a.epub.meta.json");
    let nas = new_a.to_string_lossy().into_owned();
    let nbs = new_b.to_string_lossy().into_owned();
    let s1 = snapshot_paths(&[&nas, &nbs]).unwrap();
    std::fs::write(&new_a, "valid-epub").unwrap();
    std::fs::write(&new_b, "{\"ok\":true}").unwrap();

    let inc_a = dir.path().join("inc_a.epub");
    let inc_b = dir.path().join("inc_a.epub.meta.json");
    let ias = inc_a.to_string_lossy().into_owned();
    let ibs = inc_b.to_string_lossy().into_owned();
    let s2 = snapshot_paths(&[&ias, &ibs]).unwrap();
    std::fs::write(&inc_a, "orphan-epub").unwrap();
    std::fs::write(format!("{ias}.part"), "leftover").unwrap();

    rollback_snapshots(&[s1, s2]).unwrap();
    assert!(new_a.exists() && new_b.exists());
    assert!(!inc_a.exists() && !Path::new(&format!("{ias}.part")).exists());

    // discard_snapshots deletes backup files and reports nothing missing.
    let s3: Snapshot = snapshot_paths(&[&fstr, &sstr]).unwrap();
    assert!(Path::new(&format!("{fstr}.rollback")).exists());
    assert_eq!(discard_snapshots(&[s3]), Vec::<String>::new());
    assert!(!Path::new(&format!("{fstr}.rollback")).exists());
}

#[test]
fn fetch_validates_http_status_budgets_resumes_and_envelopes() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("title.epub");
    let part = dir.path().join("title.epub.part");
    let dst_s = dst.to_string_lossy().into_owned();
    std::fs::write(&part, b"left by earlier run").unwrap();

    let payload = epub_bytes(&["<p>hello</p>"]);
    let mut http = DownloadScript::default();
    let pclone = payload.clone();
    http.actions.push(Box::new(move |_, part, _, _, _| {
        assert!(!Path::new(part).exists());
        std::fs::write(part, &pclone).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let got = fetch(
        &mut http,
        "https://example.test/epub",
        &dst_s,
        1,
        MAX_DOWNLOAD_BYTES,
    )
    .unwrap();
    assert_eq!(got.code, "200");
    assert_eq!(got.size as usize, payload.len());
    assert_eq!(std::fs::read(&dst).unwrap(), payload);

    // Missing response body is refused.
    let mut http = DownloadScript::default();
    http.actions.push(Box::new(|_, _, _, _, _| {
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let dst2 = dir.path().join("missing.epub");
    let err = fetch(
        &mut http,
        "https://example.test/epub",
        &dst2.to_string_lossy(),
        1,
        MAX_DOWNLOAD_BYTES,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("missing response body after 1 attempt "),
        "{err}"
    );
    assert!(!dst2.with_extension("epub.part").exists());

    // Oversized body stops cleanly.
    let mut http = DownloadScript::default();
    http.actions.push(Box::new(|_, part, _, _, _| {
        std::fs::write(part, b"PK\x03\x04-too-long").unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let dst3 = dir.path().join("over.epub");
    let err = fetch(
        &mut http,
        "https://example.test/epub",
        &dst3.to_string_lossy(),
        1,
        4,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("download exceeds the response budget"),
        "{err}"
    );

    // Resume with 0 remaining budget stops without issuing a second transfer.
    let mut http = DownloadScript::default();
    http.actions.push(Box::new(|_, part, _, _, _| {
        std::fs::write(part, b"PK\x03\x04").unwrap();
        Ok(Transfer {
            status: 500,
            content_type: "text/html".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let err = fetch(
        &mut http,
        "https://example.test/epub",
        &dst3.to_string_lossy(),
        2,
        4,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("download exceeds the response budget after 2 attempts"),
        "{err}"
    );
    assert_eq!(
        http.events.iter().filter(|e| e.starts_with("GET")).count(),
        1
    );
    assert!(!Path::new(&format!("{}.part", dst3.display())).exists());

    // Interrupted transfer resumes with remaining budget and succeeds.
    let split = payload.len() / 2;
    let first_half = payload[..split].to_vec();
    let second_half = payload[split..].to_vec();
    let dst4 = dir.path().join("resumed.epub");
    let mut http = DownloadScript::default();
    http.actions
        .push(Box::new(move |_, part, max_bytes, range, _| {
            assert_eq!(max_bytes, MAX_DOWNLOAD_BYTES);
            assert_eq!(range, None);
            std::fs::write(part, &first_half).unwrap();
            Ok(Transfer {
                status: 200,
                content_type: "application/epub+zip".into(),
                exceeds_budget: false,
                transport_error: Some("IncompleteRead".into()),
            })
        }));
    http.actions
        .push(Box::new(move |_, part, max_bytes, range, _| {
            assert_eq!(range, Some(split as u64));
            assert_eq!(max_bytes, MAX_DOWNLOAD_BYTES - split as u64);
            let mut f = std::fs::OpenOptions::new().append(true).open(part).unwrap();
            f.write_all(&second_half).unwrap();
            Ok(Transfer {
                status: 206,
                content_type: "application/epub+zip".into(),
                exceeds_budget: false,
                transport_error: None,
            })
        }));
    let got = fetch(
        &mut http,
        "https://example.test/epub",
        &dst4.to_string_lossy(),
        2,
        MAX_DOWNLOAD_BYTES,
    )
    .unwrap();
    assert_eq!(got.size as usize, payload.len());
    assert_eq!(std::fs::read(&dst4).unwrap(), payload);

    // Failed envelope attempt does not taint a subsequent raw EPUB retry.
    let bad_env = json!({"bytes": b64(b"not a ZIP"), "registerId": "STALE", "isAuthorised": true})
        .to_string();
    let raw_ok = payload.clone();
    let dst5 = dir.path().join("untainted.epub");
    let mut http = DownloadScript::default();
    http.actions.push(Box::new(move |_, part, _, _, _| {
        std::fs::write(part, &bad_env).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/json".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    http.actions.push(Box::new(move |_, part, _, _, _| {
        std::fs::write(part, &raw_ok).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let got = fetch(
        &mut http,
        "https://example.test/epub",
        &dst5.to_string_lossy(),
        2,
        MAX_DOWNLOAD_BYTES,
    )
    .unwrap();
    assert!(got.meta.is_none());
}

#[test]
fn main_loop_caches_authorised_metadata_handles_no_epub_and_rolls_back_upgrades() {
    let co = checkout();
    let payload = epub_bytes(&["<p>Body</p>"]);
    let env_body = json!({
        "bytes": b64(&payload),
        "registerId": "F2026C00001",
        "isAuthorised": true,
    })
    .to_string();
    std::fs::write(
        co.scratch.join("acts_resolved.json"),
        json!([
            {
                "id": "F2020L01498",
                "name": "Authorised Instrument",
                "versionStart": "2026-03-01",
                "compilationNumber": "1",
                "compilationRegisterId": "F2026C00001"
            },
            {
                "id": "F2021L00002",
                "name": "No Doc Instrument",
                "versionStart": "2026-03-02",
                "compilationNumber": "2",
                "compilationRegisterId": null
            }
        ])
        .to_string(),
    )
    .unwrap();
    std::fs::write(co.epub.join("F2021L00002.epub.part"), b"stale").unwrap();

    let mut http = DownloadScript {
        now: "2099-03-01T12:00:00Z".into(),
        ..Default::default()
    };
    http.actions.push(Box::new(move |_, part, _, _, _| {
        std::fs::write(part, &env_body).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/json".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let mut run = Run::default();
    download::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap();

    assert!(!co.epub.join("F2021L00002.epub.part").exists());
    let manifest_raw: Value = serde_json::from_str(
        &std::fs::read_to_string(co.scratch.join("manifest_raw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest_raw[0]["status"], "ok");
    assert_eq!(manifest_raw[0]["isAuthorised"], true);
    assert_eq!(manifest_raw[0]["fetched_at"], "2099-03-01T12:00:00Z");
    assert_eq!(manifest_raw[1]["status"], "no_epub");
    assert_eq!(manifest_raw[1]["reason"], "current_version_has_no_document");

    // Second run hits cache without fetching and preserves isAuthorised and fetched_at.
    std::fs::write(
        co.scratch.join("acts_resolved.json"),
        json!([{
            "id": "F2020L01498",
            "name": "Authorised Instrument",
            "versionStart": "2026-03-01",
            "compilationNumber": "1",
            "compilationRegisterId": "F2026C00001"
        }])
        .to_string(),
    )
    .unwrap();
    let mut cached_http = DownloadScript::default();
    let mut run2 = Run::default();
    download::run(
        &mut cached_http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run2,
    )
    .unwrap();
    assert!(cached_http.events.is_empty());
    let manifest_cached: Value = serde_json::from_str(
        &std::fs::read_to_string(co.scratch.join("manifest_raw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest_cached[0]["status"], "cached");
    assert_eq!(manifest_cached[0]["isAuthorised"], true);
    assert_eq!(manifest_cached[0]["fetched_at"], "2099-03-01T12:00:00Z");

    // Failed upgrade restores prior EPUB, sidecar, and manifest, and logs bounded diagnostics only.
    let prior_epub = std::fs::read(co.epub.join("F2020L01498.epub")).unwrap();
    let prior_side = std::fs::read_to_string(co.epub.join("F2020L01498.epub.meta.json")).unwrap();
    let prior_manifest = std::fs::read_to_string(co.scratch.join("manifest_raw.json")).unwrap();
    std::fs::write(
        co.scratch.join("acts_resolved.json"),
        json!([
            {
                "id": "F2020L01498",
                "name": "Upgraded Instrument",
                "versionStart": "2026-04-01",
                "compilationNumber": "2",
                "compilationRegisterId": "F2026C00002"
            },
            {
                "id": "F2022L00003",
                "name": "Blocked Instrument",
                "versionStart": "2026-04-02",
                "compilationNumber": "1",
                "compilationRegisterId": "F2026C00003"
            }
        ])
        .to_string(),
    )
    .unwrap();
    let new_zip = epub_bytes(&["<p>Upgrade</p>"]);
    let mut upgrade_http = DownloadScript::default();
    upgrade_http.actions.push(Box::new(move |_, part, _, _, _| {
        std::fs::write(part, &new_zip).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    for _ in 0..3 {
        upgrade_http.actions.push(Box::new(|_, part, _, _, _| {
            std::fs::write(part, b"<html>SECRET_BODY_MUST_NOT_LOG</html>").unwrap();
            Ok(Transfer {
                status: 403,
                content_type: "text/html\r\nX-Untrusted: ok".into(),
                exceeds_budget: false,
                transport_error: None,
            })
        }));
    }
    let mut run3 = Run::default();
    let err = download::run(
        &mut upgrade_http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run3,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("HTTP 403"), "{err}");
    assert_eq!(
        std::fs::read(co.epub.join("F2020L01498.epub")).unwrap(),
        prior_epub
    );
    assert_eq!(
        std::fs::read_to_string(co.epub.join("F2020L01498.epub.meta.json")).unwrap(),
        prior_side
    );
    assert_eq!(
        std::fs::read_to_string(co.scratch.join("manifest_raw.json")).unwrap(),
        prior_manifest
    );
    assert!(!co.epub.join("F2022L00003.epub").exists());
    let log = std::fs::read_to_string(co.scratch.join("download_log.txt")).unwrap();
    assert!(!log.contains("SECRET_BODY_MUST_NOT_LOG"), "{log}");
    assert!(
        log.contains("ROLLBACK restored 2 changed title(s)"),
        "{log}"
    );
}

#[test]
fn missing_compilation_id_bad_dates_and_foreign_tmp_are_safe() {
    let co = checkout();
    let foreign = co.epub.join("C2099A00001.epub.meta.json.tmp");
    std::fs::write(&foreign, b"unowned").unwrap();
    std::fs::write(
        co.scratch.join("acts_resolved.json"),
        json!([{
            "id": RID,
            "name": "Unpublished",
            "versionStart": "2099-01-01",
            "compilationRegisterId": null,
        }])
        .to_string(),
    )
    .unwrap();
    let mut http = DownloadScript::default();
    let mut run = Run::default();
    download::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap();
    assert_eq!(std::fs::read(&foreign).unwrap(), b"unowned");

    std::fs::write(
        co.scratch.join("acts_resolved.json"),
        json!([{
            "id": RID,
            "name": "Missing field",
            "versionStart": "2099-01-01",
        }])
        .to_string(),
    )
    .unwrap();
    let mut run = Run::default();
    let err = download::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("missing compilationRegisterId"), "{err}");

    std::fs::write(
        co.scratch.join("acts_resolved.json"),
        json!([{
            "id": RID,
            "name": "Bad Date",
            "versionStart": "2099-01-01/../../unexpected",
            "compilationRegisterId": "C2099C00001"
        }])
        .to_string(),
    )
    .unwrap();
    let mut http = DownloadScript::default();
    let mut run = Run::default();
    let err = download::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid ISO calendar date"), "{err}");
    assert!(http.events.is_empty());
}

#[test]
fn loopback_net_download_handles_range_resume_budget_and_https_only_policy() {
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("net.epub.part");
    let pstr = part.to_string_lossy().into_owned();

    // Non-HTTPS URL must be refused when https_only=true.
    let err = net_download("http://127.0.0.1:9/x", &pstr, 1024, None, true).unwrap_err();
    assert_eq!(err, Failure::Transport("HTTPSRequired".into()));

    // Loopback server testing Range: bytes=... resume & 206 append & non-HTTPS redirect block.
    let payload = b"PK\x03\x040123456789ABCDEF".to_vec();
    let split = 8usize;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let p_clone = payload.clone();
    let handle = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut req_buf = [0u8; 2048];
            let n = stream.read(&mut req_buf).unwrap_or(0);
            let req_text = String::from_utf8_lossy(&req_buf[..n]);
            if req_text.starts_with("GET /redirect-http ") {
                let _ = stream.write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/bad\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            } else if req_text.to_ascii_lowercase().contains("range: bytes=8-") {
                let tail = &p_clone[split..];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/epub+zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    tail.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(tail);
            } else if req_text.starts_with("GET /truncated ") {
                // Advertise full length, send only first 8 bytes and close socket.
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    p_clone.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&p_clone[..split]);
            } else {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
                );
                let _ = stream.write_all(&[b'x'; 100]);
            }
        }
    });

    // Attempt 1: truncated body writes 8 bytes and reports IncompleteRead.
    let t1 = net_download(
        &format!("http://{addr}/truncated"),
        &pstr,
        1024,
        None,
        false,
    )
    .unwrap();
    assert_eq!(t1.status, 200);
    assert_eq!(t1.transport_error.as_deref(), Some("IncompleteRead"));
    assert_eq!(std::fs::read(&part).unwrap(), &payload[..split]);

    // Attempt 2: Range: bytes=8- appends the rest via 206 Partial Content.
    let t2 = net_download(
        &format!("http://{addr}/resume"),
        &pstr,
        1024 - split as u64,
        Some(split as u64),
        false,
    )
    .unwrap();
    assert_eq!(t2.status, 206);
    assert_eq!(t2.transport_error, None);
    assert_eq!(std::fs::read(&part).unwrap(), payload);

    // Budget exceeded cap stops reading cleanly without buffering all bytes.
    let t3 = net_download(&format!("http://{addr}/big"), &pstr, 10, None, false).unwrap();
    assert!(t3.exceeds_budget);
    assert_eq!(std::fs::metadata(&part).unwrap().len(), 11);

    stop.store(true, Ordering::Relaxed);
    let _ = std::net::TcpStream::connect(addr);
    handle.join().unwrap();
    let _ = MAX_EPUB_BYTES;
}

#[cfg(feature = "fake-http")]
#[test]
fn cli_binary_downloads_and_logs_over_fake_http() {
    let co = checkout();
    let payload = epub_bytes(&["<p>Fake HTTP EPUB</p>"]);
    let body: String = payload.iter().map(|&b| char::from(b)).collect();
    std::fs::write(
        co.scratch.join("acts_resolved.json"),
        json!([{
            "id": RID,
            "name": "Fake HTTP Act",
            "versionStart": "2099-01-01",
            "compilationNumber": "1",
            "compilationRegisterId": "C2099C00001",
        }])
        .to_string(),
    )
    .unwrap();
    let script = co.scratch.join("script.json");
    let log = co.scratch.join("http.log");
    std::fs::write(
        &script,
        json!({
            "today": "2099-02-01",
            "now": "2099-02-01T01:02:03Z",
            "responses": [{
                "status": 200,
                "type": "application/epub+zip",
                "body": body,
            }]
        })
        .to_string(),
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("download")
        .env("FADDEN_MODULE_DIR", &co.scratch)
        .env("FADDEN_FAKE_HTTP", &script)
        .env("FADDEN_FAKE_HTTP_LOG", &log)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(co.epub.join(format!("{RID}.epub"))).unwrap(),
        payload
    );
    let http_log = std::fs::read_to_string(&log).unwrap();
    assert!(
        http_log.contains("GET https://www.legislation.gov.au/C2099A00001/2099-01-01/2099-01-01/text/original/epub"),
        "{http_log}"
    );
    assert!(http_log.contains("sleep 10"), "{http_log}");
}

#[test]
fn loopback_net_download_rejects_negative_chunk_size_and_handles_u64_max_budget() {
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("chunk.epub.part");
    let pstr = part.to_string_lossy().into_owned();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut req_buf = [0u8; 2048];
            let n = stream.read(&mut req_buf).unwrap_or(0);
            let req_text = String::from_utf8_lossy(&req_buf[..n]);
            if req_text.starts_with("GET /neg-chunk ") {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n-3\r\nhello\r\n0\r\n\r\n",
                );
            } else if req_text.starts_with("GET /max-chunked ") {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
                );
            } else if req_text.starts_with("GET /max-len ") {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                );
            }
        }
    });

    let neg = net_download(
        &format!("http://{addr}/neg-chunk"),
        &pstr,
        1024,
        None,
        false,
    )
    .unwrap();
    assert_eq!(neg.transport_error.as_deref(), Some("IncompleteRead"));
    assert!(!neg.exceeds_budget);
    assert!(std::fs::read(&part).unwrap().is_empty());

    let max_chunked = net_download(
        &format!("http://{addr}/max-chunked"),
        &pstr,
        u64::MAX,
        None,
        false,
    )
    .unwrap();
    assert_eq!(max_chunked.transport_error, None);
    assert!(!max_chunked.exceeds_budget);
    assert_eq!(std::fs::read(&part).unwrap(), b"hello");

    let max_len = net_download(
        &format!("http://{addr}/max-len"),
        &pstr,
        u64::MAX,
        None,
        false,
    )
    .unwrap();
    assert_eq!(max_len.transport_error, None);
    assert!(!max_len.exceeds_budget);
    assert_eq!(std::fs::read(&part).unwrap(), b"hello");

    stop.store(true, Ordering::Relaxed);
    let _ = std::net::TcpStream::connect(addr);
    handle.join().unwrap();
}

fn symlink_file(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, link);
    match made {
        Ok(()) => true,
        Err(error) if cfg!(windows) && error.raw_os_error() == Some(1314) => {
            eprintln!("skipped: symbolic links are unavailable to this account ({error})");
            false
        }
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn fetch_removes_dangling_part_symlink_before_and_after_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("dangling.epub");
    let part = dir.path().join("dangling.epub.part");
    let missing = dir.path().join("missing-target");
    if !symlink_file(&missing, &part) {
        return;
    }
    assert!(std::fs::symlink_metadata(&part).is_ok());
    assert!(!part.exists());

    let payload = epub_bytes(&["<p>Symlink safe</p>"]);
    let expected = payload.clone();
    let mut http = DownloadScript::default();
    http.actions.push(Box::new(move |_, part_path, _, _, _| {
        assert!(std::fs::symlink_metadata(part_path).is_err());
        std::fs::write(part_path, &payload).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let got = fetch(
        &mut http,
        "https://example.test/epub",
        &dst.to_string_lossy(),
        1,
        MAX_DOWNLOAD_BYTES,
    )
    .unwrap();
    assert_eq!(got.size as usize, expected.len());
    assert_eq!(std::fs::read(&dst).unwrap(), expected);
    assert!(!missing.exists());
    assert!(std::fs::symlink_metadata(&part).is_err());

    let mut failing = DownloadScript::default();
    let missing_after = missing.clone();
    failing.actions.push(Box::new(move |_, part_path, _, _, _| {
        assert!(symlink_file(&missing_after, Path::new(part_path)));
        Err(Failure::Transport("IncompleteRead".into()))
    }));
    let err = fetch(
        &mut failing,
        "https://example.test/epub",
        &dst.to_string_lossy(),
        1,
        MAX_DOWNLOAD_BYTES,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("IncompleteRead"), "{err}");
    assert!(std::fs::symlink_metadata(&part).is_err());
}
