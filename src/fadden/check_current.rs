// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden/check_current.py`: report which titles in `sources.json` have been
//! superseded on the Federal Register API, lack a published compilation, or
//! are no longer in force. Read-only, no downloads.
use std::path::Path;

use super::corpus_paths::{
    child, compilation_id, corpus_root, module_dir, register_id, require_builder_layout,
    script_dir_in, version_date, version_rows,
};
use super::discover::quote;
use super::http::{self, Http};
use super::{Exc, Run, get, getitem, head_str, iterate, json_error, round_half_even};
use crate::argparse::Exit;
use crate::py;
use crate::pyjson::{self, Json};
use crate::pypath;
use crate::pyvalue::py_str;

const API: &str = "https://api.prod.legislation.gov.au/v1";
pub const DELAY: f64 = 1.5;

fn versions_url(flt: &str, top: usize) -> String {
    format!(
        "{API}/versions?$top={top}&$filter={}&$select=titleId,start,compilationNumber,registerId,status",
        quote(flt)
    )
}

fn fetch_versions(http: &mut dyn Http, flt: &str, rid: &str) -> Result<Option<Vec<Json>>, Exc> {
    let Some(doc) = http::fetch_json(http, &versions_url(flt, 1))? else {
        return Ok(None);
    };
    match version_rows(&doc, rid) {
        Ok(rows) => Ok(Some(rows)),
        Err(Exc::Value { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

/// Run check_current against an explicit corpus root directory.
pub fn run_root(
    http: &mut dyn Http,
    root: &str,
    want: Option<&str>,
    run: &mut Run,
) -> Result<(), Exc> {
    let src_path = child(root, &["sources.json"])?;
    let src_text = crate::pyio::read_text(&src_path)?;
    let src = pyjson::loads_standard(&src_text).map_err(json_error)?;

    let raw_acts = match src.get("titles") {
        Some(t) if t.truthy() => t,
        _ => getitem(&src, "acts")?,
    };
    let mut acts = iterate(raw_acts)?;
    if let Some(want) = want.filter(|w| !w.is_empty()) {
        acts.retain(|a| {
            let col = a.get("collection").and_then(Json::as_str).unwrap_or("");
            col == want
        });
        if acts.is_empty() {
            run.print(&format!("No titles in collection {}.", py::repr_str(want)));
            return Ok(());
        }
    }

    let retrieved = py_str(getitem(&src, "retrieved")?);
    let mins = round_half_even(acts.len() as f64 * DELAY / 60.0).max(1);
    run.print(&format!(
        "Built {retrieved}. Checking {} titles against the Register.  Roughly {mins} minutes.\n",
        acts.len()
    ));

    let mut stale: Vec<(Json, Json, String)> = Vec::new();
    let mut unchanged = 0usize;
    let mut errors: Vec<Json> = Vec::new();
    let mut gone: Vec<Json> = Vec::new();
    let mut nodoc: Vec<(Json, String)> = Vec::new();
    let total = acts.len();

    for (idx, a) in acts.into_iter().enumerate() {
        let i = idx + 1;
        let reg_id_val = getitem(&a, "register_id")?;
        let rid = register_id(reg_id_val)?;
        let current = fetch_versions(
            http,
            &format!("titleId eq '{rid}' and isCurrent eq true"),
            &rid,
        )?;
        match current {
            None => errors.push(a),
            Some(ref v)
                if !v.is_empty()
                    && !v[0].get("titleId").is_none_or(|t| match t {
                        Json::Null => true,
                        _ => crate::pyvalue::py_eq(t, reg_id_val),
                    }) =>
            {
                errors.push(a);
            }
            Some(ref v) if v.is_empty() => {
                let any_v = fetch_versions(http, &format!("titleId eq '{rid}'"), &rid)?;
                http.sleep(DELAY);
                match any_v {
                    None => errors.push(a),
                    Some(ref av)
                        if !av.is_empty()
                            && av[0].get("titleId").is_none_or(|t| match t {
                                Json::Null => true,
                                _ => crate::pyvalue::py_eq(t, reg_id_val),
                            }) =>
                    {
                        gone.push(a);
                    }
                    _ => errors.push(a),
                }
            }
            Some(ref v)
                if !matches!(v[0].get("status"), None | Some(Json::Null))
                    && v[0].get("status").and_then(Json::as_str) != Some("InForce") =>
            {
                gone.push(a);
            }
            Some(ref v) => {
                let first = &v[0];
                let parsed =
                    version_date(first.get("start")).and_then(|d| Ok((d, compilation_id(first)?)));
                match parsed {
                    Err(Exc::Value { .. }) => {
                        errors.push(a);
                        http.sleep(DELAY);
                        continue;
                    }
                    Err(other) => return Err(other),
                    Ok((now_d, document_id)) => {
                        let now_c = first
                            .get("compilationNumber")
                            .cloned()
                            .unwrap_or(Json::Null);
                        let old_c = getitem(&a, "compilation_number")?;
                        let old_d = getitem(&a, "compilation_date")?;
                        let old_d_eq = matches!(old_d.as_str(), Some(s) if s == now_d);
                        if py_str(&now_c) != py_str(old_c) || !old_d_eq {
                            if document_id.is_none() {
                                nodoc.push((a, now_d));
                            } else {
                                stale.push((a, now_c, now_d));
                            }
                        } else {
                            unchanged += 1;
                        }
                    }
                }
            }
        }
        if i % 25 == 0 {
            run.print(&format!("  checked {i}/{total}"));
        }
        http.sleep(DELAY);
    }

    run.print(&format!(
        "\nunchanged: {unchanged}   superseded: {}   no compilation published: {}   no longer in force: {}   lookup failed: {}",
        stale.len(),
        nodoc.len(),
        gone.len(),
        errors.len(),
    ));

    if !nodoc.is_empty() {
        run.print(
            "\nNO COMPILATION PUBLISHED for the version now in force. The text \
             here is the last one the Register holds, already marked \
             version_is_current: false. Re-downloading returns 404 until the \
             Register publishes the compilation:",
        );
        for (a, dt) in &nodoc {
            let rid = py_str(getitem(a, "register_id")?);
            let comp_date = py_str(getitem(a, "compilation_date")?);
            let name = py_str(getitem(a, "name")?);
            run.print(&format!(
                "  {rid:<12} holds {comp_date}, in force since {dt}  {}",
                head_str(&name, 44)
            ));
        }
    }
    if !stale.is_empty() {
        run.print("\nSUPERSEDED, re-download these:");
        for (a, c, dt) in &stale {
            let rid = py_str(getitem(a, "register_id")?);
            let old_c = py_str(getitem(a, "compilation_number")?);
            let old_d = py_str(getitem(a, "compilation_date")?);
            let c_str = py_str(c);
            let name = py_str(getitem(a, "name")?);
            run.print(&format!(
                "  {rid:<12} comp {old_c} ({old_d}) -> comp {c_str} ({dt})  {}",
                head_str(&name, 55)
            ));
        }
    }
    if !gone.is_empty() {
        run.print(
            "\nNO LONGER IN FORCE (repealed, or sunset under Part 4 of the \
             Legislation Act 2003). Drop these:",
        );
        for a in &gone {
            let rid = py_str(getitem(a, "register_id")?);
            let col = match get(a, "collection")? {
                Some(v) if v.truthy() => py_str(v),
                _ => "-".to_string(),
            };
            let name = py_str(getitem(a, "name")?);
            run.print(&format!("  {rid:<12} {col:<22} {}", head_str(&name, 50)));
        }
    }
    if !errors.is_empty() {
        run.print("\nLOOKUP FAILED:");
        for a in &errors {
            let rid = py_str(getitem(a, "register_id")?);
            let name = py_str(getitem(a, "name")?);
            run.print(&format!("  {rid:<12} {}", head_str(&name, 60)));
        }
    }
    Ok(())
}

/// Resolve root using `check_current.py` layout rules (`SCRIPT_DIR/sources.json`
/// if present, else `corpus_root`) and execute.
pub fn run(
    http: &mut dyn Http,
    scratch: &str,
    want: Option<&str>,
    run: &mut Run,
) -> Result<(), Exc> {
    let directory = script_dir_in(scratch, "check_current.py")?;
    require_builder_layout(&directory)?;
    let root = if Path::new(&pypath::join(&directory.display(), "sources.json")).is_file() {
        directory.display()
    } else {
        corpus_root(&directory)
    };
    run_root(http, &root, want, run)
}

/// CLI entrypoint (`python -m fadden check_current`).
pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let want = argv.first().map(String::as_str);
    let mut http = http::connect();
    let outcome = self::run(http.as_mut(), &module_dir(), want, run);
    Ok(match outcome {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}
