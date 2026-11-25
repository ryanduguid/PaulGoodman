// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden retry13`: ported from `Retry13Tests` (`tests/corpus/test_pipeline_paths.py`),
//! `Retry13MergeTests` and `Retry13ManifestWriteTests` (`tests/corpus/test_regressions.py`),
//! and bad-date security tests (`tests/corpus/test_output_security.py`).
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tax_radar_au::fadden::http::{Failure, Http, Request, Transfer};
use tax_radar_au::fadden::{Run, retry13};
use tax_radar_au::pyzlib::crc32;

const RID: &str = "C2099A00001";

fn epub_bytes(html: &str) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    let name = b"document_1.xhtml";
    let plain = html.as_bytes();
    let sizes = [crc32(plain, 0), plain.len() as u32, plain.len() as u32];
    out.extend(b"PK\x03\x04");
    for v in [20u16, 0, 0, 0, 33] {
        out.extend(v.to_le_bytes());
    }
    sizes.iter().for_each(|v| out.extend(v.to_le_bytes()));
    for v in [name.len() as u16, 0] {
        out.extend(v.to_le_bytes());
    }
    out.extend(name);
    out.extend(plain);
    central.extend(b"PK\x01\x02");
    for v in [20u16, 20, 0, 0, 0, 33] {
        central.extend(v.to_le_bytes());
    }
    sizes.iter().for_each(|v| central.extend(v.to_le_bytes()));
    for v in [name.len() as u16, 0, 0, 0, 0] {
        central.extend(v.to_le_bytes());
    }
    for v in [0u32, 0] {
        central.extend(v.to_le_bytes());
    }
    central.extend(name);
    let (start, size) = (out.len() as u32, central.len() as u32);
    out.extend(central);
    out.extend(b"PK\x05\x06");
    for v in [0u16, 0, 1, 1] {
        out.extend(v.to_le_bytes());
    }
    for v in [size, start] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out
}

type DownloadAction = Box<dyn FnMut(&str, &str) -> Result<Transfer, Failure>>;

#[derive(Default)]
struct RetryHttp {
    actions: Vec<DownloadAction>,
    events: Vec<String>,
}

impl Http for RetryHttp {
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
        _max_bytes: u64,
        _range_start: Option<u64>,
        _https_only: bool,
        _request: &mut Request,
    ) -> Result<Transfer, Failure> {
        self.events.push(format!("GET {url}"));
        if self.actions.is_empty() {
            return Err(Failure::Transport("URLError".into()));
        }
        let mut act = self.actions.remove(0);
        act(url, part)
    }

    fn sleep(&mut self, seconds: f64) {
        self.events.push(format!("sleep {seconds}"));
    }

    fn today(&mut self) -> String {
        "2099-02-01".into()
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
fn nothing_to_retry_writes_only_the_empty_audit_record() {
    let co = checkout();
    let manifest = json!([{
        "id": RID,
        "name": "Synthetic Tax Act",
        "epub": null,
        "versionStart": "2099-03-01",
    }]);
    std::fs::write(co.scratch.join("manifest_raw.json"), manifest.to_string()).unwrap();
    std::fs::write(co.scratch.join("probe13.json"), "[]").unwrap();

    let mut http = RetryHttp::default();
    let mut run = Run::default();
    retry13::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap();
    let patch: Value = serde_json::from_str(
        &std::fs::read_to_string(co.scratch.join("retry13_patch.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(patch, json!([]));
    let after: Value = serde_json::from_str(
        &std::fs::read_to_string(co.scratch.join("manifest_raw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(after, manifest);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("recovered 0 / 0"), "{stdout}");
    assert!(!stdout.contains("patched"), "{stdout}");
}

#[test]
fn recoveries_are_merged_into_manifest_raw_in_place_and_leave_untouched_titles() {
    let co = checkout();
    let manifest = json!([
        {
            "id": "C2004A00001",
            "name": "Already Downloaded Act",
            "epub": "C2004A00001.epub",
            "bytes": 10,
            "status": "ok",
            "versionStart": "2026-01-01"
        },
        {
            "id": "F2020L01498",
            "name": "Recoverable Instrument",
            "epub": null,
            "bytes": 0,
            "status": "no_epub",
            "reason": "current_version_has_no_document",
            "versionStart": "2026-03-01"
        },
        {
            "id": "F2021L00002",
            "name": "Unrecoverable Instrument",
            "epub": null,
            "bytes": 0,
            "status": "no_epub",
            "reason": "current_version_has_no_document",
            "versionStart": "2026-04-01"
        }
    ]);
    std::fs::write(co.scratch.join("manifest_raw.json"), manifest.to_string()).unwrap();
    std::fs::write(
        co.scratch.join("probe13.json"),
        json!([{
            "id": "F2020L01498",
            "name": "Recoverable Instrument",
            "latest_doc": {
                "start": "2025-06-30T00:00:00",
                "compilationNumber": "4",
                "registerId": "F2025C00010"
            }
        }])
        .to_string(),
    )
    .unwrap();

    let payload = epub_bytes("<p>Recovered</p>");
    let p_len = payload.len();
    let mut http = RetryHttp::default();
    http.actions.push(Box::new(move |_, part| {
        std::fs::write(part, &payload).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    let mut run = Run::default();
    retry13::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap();

    let patches: Value = serde_json::from_str(
        &std::fs::read_to_string(co.scratch.join("retry13_patch.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(patches.as_array().unwrap().len(), 1);
    assert_eq!(patches[0]["id"], "F2020L01498");

    let merged: Value = serde_json::from_str(
        &std::fs::read_to_string(co.scratch.join("manifest_raw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(merged.as_array().unwrap().len(), 3);
    assert_eq!(merged[1]["epub"], "F2020L01498.epub");
    assert_eq!(merged[1]["bytes"], p_len);
    assert_eq!(merged[1]["status"], "ok_superseded_version");
    assert_eq!(merged[1]["version_is_current"], false);
    assert_eq!(merged[1]["current_version_start"], "2026-03-01");
    assert_eq!(merged[1]["current_version_has_document"], false);
    assert_eq!(merged[1]["name"], "Recoverable Instrument");
    assert_eq!(merged[2]["status"], "no_epub");
    assert_eq!(merged[2]["epub"], Value::Null);
    assert!(merged[2].get("version_is_current").is_none());
    assert_eq!(merged[0]["status"], "ok");

    let sidecar: Value = serde_json::from_str(
        &std::fs::read_to_string(co.epub.join("F2020L01498.epub.meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["versionStart"], "2025-06-30");
    assert_eq!(sidecar["compilationRegisterId"], "F2025C00010");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("patched 1 entry in manifest_raw.json"),
        "{stdout}"
    );
}

#[test]
fn a_later_retry_failure_restores_every_earlier_change() {
    let co = checkout();
    let ids = ["F2020L01498", "F2021L00002"];
    let manifest: Vec<Value> = ids
        .iter()
        .enumerate()
        .map(|(i, rid)| {
            json!({
                "id": rid,
                "name": format!("Recoverable Instrument {}", i + 1),
                "epub": null,
                "bytes": 0,
                "status": "no_epub",
                "versionStart": format!("2026-03-0{}", i + 1)
            })
        })
        .collect();
    let mpath = co.scratch.join("manifest_raw.json");
    let ppath = co.scratch.join("retry13_patch.json");
    let old_manifest = Value::Array(manifest).to_string();
    let old_patch = r#"[{"prior":true}]"#;
    std::fs::write(&mpath, &old_manifest).unwrap();
    std::fs::write(&ppath, old_patch).unwrap();
    std::fs::write(
        co.scratch.join("probe13.json"),
        Value::Array(
            ids.iter()
                .enumerate()
                .map(|(i, rid)| {
                    json!({
                        "id": rid,
                        "name": format!("Recoverable Instrument {}", i + 1),
                        "latest_doc": {
                            "start": "2025-06-30T00:00:00",
                            "compilationNumber": (i + 1).to_string(),
                            "registerId": format!("F2025C0000{}", i + 1)
                        }
                    })
                })
                .collect(),
        )
        .to_string(),
    )
    .unwrap();

    let first_epub = co.epub.join(format!("{}.epub", ids[0]));
    let first_side = co.epub.join(format!("{}.epub.meta.json", ids[0]));
    std::fs::write(&first_epub, b"prior EPUB").unwrap();
    std::fs::write(&first_side, r#"{"prior":true}"#).unwrap();

    let replacement = epub_bytes("<p>Replacement</p>");
    let mut http = RetryHttp::default();
    http.actions.push(Box::new(move |_, part| {
        std::fs::write(part, &replacement).unwrap();
        Ok(Transfer {
            status: 200,
            content_type: "application/epub+zip".into(),
            exceeds_budget: false,
            transport_error: None,
        })
    }));
    for _ in 0..3 {
        http.actions.push(Box::new(|_, part| {
            std::fs::write(part, b"blocked").unwrap();
            Ok(Transfer {
                status: 403,
                content_type: "text/html".into(),
                exceeds_budget: false,
                transport_error: None,
            })
        }));
    }
    let mut run = Run::default();
    let err = retry13::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("HTTP 403"), "{err}");

    assert_eq!(std::fs::read_to_string(&mpath).unwrap(), old_manifest);
    assert_eq!(std::fs::read_to_string(&ppath).unwrap(), old_patch);
    assert_eq!(std::fs::read(&first_epub).unwrap(), b"prior EPUB");
    assert_eq!(
        std::fs::read_to_string(&first_side).unwrap(),
        r#"{"prior":true}"#
    );
    assert!(!co.epub.join(format!("{}.epub", ids[1])).exists());
    assert!(!Path::new(&format!("{}.epub.part", co.epub.join(ids[1]).display())).exists());
    assert!(!co.epub.join(format!("{}.epub.rollback", ids[0])).exists());
}

#[test]
fn bad_retry13_date_never_reaches_fetch() {
    let co = checkout();
    std::fs::write(
        co.scratch.join("manifest_raw.json"),
        json!([{
            "id": RID,
            "name": "Synthetic",
            "epub": null,
            "versionStart": "2099-01-01/../../unexpected",
        }])
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        co.scratch.join("probe13.json"),
        json!([{
            "id": RID,
            "latest_doc": {
                "start": "2099-01-01/../../unexpected",
                "registerId": "C2099C00001"
            }
        }])
        .to_string(),
    )
    .unwrap();
    let mut http = RetryHttp::default();
    let mut run = Run::default();
    let err = retry13::run(
        &mut http,
        &co.scratch.to_string_lossy(),
        &co.epub.to_string_lossy(),
        0.0,
        &mut run,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid Register version start"), "{err}");
    assert!(http.events.is_empty());
}

#[cfg(feature = "fake-http")]
#[test]
fn cli_binary_retries_and_patches_manifest_over_fake_http() {
    let co = checkout();
    let payload = epub_bytes("<p>Recovered via CLI</p>");
    let body: String = payload.iter().map(|&b| char::from(b)).collect();
    std::fs::write(
        co.scratch.join("manifest_raw.json"),
        json!([{
            "id": RID,
            "name": "Synthetic Tax Act",
            "epub": null,
            "bytes": 0,
            "status": "no_epub",
            "versionStart": "2099-03-01",
        }])
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        co.scratch.join("probe13.json"),
        json!([{
            "id": RID,
            "latest_doc": {
                "start": "2099-01-01T00:00:00",
                "compilationNumber": "2",
                "registerId": "C2099C00002",
            }
        }])
        .to_string(),
    )
    .unwrap();
    let script = co.scratch.join("script.json");
    let log = co.scratch.join("http.log");
    std::fs::write(
        &script,
        json!({
            "today": "2099-04-01",
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
        .arg("retry13")
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
    let patched: Value = serde_json::from_str(
        &std::fs::read_to_string(co.scratch.join("manifest_raw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(patched[0]["status"], "ok_superseded_version");
}
