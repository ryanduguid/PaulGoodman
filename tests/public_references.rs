// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Opt-in references over independently identified official EPUBs. No network.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use tax_radar_au::fadden::{Run, extract};

#[test]
#[ignore = "requires the 2 hash-pinned official EPUBs in TAX_PUBLIC_EPUB_DIR"]
fn official_compilations_keep_section_and_version_identity() {
    let references: Value =
        serde_json::from_str(include_str!("fixtures/public-references.json")).unwrap();
    let supplied =
        PathBuf::from(std::env::var_os("TAX_PUBLIC_EPUB_DIR").expect("set TAX_PUBLIC_EPUB_DIR"));
    let mut provision_text = Vec::new();
    let labels = regex::Regex::new(r"(?m)^\s*(?:[-*]\s*)?(\([123abcd]\))").unwrap();
    for version in references["versions"].as_array().unwrap() {
        let bytes = fs::read(supplied.join(version["filename"].as_str().unwrap())).unwrap();
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(digest, version["sha256"]);
        let root = tempfile::tempdir().unwrap();
        let build = root.path().join("build");
        fs::create_dir(&build).unwrap();
        fs::create_dir(root.path().join("epub")).unwrap();
        fs::write(root.path().join("epub/C2004A05138.epub"), bytes).unwrap();
        fs::write(
            build.join("manifest_raw.json"),
            json!([{
                "id":"C2004A05138", "name":"Income Tax Assessment Act 1997",
                "epub":"C2004A05138.epub", "collection":"Act",
                "versionStart":version["start"], "compilationNumber":version["compilation"],
                "sourceUrl":version["source_url"],
                "version_is_current":version["current_on_2026_11_01"],
                "current_version_start":"2026-10-01"
            }])
            .to_string(),
        )
        .unwrap();
        extract::run(
            &build.to_string_lossy(),
            Some("2026-11-01"),
            &mut Run::default(),
        )
        .unwrap();
        let rows =
            fs::read_to_string(root.path().join("markdown/C2004A05138/sections.jsonl")).unwrap();
        let candidates: Vec<Value> = rows
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| {
                row["section"]
                    .as_str()
                    .is_some_and(|s| s.replace('\u{2011}', "-") == "8-1")
            })
            .collect();
        assert_eq!(
            candidates.len(),
            1,
            "one independently identified s8-1 per compilation"
        );
        let row = &candidates[0];
        assert_eq!(row["register_id"], "C2004A05138");
        assert_eq!(row["compilation_number"], version["compilation"]);
        assert_eq!(row["compilation_date"], version["start"]);
        assert_eq!(row["version_is_current"], version["current_on_2026_11_01"]);
        assert!(
            row["heading"]
                .as_str()
                .unwrap()
                .contains("General deductions")
        );
        let text = row["text"].as_str().unwrap();
        let observed: Vec<&str> = labels
            .captures_iter(text)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert_eq!(
            observed,
            [
                "(1)", "(a)", "(b)", "(2)", "(a)", "(b)", "(c)", "(d)", "(3)"
            ]
        );
        assert!(text.contains("private or domestic"));
        // Each clause's wording, in order, so a dropped clause body fails even when its label survives.
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut from = 0;
        for phrase in references["clause_phrases"].as_array().unwrap() {
            let phrase = phrase.as_str().unwrap();
            let at = flat[from..]
                .find(phrase)
                .unwrap_or_else(|| panic!("s 8-1 lacks {phrase:?} after byte {from}"));
            from += at + phrase.len();
        }
        assert!(!text.contains("nonexistentquartzterm"));
        assert_eq!(row["source_url"], version["source_url"]);
        provision_text.push(text.to_owned());
    }
    assert_eq!(
        provision_text[0], provision_text[1],
        "independent HTML reference identifies an unchanged provision across these versions"
    );
}
