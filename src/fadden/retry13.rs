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
    CRAWL_DELAY, Fetched, MAX_DOWNLOAD_BYTES, Snapshot, dict_update, discard_snapshots, fetch,
    rollback_snapshots, snapshot_paths,
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

/// `probe13.json` keyed by title id.
fn load_probe(scratch: &str) -> Result<HashMap<String, Json>, Exc> {
    let probe_text = crate::pyio::read_text(&pypath::join(scratch, "probe13.json"))?;
    let probe_list = pyjson::loads_standard(&probe_text).map_err(json_error)?;
    let mut probe = HashMap::new();
    for entry in iterate(&probe_list)? {
        let key = id_text(getitem(&entry, "id")?)?;
        probe.insert(key, entry);
    }
    Ok(probe)
}

/// Manifest rows with no EPUB whose title has a published compilation in the probe.
fn retryable(manifest: &[Json], probe: &HashMap<String, Json>) -> Result<Vec<Json>, Exc> {
    let mut todo = Vec::new();
    for a in manifest {
        if truthy(a.get("epub")) {
            continue;
        }
        let Some(key) = getitem(a, "id")?.as_str() else {
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
    Ok(todo)
}

/// The envelope's `registerId` when it gave one, else the probed version's.
fn register_id_of(fetched: &Fetched, v: &Json) -> Json {
    fetched
        .meta
        .as_ref()
        .and_then(|m| m.get("registerId"))
        .filter(|id| id.truthy())
        .or_else(|| v.get("registerId"))
        .cloned()
        .unwrap_or(Json::Null)
}

/// Where the latest published compilation of manifest row `a` comes from and
/// goes to: version start, EPUB, sidecar and URL.
fn locate(a: &Json, probe: &HashMap<String, Json>, epub_dir: &str) -> Result<Located, Exc> {
    let rid = register_id(getitem(a, "id")?)?;
    let v = probe
        .get(&rid)
        .and_then(|p| p.get("latest_doc"))
        .ok_or_else(|| Exc::Key("latest_doc".into()))?
        .clone();
    let d = version_date(v.get("start"))?;
    compilation_id(&v)?;
    let dst = child(epub_dir, &[&format!("{rid}.epub")])?;
    let side = child(epub_dir, &[&format!("{rid}.epub.meta.json")])?;
    let url = format!("https://www.legislation.gov.au/{rid}/{d}/{d}/text/original/epub");
    Ok(Located {
        rid,
        v,
        d,
        dst,
        side,
        url,
    })
}

/// One title's probed version and its paths.
struct Located {
    rid: String,
    v: Json,
    d: String,
    dst: String,
    side: String,
    url: String,
}

/// Fetch the latest published compilation for manifest row `a`, write its
/// sidecar and return the patch row with the EPUB's size.
fn recover(
    http: &mut dyn Http,
    a: &Json,
    probe: &HashMap<String, Json>,
    epub_dir: &str,
    snapshots: &mut Vec<Snapshot>,
) -> Result<(Json, u64), Exc> {
    let place = locate(a, probe, epub_dir)?;
    snapshots.push(snapshot_paths(&[&place.dst, &place.side])?);
    let fetched = fetch(http, &place.url, &place.dst, 3, MAX_DOWNLOAD_BYTES)?;
    let comp_reg_id = register_id_of(&fetched, &place.v);
    let comp_num = place
        .v
        .get("compilationNumber")
        .cloned()
        .unwrap_or(Json::Null);
    let mut pairs = vec![
        ("id", Json::str(&place.rid)),
        ("sourceUrl", Json::str(&place.url)),
        ("versionStart", Json::str(&place.d)),
        ("compilationNumber", comp_num.clone()),
        ("compilationRegisterId", comp_reg_id.clone()),
        ("version_is_current", Json::Bool(false)),
        (
            "current_version_start",
            Json::str(&version_date(a.get("versionStart"))?),
        ),
        ("current_version_has_document", Json::Bool(false)),
    ];
    if let Some(meta) = &fetched.meta {
        let authorised = meta.get("isAuthorised").cloned().unwrap_or(Json::Null);
        pairs.push(("isAuthorised", authorised));
    }
    pairs.extend([
        ("epub", Json::str(PyPath::new(&place.dst).name())),
        ("bytes", Json::Int(fetched.size.into())),
        ("status", Json::str("ok_superseded_version")),
    ]);
    write_sidecar(&place, comp_num, comp_reg_id, fetched.size)?;
    Ok((crate::pyjson::obj(pairs), fetched.size))
}

/// The 4-key sidecar beside a recovered EPUB (no `fetched_at`, as Python wrote it).
fn write_sidecar(place: &Located, comp_num: Json, comp_reg_id: Json, size: u64) -> Result<(), Exc> {
    let sidecar = Json::obj([
        ("versionStart", Json::str(&place.d)),
        ("compilationNumber", comp_num),
        ("compilationRegisterId", comp_reg_id),
        ("bytes", Json::Int(size.into())),
    ]);
    write_json_atomic(&place.side, &sidecar, None, OUTPUT_DEPTH)
}

/// Merge each patch into its manifest row; returns how many rows changed.
fn merge_patches(manifest: &mut [Json], patches: &[Json]) -> usize {
    let mut recovered: HashMap<String, &Json> = HashMap::new();
    for p in patches {
        if let Some(id) = p.get("id").and_then(Json::as_str) {
            recovered.insert(id.to_string(), p);
        }
    }
    let mut merged = 0usize;
    for row in manifest {
        if let Some(id) = row.get("id").and_then(Json::as_str)
            && let Some(Json::Object(pairs)) = recovered.get(id)
        {
            let updates: Vec<(&str, Json)> = pairs
                .iter()
                .filter_map(|(k, v)| k.as_str().map(|ks| (ks, v.clone())))
                .collect();
            *row = dict_update(row, updates);
            merged += 1;
        }
    }
    merged
}

/// Shared state of one retry13 run: inputs, the patches made and the rollback journal.
struct Retry<'a> {
    http: &'a mut dyn Http,
    scratch: &'a str,
    epub_dir: &'a str,
    manifest_path: &'a str,
    probe: HashMap<String, Json>,
    delay: f64,
    run: &'a mut Run,
    patches: Vec<Json>,
    snapshots: Vec<Snapshot>,
}

impl Retry<'_> {
    /// Recover every title in `todo`, then publish the patch and the manifest.
    fn recover_all(&mut self, todo: &[Json], manifest: &mut Vec<Json>) -> Result<usize, Exc> {
        let total = todo.len();
        for (idx, a) in todo.iter().enumerate() {
            let (rec, size) = recover(
                self.http,
                a,
                &self.probe,
                self.epub_dir,
                &mut self.snapshots,
            )?;
            let rid = py_str(getitem(&rec, "id")?);
            self.patches.push(rec);
            let name = py_str(getitem(a, "name")?);
            self.run.print(&format!(
                "{:2}/{total:2} {rid:<12} {:<22} {size:9}  {}",
                idx + 1,
                "OK",
                head_str(&name, 50)
            ));
            self.http.sleep(self.delay);
        }
        let merged = merge_patches(manifest, &self.patches);
        let patch_target = pypath::join(self.scratch, "retry13_patch.json");
        let snapshot = snapshot_paths(&[&patch_target, self.manifest_path])?;
        self.snapshots.push(snapshot);
        let patch_doc = Json::Array(self.patches.clone());
        write_json_atomic(&patch_target, &patch_doc, Some(1), OUTPUT_DEPTH)?;
        if !self.patches.is_empty() {
            let doc = Json::Array(std::mem::take(manifest));
            write_json_atomic(self.manifest_path, &doc, Some(1), OUTPUT_DEPTH)?;
        }
        Ok(merged)
    }

    /// Drop the rollback copies and print the run's summary.
    fn commit(self, total: usize, merged: usize) {
        let leftovers = discard_snapshots(&self.snapshots);
        if !leftovers.is_empty() {
            let count = leftovers.len();
            self.run.print(&format!(
                "WARNING retained {count} rollback file(s) after commit"
            ));
        }
        let recovered = self.patches.len();
        self.run
            .print(&format!("\nrecovered {recovered} / {total}"));
        if recovered > 0 {
            let suffix = if merged == 1 { "y" } else { "ies" };
            self.run.print(&format!(
                "patched {merged} entr{suffix} in manifest_raw.json"
            ));
        }
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
    let probe = load_probe(scratch)?;
    let manifest_path = pypath::join(scratch, "manifest_raw.json");
    let manifest_text = crate::pyio::read_text(&manifest_path)?;
    let manifest_val = pyjson::loads_standard(&manifest_text).map_err(json_error)?;
    let mut manifest = iterate(&manifest_val)?;
    let todo = retryable(&manifest, &probe)?;
    run.print(&format!("retrying {} titles", todo.len()));
    let mut retry = Retry {
        http,
        scratch,
        epub_dir,
        manifest_path: &manifest_path,
        probe,
        delay,
        run,
        patches: Vec::new(),
        snapshots: Vec::new(),
    };
    let merged = match retry.recover_all(&todo, &mut manifest) {
        Ok(merged) => merged,
        Err(err) => {
            if rollback_snapshots(&retry.snapshots).is_err() {
                return Err(Exc::Runtime(
                    "retry failed and rollback was incomplete".into(),
                ));
            }
            return Err(err);
        }
    };
    retry.commit(todo.len(), merged);
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
