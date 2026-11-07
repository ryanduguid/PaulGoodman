// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! One fabricated consumer case, run end to end over the shipped search demo: the
//! checks a consumer of this builder runs on a draft before relying on it, in the order
//! docs/comparison.md lists its cases. The ruling is fabricated; its run directory is
//! written as the `rulings` stage writes one.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::rulings::NOTICE;

const SECTION_3: &str = "C2099A00001:0003:3";
const NAME: &str = "Sample Consumption Tax Act 2099";
const HELD: &str = "A seller is liable for sample consumption tax";
const ALTERED: &str = "A buyer is liable for sample consumption tax";

fn fadden(cwd: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.is_empty() || out.status.code() != Some(0), "{err}");
    (out.status.code().unwrap(), text)
}

fn json_lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A rulings run with one fabricated ruling paragraph citing section 3 by its Act.
fn write_run(dir: &Path) -> PathBuf {
    let run = dir.join("rulings-run");
    std::fs::create_dir(&run).unwrap();
    let row = json!({
        "docid": "TXR/TR20991/NAT/ATO/00001", "family": "Taxation Ruling",
        "title": "TR 2099/1 Sample consumption tax: sellers", "paragraph": "5",
        "numbering": "document", "heading": "Ruling",
        "text": format!("Section 3 of the {NAME} makes the seller liable."),
        "source_url": "https://example.invalid/TR20991", "source_sha256": "0".repeat(64),
        "fetched_on": "2099-08-02", "licence_basis": "document-notice",
        "withdrawal_status": "UNKNOWN", "withdrawal_notice": "",
    });
    std::fs::write(run.join("rulings.jsonl"), format!("{row}\n")).unwrap();
    let notice = format!("# ATO Legal Database material\n\n> {NOTICE}\n");
    std::fs::write(run.join("LICENCE-NOTICE.md"), notice).unwrap();
    run
}

fn draft(dir: &Path, name: &str, text: &str) -> String {
    std::fs::write(dir.join(name), text).unwrap();
    name.to_string()
}

#[test]
fn a_draft_is_checked_end_to_end_against_one_build() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let demo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("search-demo");
    let demo = demo.to_string_lossy();
    let run = write_run(cwd);
    let run = run.to_string_lossy();
    let build = [
        "search",
        "build",
        &demo,
        "--out",
        "index.sqlite",
        "--rulings",
        &run,
    ];
    assert_eq!(fadden(cwd, &build).0, 0);

    // Case 1: a quotation absent from the cited row is rejected, however plausible the
    // citation, by cite-check and by quote alike.
    let altered = draft(
        cwd,
        "altered.md",
        &format!("Section 3 of the {NAME} says \"{ALTERED}\".\n"),
    );
    let (code, out) = fadden(
        cwd,
        &["search", "cite-check", "index.sqlite", &altered, "--json"],
    );
    let citation = &json_lines(&out)[0];
    assert_eq!((code, citation["outcome"].as_str()), (1, Some("resolved")));
    assert_eq!(citation["quotations"][0]["grade"], "partial");
    let (code, out) = fadden(
        cwd,
        &[
            "search",
            "quote",
            "index.sqlite",
            SECTION_3,
            ALTERED,
            "--json",
        ],
    );
    assert_eq!(
        (code, json_lines(&out)[0]["grade"].as_str()),
        (1, Some("partial"))
    );
    let (code, out) = fadden(
        cwd,
        &["search", "quote", "index.sqlite", SECTION_3, HELD, "--json"],
    );
    assert_eq!(
        (code, json_lines(&out)[0]["grade"].as_str()),
        (0, Some("exact"))
    );

    // Case 2: a quotation the row holds, on a date before its compilation. The grade and
    // the date's status are kept apart, and the date alone fails the check.
    let held = draft(
        cwd,
        "held.md",
        &format!("Section 3 of the {NAME} says \"{HELD}\".\n"),
    );
    let dated = |date: &str| {
        let (code, out) = fadden(
            cwd,
            &[
                "search",
                "cite-check",
                "index.sqlite",
                &held,
                "--as-at",
                date,
                "--json",
            ],
        );
        let item = json_lines(&out).remove(0);
        let grade = item["quotations"][0]["grade"].as_str().unwrap().to_string();
        let status = item["as_at"]["status"].as_str().unwrap().to_string();
        (code, grade, status)
    };
    assert_eq!(
        dated("2099-06-30"),
        (1, "exact".into(), "before_compilation".into())
    );
    assert_eq!(dated("2099-07-15"), (0, "exact".into(), "captured".into()));

    // Case 4: the ruling that cites section 3 carries its source and capture date, and
    // no compilation status.
    let (code, out) = fadden(
        cwd,
        &["search", "refs", "index.sqlite", SECTION_3, "--json"],
    );
    assert_eq!(code, 0);
    let rulings = &json_lines(&out)[0]["cited_by_rulings"];
    assert_eq!(rulings.as_array().unwrap().len(), 1);
    let ruling = &rulings[0];
    assert_eq!(ruling["title"], "TR 2099/1 Sample consumption tax: sellers");
    assert_eq!(ruling["source_url"], "https://example.invalid/TR20991");
    assert_eq!(ruling["fetched_on"], "2099-08-02");
    assert!(ruling.get("compilation_date").is_none() && ruling.get("version_is_current").is_none());

    // A section the build does not hold names the held sections either side.
    let missing = draft(
        cwd,
        "missing.md",
        &format!("Section 4 of the {NAME} applies.\n"),
    );
    let (code, out) = fadden(
        cwd,
        &["search", "cite-check", "index.sqlite", &missing, "--json"],
    );
    let item = &json_lines(&out)[0];
    assert_eq!((code, item["outcome"].as_str()), (1, Some("not_held")));
    assert_eq!(item["nearest"], json!(["3"]));

    // The gate's closed list from the same build: the draft's authorities in the form
    // the gate reads are each on it. The gate itself runs in llm-tax-guardrails.
    let jsonl = format!("{run}/rulings.jsonl");
    let (code, _) = fadden(
        cwd,
        &[
            "authorities",
            "index.sqlite",
            "--out",
            "authorities.json",
            "--rulings",
            &jsonl,
        ],
    );
    assert_eq!(code, 0);
    let listed: Value =
        serde_json::from_slice(&std::fs::read(cwd.join("authorities.json")).unwrap()).unwrap();
    assert_eq!(
        listed,
        json!({"authorities": [
            "TR 2099/1",
            format!("s 1 of the {NAME}"),
            format!("s 2 of the {NAME}"),
            format!("s 3 of the {NAME}"),
        ]})
    );
}
