// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden quick`: ported from `tests/corpus/test_quick.py`, plus end-to-end
//! `--features fake-http` binary verification building an indexed 9-Act corpus
//! with earlier `--in-force-on` compilations.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Value, json};
use tax_radar_au::fadden::quick::{
    OMISSION_REASONS, PRESET, Runner, SMOKE_QUERY, SUPPORT, build, check_windows,
    main as quick_main, prepare, run_cli, validate_in_force_date,
};
use tax_radar_au::fadden::{Exc, Run};
use tax_radar_au::pypath::realpath;
#[cfg(feature = "fake-http")]
use tax_radar_au::pyzlib::crc32;

const HIT: &str = "1. Income Tax Assessment Act 1997: 8-1 General deductions\n";

type VersionMap = BTreeMap<Option<String>, Vec<(String, String, Option<String>)>>;
type CaptureFn = Box<dyn FnMut(&str, &[&str], bool) -> Result<String, Exc>>;

struct MockStages {
    ran: Vec<String>,
    calls: Vec<Vec<String>>,
    hit: String,
    statuses: Vec<&'static str>,
    indexed: Vec<String>,
    versions: VersionMap,
    unextracted: BTreeSet<String>,
    incomplete_report: bool,
    invalid_omissions: bool,
    omission_reason: String,
    epub_dir: String,
    epub_dirs: Vec<String>,
    capture_override: Option<CaptureFn>,
    printed: Vec<String>,
    elapsed: f64,
}

impl Default for MockStages {
    fn default() -> Self {
        Self {
            ran: Vec::new(),
            calls: Vec::new(),
            hit: HIT.into(),
            statuses: vec!["ok"],
            indexed: PRESET.iter().map(|s| (*s).to_string()).collect(),
            versions: BTreeMap::new(),
            unextracted: BTreeSet::new(),
            incomplete_report: false,
            invalid_omissions: false,
            omission_reason: "no version was in force then".into(),
            epub_dir: String::new(),
            epub_dirs: Vec::new(),
            capture_override: None,
            printed: Vec::new(),
            elapsed: 0.0,
        }
    }
}

impl MockStages {
    fn write_index(&self, path: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute("CREATE TABLE rows (register_id TEXT)", [])
            .unwrap();
        for rid in &self.indexed {
            conn.execute("INSERT INTO rows VALUES (?1)", [rid]).unwrap();
        }
    }
}

impl Runner for MockStages {
    fn stage(&mut self, scratch: &str, args: &[&str], capture: bool) -> Result<String, Exc> {
        if let Some(mut custom) = self.capture_override.take() {
            let res = custom(scratch, args, capture);
            self.capture_override = Some(custom);
            if capture {
                return res;
            }
        }
        let label = if args[0] == "search" {
            format!("{} {}", args[0], args[1])
        } else {
            args[0].to_string()
        };
        self.ran.push(label);
        self.calls
            .push(args.iter().map(|s| (*s).to_string()).collect());
        let scratch_p = Path::new(scratch);
        if args[0] == "versions" {
            let on = if args.get(1) == Some(&"--in-force-on") {
                Some(args[2].to_string())
            } else {
                None
            };
            let rows: Vec<Value> = self
                .versions
                .get(&on)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(rid, start, end)| {
                    json!({
                        "id": rid,
                        "versionStart": start,
                        "current_version_start": end,
                        "compilationRegisterId": "C2024C00001"
                    })
                })
                .collect();
            if let Some(ref date) = on {
                let resolved_ids: BTreeSet<String> = rows
                    .iter()
                    .filter_map(|r| r["id"].as_str().map(str::to_string))
                    .collect();
                let current: Vec<Value> = rows
                    .iter()
                    .filter(|r| r["current_version_start"].is_null())
                    .cloned()
                    .collect();
                let resolved: Vec<Value> = rows
                    .iter()
                    .filter(|r| !r["current_version_start"].is_null())
                    .cloned()
                    .collect();
                let mut skipped: Vec<Value> = PRESET
                    .iter()
                    .filter(|rid| !resolved_ids.contains(**rid))
                    .map(|rid| json!({"register_id": rid, "reason": self.omission_reason}))
                    .collect();
                let omissions = if self.invalid_omissions {
                    json!({})
                } else {
                    if self.incomplete_report && !skipped.is_empty() {
                        skipped.remove(0);
                    }
                    Value::Array(skipped)
                };
                let report = json!({
                    "in_force_on": date,
                    "current": current,
                    "resolved": resolved,
                    "omissions": omissions,
                });
                std::fs::write(scratch_p.join("dated_resolution.json"), report.to_string())
                    .unwrap();
            } else {
                std::fs::write(
                    scratch_p.join("acts_resolved.json"),
                    Value::Array(rows).to_string(),
                )
                .unwrap();
            }
        }
        if args[0] == "extract" {
            let rows: Vec<Value> = serde_json::from_str(
                &std::fs::read_to_string(scratch_p.join("acts_resolved.json")).unwrap(),
            )
            .unwrap();
            let kept: Vec<Value> = rows
                .into_iter()
                .filter(|r| {
                    r["id"]
                        .as_str()
                        .is_none_or(|id| !self.unextracted.contains(id))
                })
                .collect();
            std::fs::write(
                scratch_p.join("manifest_md.json"),
                Value::Array(kept).to_string(),
            )
            .unwrap();
        }
        if args[0] == "finalize" {
            std::fs::write(scratch_p.parent().unwrap().join("sources.json"), "{}").unwrap();
        }
        if args.get(..2) == Some(&["search", "build"][..]) {
            self.write_index(args[4]);
        }
        Ok(if capture {
            self.hit.clone()
        } else {
            String::new()
        })
    }

    fn download(&mut self, scratch: &str, epub_dir: &str) -> Result<(), Exc> {
        self.ran.push("download".into());
        let rows: Vec<Value> = self.statuses.iter().map(|s| json!({"status": s})).collect();
        std::fs::write(
            Path::new(scratch).join("manifest_raw.json"),
            Value::Array(rows).to_string(),
        )
        .unwrap();
        self.epub_dir = epub_dir.to_string();
        self.epub_dirs.push(epub_dir.to_string());
        Ok(())
    }

    fn retry13(&mut self, _scratch: &str, _epub_dir: &str) -> Result<(), Exc> {
        self.ran.push("retry13".into());
        Ok(())
    }

    fn monotonic(&mut self) -> f64 {
        self.elapsed
    }

    fn print(&mut self, line: &str) {
        self.printed.push(line.to_string());
    }
}

struct ModuleDir {
    _dir: tempfile::TempDir,
    module: PathBuf,
    base: PathBuf,
    out: PathBuf,
}

fn setup() -> ModuleDir {
    let dir = tempfile::tempdir().unwrap();
    // `quick` prints the real path of `--out`; Windows runners hand out 8.3 temp names.
    let root = PathBuf::from(realpath(&dir.path().to_string_lossy()).unwrap());
    let repo_fadden = Path::new(env!("CARGO_MANIFEST_DIR")).join("fadden");
    let module = root.join("module");
    std::fs::create_dir(&module).unwrap();
    for name in [
        "titles_principal.json",
        "pii_contact_allowlist.json",
        "check_current.py",
        "corpus_paths.py",
        "http_fetch.py",
    ] {
        std::fs::copy(repo_fadden.join(name), module.join(name)).unwrap();
    }
    let base = root.join("work");
    std::fs::create_dir(&base).unwrap();
    let out = base.join("law");
    ModuleDir {
        _dir: dir,
        module,
        base,
        out,
    }
}

fn staging(out: &Path) -> PathBuf {
    out.parent().unwrap().join(format!(
        "{}.building",
        out.file_name().unwrap().to_string_lossy()
    ))
}

#[test]
fn preparing_lays_out_preset_and_support_and_refuses_dirty_targets() {
    let s = setup();
    let stg = prepare(&s.module.to_string_lossy(), &s.out.to_string_lossy()).unwrap();
    assert_eq!(PathBuf::from(&stg), staging(&s.out));
    assert!(!s.out.exists());
    let scratch = Path::new(&stg).join("build");
    let titles: Value =
        serde_json::from_str(&std::fs::read_to_string(scratch.join("titles_all.json")).unwrap())
            .unwrap();
    let ids: BTreeSet<String> = titles
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        PRESET
            .iter()
            .map(|x| (*x).to_string())
            .collect::<BTreeSet<_>>()
    );
    for name in SUPPORT {
        assert!(scratch.join(name).is_file(), "{name}");
    }

    // Earlier partial build refuses and stays intact.
    let err = prepare(&s.module.to_string_lossy(), &s.out.to_string_lossy())
        .unwrap_err()
        .to_string();
    assert!(err.contains("earlier partial build"), "{err}");
    std::fs::remove_dir_all(&stg).unwrap();

    // Non-empty out is refused.
    std::fs::create_dir(&s.out).unwrap();
    std::fs::write(s.out.join("held.txt"), "x").unwrap();
    let err = prepare(&s.module.to_string_lossy(), &s.out.to_string_lossy())
        .unwrap_err()
        .to_string();
    assert!(err.contains("not empty"), "{err}");
    assert!(!staging(&s.out).exists());

    // Absent parent fails naturally.
    assert!(
        prepare(
            &s.module.to_string_lossy(),
            &s.base.join("absent").join("law").to_string_lossy()
        )
        .is_err()
    );

    // Incomplete titles_principal.json stops prepare.
    std::fs::write(
        s.module.join("titles_principal.json"),
        json!([{"id": PRESET[0], "isPrincipal": true}]).to_string(),
    )
    .unwrap();
    let out2 = s.base.join("law2");
    let err = prepare(&s.module.to_string_lossy(), &out2.to_string_lossy())
        .unwrap_err()
        .to_string();
    assert!(err.contains("every preset Act"), "{err}");
}

#[test]
fn stages_run_in_pipeline_order_with_preset_flag_and_recovery_route() {
    let s = setup();
    std::fs::create_dir(&s.out).unwrap();
    let mut runner = MockStages {
        statuses: vec!["ok", "no_epub"],
        ..Default::default()
    };
    let hit = build(
        &mut runner,
        &s.module.to_string_lossy(),
        &s.out.to_string_lossy(),
        &[],
    )
    .unwrap();
    assert_eq!(hit, HIT);
    assert!(s.out.join("sources.json").is_file());
    assert!(s.out.join("index.sqlite").is_file());
    assert!(!staging(&s.out).exists());
    assert_eq!(
        runner.ran,
        [
            "versions",
            "download",
            "probe13",
            "retry13",
            "extract",
            "pii_scan",
            "pii_scan2",
            "finalize",
            "search build",
            "search query"
        ]
    );
    assert!(
        runner
            .calls
            .contains(&vec!["finalize".into(), "--preset".into()])
    );
    assert_eq!(
        runner.epub_dir,
        staging(&s.out).join("epub").to_string_lossy()
    );
}

#[test]
fn late_failures_leave_output_empty_and_preserve_staging() {
    let s = setup();
    std::fs::create_dir(&s.out).unwrap();
    let mut runner = MockStages {
        hit: "0 rows.\n".into(),
        ..Default::default()
    };
    let mut run = Run::default();
    let code = run_cli(
        &mut runner,
        &s.module.to_string_lossy(),
        &["--out".into(), s.out.to_string_lossy().into_owned()],
        &mut run,
    )
    .unwrap();
    assert_eq!(code, 1);
    assert_eq!(std::fs::read_dir(&s.out).unwrap().count(), 0);
    assert!(staging(&s.out).join("sources.json").is_file());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("holds no row"), "{stderr}");
    assert!(stderr.contains(&staging(&s.out).to_string_lossy().into_owned()));

    // Missing preset row in index stops publish.
    let s2 = setup();
    let mut runner2 = MockStages {
        indexed: PRESET[..PRESET.len() - 1]
            .iter()
            .map(|x| (*x).to_string())
            .collect(),
        ..Default::default()
    };
    let mut run2 = Run::default();
    let code2 = run_cli(
        &mut runner2,
        &s2.module.to_string_lossy(),
        &["--out".into(), s2.out.to_string_lossy().into_owned()],
        &mut run2,
    )
    .unwrap();
    assert_eq!(code2, 1);
    assert!(!s2.out.exists());
    let err2 = String::from_utf8_lossy(&run2.stderr);
    assert!(
        err2.contains(&format!(
            "the index holds no rows for {}",
            PRESET[PRESET.len() - 1]
        )),
        "{err2}"
    );
}

#[test]
fn dated_compilations_reuse_held_versions_detect_overlaps_and_validate_dates() {
    let s = setup();
    let [itaa97, gst, itaa36] = [PRESET[0], PRESET[1], PRESET[2]];
    let mut versions = BTreeMap::new();
    versions.insert(
        None,
        vec![
            (itaa97.into(), "2026-10-01".into(), None),
            (gst.into(), "2024-01-01".into(), None),
        ],
    );
    versions.insert(
        Some("2024-07-01".into()),
        vec![
            (
                itaa97.into(),
                "2024-07-01".into(),
                Some("2025-08-01".into()),
            ),
            (gst.into(), "2024-01-01".into(), None),
        ],
    );
    versions.insert(
        Some("2025-07-01".into()),
        vec![
            (
                itaa97.into(),
                "2024-07-01".into(),
                Some("2025-08-01".into()),
            ),
            (
                itaa36.into(),
                "2025-03-01".into(),
                Some("2025-09-01".into()),
            ),
        ],
    );
    versions.insert(
        Some("2026-07-01".into()),
        vec![(gst.into(), "2024-01-01".into(), None)],
    );
    let mut runner = MockStages {
        versions,
        ..Default::default()
    };
    let mut out_run = Run::default();
    let code = run_cli(
        &mut runner,
        &s.module.to_string_lossy(),
        &[
            "--out".into(),
            s.out.to_string_lossy().into_owned(),
            "--in-force-on".into(),
            "2026-07-01".into(),
            "--in-force-on".into(),
            "2025-07-01".into(),
            "--in-force-on".into(),
            "2024-07-01".into(),
        ],
        &mut out_run,
    )
    .unwrap();
    assert_eq!(code, 0);
    let folder = s.out.join("compilations");
    let dirs: BTreeSet<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        dirs,
        ["2024-07-01", "2025-07-01"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    let sources: Value =
        serde_json::from_str(&std::fs::read_to_string(s.out.join("sources.json")).unwrap())
            .unwrap();
    assert_eq!(sources["historical_omissions"].as_array().unwrap().len(), 3);
    let stdout = String::from_utf8_lossy(&out_run.stdout);
    assert!(stdout.contains("Built and indexed 9 Acts in "), "{stdout}");
    assert!(
        stdout.contains(&format!("AUS_ACCOUNTING_CORPUS_ROOT={}", s.out.display())),
        "{stdout}"
    );
    assert!(
        runner.printed.iter().any(|l| l
            == "compilations/2026-07-01: 0 to fetch, 1 already held, 8 with none to fetch, listed above"),
        "{:?}",
        runner.printed
    );

    // Date validation error messages match Python argparse.
    for (bad, prefix) in [
        ("2025-7-01", "'2025-7-01' is not a date as YYYY-MM-DD"),
        ("20250315", "'20250315' is not a date as YYYY-MM-DD"),
        ("2025-02-30", "'2025-02-30' is not a date: day "),
        ("2025-13-01", "'2025-13-01' is not a date: month "),
        ("0000-01-01", "'0000-01-01' is not a date: year "),
    ] {
        let msg = validate_in_force_date(bad).unwrap_err();
        assert!(msg.starts_with(prefix), "{bad}: {msg}");
    }

    // Window overlap checks match dist_verify rules.
    for (windows, overlaps_expected) in [
        (
            vec![
                ("2026-07-01", None),
                ("2025-03-28", Some("2026-07-01")),
                ("2024-07-01", Some("2025-03-28")),
            ],
            false,
        ),
        (
            vec![("2026-07-01", None), ("2025-03-28", Some("2026-07-02"))],
            true,
        ),
        (
            vec![
                ("2025-01-01", Some("2025-06-01")),
                ("2025-01-01", Some("2025-03-01")),
            ],
            true,
        ),
    ] {
        let rows: Vec<tax_radar_au::pyjson::Json> = windows
            .into_iter()
            .map(|(start, end)| {
                let mut pairs = vec![
                    ("id", tax_radar_au::pyjson::Json::str("C1")),
                    ("versionStart", tax_radar_au::pyjson::Json::str(start)),
                ];
                if let Some(e) = end {
                    pairs.push(("current_version_start", tax_radar_au::pyjson::Json::str(e)));
                } else {
                    pairs.push(("current_version_start", tax_radar_au::pyjson::Json::Null));
                }
                tax_radar_au::pyjson::obj(pairs)
            })
            .collect();
        assert_eq!(check_windows(&rows).is_err(), overlaps_expected);
    }

    // Subprocess stage error formatting via CLI binary.
    let mut bad_run = Run::default();
    let res = quick_main(&["--bad-flag".into()], &mut bad_run);
    assert!(res.is_err());
    let _ = OMISSION_REASONS;
    let _ = SMOKE_QUERY;
}

#[test]
fn report_validation_refuses_incomplete_or_bad_omissions() {
    let s = setup();
    let mut runner = MockStages {
        incomplete_report: true,
        versions: BTreeMap::from([(
            Some("2025-07-01".into()),
            vec![(
                PRESET[0].into(),
                "2024-01-01".into(),
                Some("2026-01-01".into()),
            )],
        )]),
        ..Default::default()
    };
    let err = build(
        &mut runner,
        &s.module.to_string_lossy(),
        &s.out.to_string_lossy(),
        &["2025-07-01".into()],
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("incomplete title resolution report"), "{err}");

    let s2 = setup();
    let mut runner2 = MockStages {
        omission_reason: "Register unavailable".into(),
        ..Default::default()
    };
    let err2 = build(
        &mut runner2,
        &s2.module.to_string_lossy(),
        &s2.out.to_string_lossy(),
        &["2025-07-01".into()],
    )
    .unwrap_err()
    .to_string();
    assert!(err2.contains("invalid omission reason"), "{err2}");
}

#[cfg(feature = "fake-http")]
#[test]
fn cli_binary_builds_and_indexes_9_act_preset_with_probe13_and_dated_compilations() {
    let s = setup();
    let project = s.base.join("proj");
    let module = project.join("fadden");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(project.join("pyproject.toml"), "x\n").unwrap();
    std::fs::write(project.join("VERSION"), "x\n").unwrap();
    for entry in std::fs::read_dir(&s.module).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, module.join(path.file_name().unwrap())).unwrap();
    }

    // Order in which `versions` processes principal titles: sorted by `name`.
    let mut rows: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(module.join("titles_principal.json")).unwrap(),
    )
    .unwrap();
    rows.retain(|r| r["id"].as_str().is_some_and(|id| PRESET.contains(&id)));
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let ordered: Vec<String> = rows
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(ordered.len(), 9);

    let html = b"<html><body>\
        <p class=\"ActHead4\"><span class=\"CharSectno\">8-1</span> General deductions</p>\
        <p class=\"subsection\">General deductions apply to assessable income.</p>\
        </body></html>";
    let name = b"document_1.xhtml";
    let mut epub = Vec::new();
    let mut central = Vec::new();
    let sizes = [crc32(html, 0), html.len() as u32, html.len() as u32];
    epub.extend(b"PK\x03\x04");
    for v in [20u16, 0, 0, 0, 33] {
        epub.extend(v.to_le_bytes());
    }
    sizes.iter().for_each(|v| epub.extend(v.to_le_bytes()));
    for v in [name.len() as u16, 0] {
        epub.extend(v.to_le_bytes());
    }
    epub.extend(name);
    epub.extend(html);
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
    let (start, size) = (epub.len() as u32, central.len() as u32);
    epub.extend(central);
    epub.extend(b"PK\x05\x06");
    for v in [0u16, 0, 1, 1] {
        epub.extend(v.to_le_bytes());
    }
    for v in [size, start] {
        epub.extend(v.to_le_bytes());
    }
    epub.extend(0u16.to_le_bytes());
    let epub_str: String = epub.iter().map(|&b| char::from(b)).collect();

    let mut responses: Vec<Value> = Vec::new();
    // 1. versions probe
    responses.push(json!({
        "status": 200,
        "type": "application/json",
        "body": json!({"value": [{
            "titleId": "C2004A05138",
            "start": "2024-01-01T00:00:00Z",
            "compilationNumber": "1",
            "isCurrent": true
        }]}).to_string()
    }));
    // 2. versions batch 1 (first 8 titles): title 0 starts 2026-10-01 so 2024-07-01 fetches earlier; 1..8 start 2024-01-01.
    let batch1: Vec<Value> = ordered[..8]
        .iter()
        .enumerate()
        .map(|(idx, id)| {
            json!({
                "titleId": id,
                "start": if idx == 0 { "2026-10-01T00:00:00Z" } else { "2024-01-01T00:00:00Z" },
                "compilationNumber": "2",
                "registerId": format!("C2026C0000{}", idx + 1),
            })
        })
        .collect();
    responses.push(json!({
        "status": 200,
        "type": "application/json",
        "body": json!({"value": batch1}).to_string(),
    }));
    // 3. versions batch 2 (9th title): no current registerId -> triggers probe13 + retry13.
    let ninth = &ordered[8];
    responses.push(json!({
        "status": 200,
        "type": "application/json",
        "body": json!({"value": [{
            "titleId": ninth,
            "start": "2026-10-01T00:00:00Z",
            "compilationNumber": "2",
            "registerId": null,
        }]}).to_string(),
    }));
    // 4. download for first 8 titles
    for _ in 0..8 {
        responses.push(json!({
            "status": 200,
            "type": "application/epub+zip",
            "body": epub_str,
        }));
    }
    // 5. probe13 for 9th title
    responses.push(json!({
        "status": 200,
        "type": "application/json",
        "body": json!({"value": [
            {"titleId": ninth, "start": "2026-10-01T00:00:00Z", "end": null, "isCurrent": true, "compilationNumber": "2", "registerId": null},
            {"titleId": ninth, "start": "2025-01-01T00:00:00Z", "end": "2026-10-01T00:00:00Z", "isCurrent": false, "compilationNumber": "1", "registerId": "C2025C00009"},
        ]}).to_string(),
    }));
    // 6. retry13 EPUB for 9th title
    responses.push(json!({
        "status": 200,
        "type": "application/epub+zip",
        "body": epub_str,
    }));
    // 7. add_compilations dated versions queries (--in-force-on 2024-07-01 --report-omissions)
    for (idx, id) in ordered.iter().enumerate() {
        let val = if idx == 0 {
            json!([
                {"titleId": id, "start": "2024-01-01T00:00:00Z", "end": "2026-10-01T00:00:00Z", "isCurrent": false, "compilationNumber": "1", "registerId": "C2024C00001"}
            ])
        } else if idx < 8 {
            json!([
                {"titleId": id, "start": "2024-01-01T00:00:00Z", "end": null, "isCurrent": true, "compilationNumber": "2", "registerId": format!("C2026C0000{}", idx + 1)}
            ])
        } else {
            json!([])
        };
        responses.push(json!({
            "status": 200,
            "type": "application/json",
            "body": json!({"value": val}).to_string(),
        }));
    }
    // 8. dated EPUB download for ordered[0]
    responses.push(json!({
        "status": 200,
        "type": "application/epub+zip",
        "body": epub_str,
    }));

    let script = s.base.join("script.json");
    let log = s.base.join("http.log");
    std::fs::write(
        &script,
        json!({
            "today": "2026-11-01",
            "now": "2026-11-01T01:02:03Z",
            "responses": responses,
        })
        .to_string(),
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fadden"))
        .args([
            "quick",
            "--out",
            &s.out.to_string_lossy(),
            "--in-force-on",
            "2024-07-01",
        ])
        .env("FADDEN_MODULE_DIR", &module)
        .env("FADDEN_FAKE_HTTP", &script)
        .env("FADDEN_FAKE_HTTP_LOG", &log)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("Built and indexed 9 Acts in "), "{stdout}");
    assert!(
        stdout.contains(&format!("AUS_ACCOUNTING_CORPUS_ROOT={}", s.out.display())),
        "{stdout}"
    );
    assert!(s.out.join("index.sqlite").is_file());
    assert!(
        s.out
            .join("compilations")
            .join("2024-07-01")
            .join("build")
            .join("manifest_md.json")
            .is_file()
    );
}

#[test]
fn run_subprocess_reports_non_zero_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = tax_radar_au::fadden::Run::default();
    let err = tax_radar_au::fadden::quick::run_subprocess(
        env!("CARGO_BIN_EXE_fadden"),
        &dir.path().to_string_lossy(),
        &["--no-such-flag"],
        false,
        &mut r,
    )
    .unwrap_err();
    assert!(err.to_string().contains("exited"));
}
