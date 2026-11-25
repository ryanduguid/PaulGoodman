// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden/retry13.py`: recover titles whose current version carries no
//! published document by fetching the latest published compilation from
//! `probe13.json` and patching `manifest_raw.json` in place.
use std::collections::HashMap;

use super::corpus_paths::{
    child, compilation_id, corpus_root, module_dir, register_id, require_builder_layout,
    script_dir_in, version_date, write_json_atomic,
};
use super::download::{
    CRAWL_DELAY, MAX_DOWNLOAD_BYTES, dict_update, discard_snapshots, fetch, rollback_snapshots,
    snapshot_paths,
};
use super::http::{self, Http};
use super::{Exc, Run, getitem, head_str, iterate, json_error};
use crate::argparse::Exit;
use crate::pyjson::{self, Json, PyStr};
use crate::pypath::{self, PyPath};
use crate::pyvalue::py_str;

const OUTPUT_DEPTH: usize = 990;

fn truthy(value: Option<&Json>) -> bool {
    value.is_some_and(Json::truthy)
}

fn id_text(value: &Json) -> Result<String, Exc> {
    match value {
        Json::Str(PyStr::Text(s)) => Ok(s.clone()),
        _ => register_id(value),
    }
}

/// Run retry13 with explicit `Http`, `scratch`, `epub_dir` and `delay`.
pub fn run(
    http: &mut dyn Http,
    scratch: &str,
    epub_dir: &str,
    delay: f64,
    run: &mut Run,
) -> Result<(), Exc> {
    let probe_text = crate::pyio::read_text(&pypath::join(scratch, "probe13.json"))?;
    let probe_list = pyjson::loads_standard(&probe_text).map_err(json_error)?;
    let mut probe: HashMap<String, Json> = HashMap::new();
    for entry in iterate(&probe_list)? {
        let key = id_text(getitem(&entry, "id")?)?;
        probe.insert(key, entry);
    }

    let manifest_path = pypath::join(scratch, "manifest_raw.json");
    let manifest_text = crate::pyio::read_text(&manifest_path)?;
    let manifest_val = pyjson::loads_standard(&manifest_text).map_err(json_error)?;
    let mut manifest = iterate(&manifest_val)?;

    let mut todo = Vec::new();
    for a in &manifest {
        if truthy(a.get("epub")) {
            continue;
        }
        let id = getitem(a, "id")?;
        let Some(key) = id.as_str() else {
            continue;
        };
        if probe
            .get(key)
            .and_then(|p| p.get("latest_doc"))
            .is_some_and(Json::truthy)
        {
            todo.push(a.clone());
        }
    }
    run.print(&format!("retrying {} titles", todo.len()));

    let mut patches = Vec::new();
    let mut snapshots = Vec::new();
    let total = todo.len();

    let body = (|| -> Result<usize, Exc> {
        for (idx, a) in todo.iter().enumerate() {
            let i = idx + 1;
            let rid = register_id(getitem(a, "id")?)?;
            let v = probe
                .get(&rid)
                .and_then(|p| p.get("latest_doc"))
                .ok_or_else(|| Exc::Key("latest_doc".into()))?;
            let d = version_date(v.get("start"))?;
            compilation_id(v)?;
            let dst = child(epub_dir, &[&format!("{rid}.epub")])?;
            let side = child(epub_dir, &[&format!("{rid}.epub.meta.json")])?;
            let url = format!("https://www.legislation.gov.au/{rid}/{d}/{d}/text/original/epub");

            snapshots.push(snapshot_paths(&[&dst, &side])?);
            let fetched = fetch(http, &url, &dst, 3, MAX_DOWNLOAD_BYTES)?;
            let comp_reg_id = fetched
                .meta
                .as_ref()
                .and_then(|m| m.get("registerId"))
                .filter(|v| v.truthy())
                .or_else(|| v.get("registerId"))
                .cloned()
                .unwrap_or(Json::Null);
            let comp_num = v.get("compilationNumber").cloned().unwrap_or(Json::Null);
            let dst_name = PyPath::new(&dst).name().to_string();
            let current_start = version_date(a.get("versionStart"))?;

            let mut pairs = vec![
                ("id", Json::str(&rid)),
                ("sourceUrl", Json::str(&url)),
                ("versionStart", Json::str(&d)),
                ("compilationNumber", comp_num.clone()),
                ("compilationRegisterId", comp_reg_id.clone()),
                ("version_is_current", Json::Bool(false)),
                ("current_version_start", Json::str(&current_start)),
                ("current_version_has_document", Json::Bool(false)),
            ];
            if let Some(meta) = &fetched.meta {
                pairs.push((
                    "isAuthorised",
                    meta.get("isAuthorised").cloned().unwrap_or(Json::Null),
                ));
            }
            pairs.extend([
                ("epub", Json::str(&dst_name)),
                ("bytes", Json::Int(fetched.size.into())),
                ("status", Json::str("ok_superseded_version")),
            ]);
            let rec = crate::pyjson::obj(pairs);

            let sidecar = Json::obj([
                ("versionStart", Json::str(&d)),
                ("compilationNumber", comp_num),
                ("compilationRegisterId", comp_reg_id),
                ("bytes", Json::Int(fetched.size.into())),
            ]);
            write_json_atomic(&side, &sidecar, None, OUTPUT_DEPTH)?;
            patches.push(rec);

            let name = py_str(getitem(a, "name")?);
            run.print(&format!(
                "{i:2}/{total:2} {rid:<12} {:<22} {:9}  {}",
                "OK",
                fetched.size,
                head_str(&name, 50)
            ));
            http.sleep(delay);
        }

        let mut recovered: HashMap<String, Json> = HashMap::new();
        for p in &patches {
            if let Some(id) = p.get("id").and_then(Json::as_str) {
                recovered.insert(id.to_string(), p.clone());
            }
        }
        let mut merged = 0usize;
        for row in &mut manifest {
            if let Some(id) = row.get("id").and_then(Json::as_str)
                && let Some(patch) = recovered.get(id)
                && let Json::Object(pairs) = patch
            {
                let updates: Vec<(&str, Json)> = pairs
                    .iter()
                    .filter_map(|(k, v)| k.as_str().map(|ks| (ks, v.clone())))
                    .collect();
                *row = dict_update(row, updates);
                merged += 1;
            }
        }

        let patch_target = pypath::join(scratch, "retry13_patch.json");
        snapshots.push(snapshot_paths(&[&patch_target, &manifest_path])?);
        write_json_atomic(
            &patch_target,
            &Json::Array(patches.clone()),
            Some(1),
            OUTPUT_DEPTH,
        )?;
        if !recovered.is_empty() {
            write_json_atomic(
                &manifest_path,
                &Json::Array(manifest),
                Some(1),
                OUTPUT_DEPTH,
            )?;
        }
        Ok(merged)
    })();

    let merged = match body {
        Ok(merged) => merged,
        Err(err) => {
            if rollback_snapshots(&snapshots).is_err() {
                return Err(Exc::Runtime(
                    "retry failed and rollback was incomplete".into(),
                ));
            }
            return Err(err);
        }
    };

    let leftovers = discard_snapshots(&snapshots);
    if !leftovers.is_empty() {
        run.print(&format!(
            "WARNING retained {} rollback file(s) after commit",
            leftovers.len()
        ));
    }
    run.print(&format!("\nrecovered {} / {total}", patches.len()));
    if !patches.is_empty() {
        run.print(&format!(
            "patched {merged} entr{} in manifest_raw.json",
            if merged == 1 { "y" } else { "ies" }
        ));
    }
    Ok(())
}

/// CLI entrypoint (`python -m fadden retry13`).
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let outcome = (|| -> Result<(), Exc> {
        let scratch = module_dir();
        let directory = script_dir_in(&scratch, "retry13.py")?;
        require_builder_layout(&directory)?;
        let epub_dir = child(&corpus_root(&directory), &["epub"])?;
        let mut http = http::connect();
        self::run(http.as_mut(), &scratch, &epub_dir, CRAWL_DELAY, run)
    })();
    Ok(match outcome {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}
