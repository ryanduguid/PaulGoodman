// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! Stage 3 (`fadden/download.py`): download the current EPUB for each resolved
//! Act, honouring the Register's `Crawl-delay` of 10 seconds and rolling back
//! changed files on mid-run or manifest failure.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use super::corpus_paths::{
    child, corpus_root, iso_date, module_dir, register_id, require_builder_layout, script_dir_in,
    write_json_atomic,
};
use super::epub;
use super::http::{self, Failure, Http, Request};
use super::{Exc, Run, get, getitem, head_str, iterate, json_error};
use crate::argparse::Exit;
use crate::pyjson::{self, Json, PyStr, set_field};
use crate::pyos::{OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyvalue::py_eq;

pub const CRAWL_DELAY: f64 = 10.0;
pub const RETRY_DELAY: f64 = 5.0;
pub const MAX_EPUB_BYTES: u64 = epub::MAX_EPUB_BYTES;
pub const MAX_DOWNLOAD_BYTES: u64 = 192 * 1024 * 1024;
const OUTPUT_DEPTH: usize = 990;

const ENVELOPE_KEYS: [&str; 7] = [
    "registerId",
    "fileName",
    "sizeInBytes",
    "isAuthorised",
    "compilationNumber",
    "mimeType",
    "extension",
];

/// Result of one validated download attempt.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub code: String,
    pub content_type: String,
    pub size: u64,
    pub meta: Option<Json>,
}

/// Whole-run backup state for rollback on failure.
#[derive(Debug, Clone)]
pub struct Snapshot {
    entries: Vec<(String, String, bool)>,
}

/// `DownloadError(message)`: raised when the Register did not return a usable
/// document.
pub fn download_error(message: impl Into<String>) -> Exc {
    Exc::Other {
        class: "DownloadError",
        message: message.into(),
    }
}

/// Collapse and bound untrusted response metadata for logs.
pub fn diagnostic(value: &str, limit: usize) -> String {
    let sanitized: String = value
        .chars()
        .map(|c| if crate::py::is_printable(c) { c } else { ' ' })
        .collect();
    let collapsed = crate::py::split_whitespace(&sanitized).join(" ");
    let truncated: String = collapsed.chars().take(limit).collect();
    if truncated.is_empty() {
        "?".to_string()
    } else {
        truncated
    }
}

/// Back up a group of paths for whole-run rollback.
pub fn snapshot_paths(paths: &[&str]) -> Result<Snapshot, Exc> {
    let mut entries = Vec::with_capacity(paths.len());
    for &path in paths {
        let backup = format!("{path}.rollback");
        if Path::new(&backup).exists() {
            return Err(Exc::Runtime(format!("unfinished rollback file: {backup}")));
        }
        entries.push((path.to_string(), backup, Path::new(path).exists()));
    }
    for (path, backup, existed) in &entries {
        if *existed && let Err(err) = copy_file(path, backup) {
            for (_, b, _) in &entries {
                if Path::new(b).exists() {
                    let _ = std::fs::remove_file(b);
                }
            }
            return Err(err);
        }
    }
    Ok(Snapshot { entries })
}

fn copy_file(source: &str, destination: &str) -> Result<(), Exc> {
    std::fs::copy(source, destination).map_err(|e| OsError::new(&e, Some(source), Style::Errno))?;
    if let Ok(meta) = std::fs::metadata(source) {
        let _ = std::fs::set_permissions(destination, meta.permissions());
    }
    Ok(())
}

/// Restore every path changed since the prior manifest was read.
pub fn rollback_snapshots(snapshots: &[Snapshot]) -> Result<(), Exc> {
    for snapshot in snapshots.iter().rev() {
        let new_pair_written = snapshot
            .entries
            .iter()
            .all(|(path, _, existed)| !existed && Path::new(path).exists());
        for (path, backup, existed) in snapshot.entries.iter().rev() {
            if *existed {
                if !Path::new(backup).exists() {
                    return Err(Exc::Runtime(format!("missing rollback file: {backup}")));
                }
                crate::pyos::replace(backup, path)?;
            } else if Path::new(path).exists() && !new_pair_written {
                std::fs::remove_file(path)
                    .map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
            }
        }
        for (path, backup, _) in &snapshot.entries {
            for transient in [backup.as_str(), &format!("{path}.part")] {
                if Path::new(transient).exists() {
                    std::fs::remove_file(transient)
                        .map_err(|e| OsError::new(&e, Some(transient), Style::Errno))?;
                }
            }
        }
    }
    Ok(())
}

/// Remove rollback copies after the new manifest commits; returns any paths that
/// could not be deleted.
pub fn discard_snapshots(snapshots: &[Snapshot]) -> Vec<String> {
    let mut leftovers = Vec::new();
    for snapshot in snapshots {
        for (_, backup, _) in &snapshot.entries {
            if Path::new(backup).exists() && std::fs::remove_file(backup).is_err() {
                leftovers.push(backup.clone());
            }
        }
    }
    leftovers
}

/// Read the first byte of `path`, or `None` if unreadable/empty.
pub fn sniff(path: &str) -> Option<u8> {
    use std::io::Read as _;
    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 1];
    if file.read(&mut buf).ok()? == 1 {
        Some(buf[0])
    } else {
        None
    }
}

/// `base64.b64decode(text, validate=True)` over an ASCII base64 string.
pub fn decode_base64_strict(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let val = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let (chunks, _) = bytes.as_chunks::<4>();
    let last_chunk = chunks.len() - 1;
    for (idx, chunk) in chunks.iter().enumerate() {
        let is_last = idx == last_chunk;
        let a = val(chunk[0])?;
        let b = val(chunk[1])?;
        if chunk[2] == b'=' {
            if !is_last || chunk[3] != b'=' {
                return None;
            }
            out.push((a << 2) | (b >> 4));
        } else if chunk[3] == b'=' {
            if !is_last {
                return None;
            }
            let c = val(chunk[2])?;
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (c >> 2));
        } else {
            let c = val(chunk[2])?;
            let d = val(chunk[3])?;
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

/// Replace `path` with the decoded EPUB if it is a valid JSON envelope.
pub fn decode_envelope(path: &str) -> Envelope {
    let Ok(meta) = std::fs::metadata(path) else {
        return Envelope::Invalid;
    };
    if meta.len() > MAX_DOWNLOAD_BYTES {
        return Envelope::Invalid;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Envelope::Invalid;
    };
    let Ok(doc) = pyjson::loads_standard(&text) else {
        return Envelope::Invalid;
    };
    let Json::Object(_) = &doc else {
        return Envelope::Invalid;
    };
    let Some(Json::Str(PyStr::Text(b64))) = doc.get("bytes") else {
        return Envelope::Invalid;
    };
    if b64.is_empty() || !b64.is_ascii() {
        return Envelope::Invalid;
    }
    let max_b64_len = 4 * ((MAX_EPUB_BYTES as usize).div_ceil(3));
    if b64.len() > max_b64_len {
        return Envelope::Invalid;
    }
    let Some(decoded) = decode_base64_strict(b64) else {
        return Envelope::Invalid;
    };
    if decoded.len() as u64 > MAX_EPUB_BYTES {
        return Envelope::Invalid;
    }
    if std::fs::write(path, &decoded).is_err() {
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

/// Fetch one EPUB into `dst` with bounded streaming, HTTP 206 resume and validation.
pub fn fetch(
    http: &mut dyn Http,
    url: &str,
    dst: &str,
    tries: u32,
    max_download_bytes: u64,
) -> Result<Fetched, Exc> {
    let part = format!("{dst}.part");
    if Path::new(&part).exists() {
        std::fs::remove_file(&part).map_err(|e| OsError::new(&e, Some(&part), Style::Errno))?;
    }
    let mut ctype = String::new();
    let mut problem = String::from("no response");
    for attempt in 0..tries {
        let mut meta = None;
        let resume = attempt > 0 && Path::new(&part).exists() && sniff(&part) == Some(b'P');
        let existing = if resume {
            std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        let remaining = i128::from(max_download_bytes) - i128::from(existing);
        if remaining <= 0 {
            problem = "download exceeds the response budget".into();
            break;
        }
        let mut request = Request::default();
        let outcome = http.download(
            url,
            &part,
            remaining as u64,
            resume.then_some(existing),
            true,
            &mut request,
        );
        match outcome {
            Err(Failure::Uncaught { class, message }) => {
                return Err(http::uncaught(class, message));
            }
            Err(Failure::Transport(name)) => {
                ctype = diagnostic("", 80);
                problem = name;
            }
            Err(Failure::Status(status)) => {
                let code = diagnostic(&status.to_string(), 10);
                ctype = diagnostic("", 80);
                problem = format!("HTTP {code}");
            }
            Ok(transfer) => {
                let code = diagnostic(&transfer.status.to_string(), 10);
                ctype = diagnostic(&transfer.content_type, 80);
                if let Some(err) = transfer.transport_error {
                    problem = err;
                } else if !(200..300).contains(&transfer.status) {
                    problem = format!("HTTP {code}");
                } else if transfer.exceeds_budget
                    || std::fs::metadata(&part).is_ok_and(|m| m.len() > max_download_bytes)
                {
                    problem = "download exceeds the response budget".into();
                } else if Path::new(&part).exists() {
                    let mut invalid_envelope = false;
                    if sniff(&part) == Some(b'{') {
                        match decode_envelope(&part) {
                            Envelope::Invalid => invalid_envelope = true,
                            Envelope::Valid(m) => meta = Some(m),
                        }
                    }
                    if !invalid_envelope && epub::valid_zip(&part) {
                        let size = std::fs::metadata(&part)
                            .map_err(|e| OsError::new(&e, Some(&part), Style::Errno))?
                            .len();
                        crate::pyos::replace(&part, dst)?;
                        return Ok(Fetched {
                            code,
                            content_type: ctype,
                            size,
                            meta,
                        });
                    }
                    problem = if invalid_envelope {
                        "invalid JSON envelope".into()
                    } else {
                        "invalid EPUB response".into()
                    };
                } else {
                    problem = "missing response body".into();
                }
            }
        }
        if attempt + 1 < tries {
            http.sleep(RETRY_DELAY);
        }
    }
    let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if Path::new(&part).exists() {
        std::fs::remove_file(&part).map_err(|e| OsError::new(&e, Some(&part), Style::Errno))?;
    }
    Err(download_error(format!(
        "{problem} after {tries} attempt{} (content-type {ctype}, {size} bytes)",
        if tries == 1 { "" } else { "s" }
    )))
}

fn append_log_line(log: &mut File, line: &str) -> Result<(), Exc> {
    let eol = if cfg!(windows) { "\r\n" } else { "\n" };
    log.write_all(line.as_bytes())
        .and_then(|()| log.write_all(eol.as_bytes()))
        .and_then(|()| log.flush())
        .map_err(|e| OsError::new(&e, None, Style::Errno).into())
}

fn str_field(obj: &Json, key: &str) -> Result<String, Exc> {
    let value = getitem(obj, key)?;
    match value {
        Json::Str(PyStr::Text(s)) => Ok(s.clone()),
        Json::Str(PyStr::Lone(points)) => Ok(crate::pyvalue::repr_points(points)),
        _ => Ok(crate::pyvalue::py_str(value)),
    }
}

/// Update `target` in place with `(key, value)` pairs, preserving existing order.
pub(crate) fn dict_update(base: &Json, updates: Vec<(&str, Json)>) -> Json {
    let mut out = base.clone();
    for (k, v) in updates {
        set_field(&mut out, k, v);
    }
    out
}

/// Execute the download stage with explicit `Http`, `scratch`, `epub_dir` and `delay`.
pub fn run(
    http: &mut dyn Http,
    scratch: &str,
    epub_dir: &str,
    delay: f64,
    run: &mut Run,
) -> Result<(), Exc> {
    crate::pyos::makedirs(&PyPath::new(epub_dir))?;
    let acts_path = pypath::join(scratch, "acts_resolved.json");
    let text = crate::pyio::read_text(&acts_path)?;
    let acts = pyjson::loads_standard(&text).map_err(json_error)?;
    let acts = iterate(&acts)?;

    let log_path = pypath::join(scratch, "download_log.txt");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| OsError::new(&e, Some(&log_path), Style::Errno))?;

    let mut manifest = Vec::with_capacity(acts.len());
    let mut ok_n = 0usize;
    let mut fail_n = 0usize;
    let mut total_bytes = 0u64;
    let mut snapshots = Vec::new();
    let total = acts.len();

    let body = (|| -> Result<(), Exc> {
        for (idx, a) in acts.iter().enumerate() {
            let i = idx + 1;
            let rid = register_id(getitem(a, "id")?)?;
            let start = getitem(a, "versionStart")?;
            let d = match start.as_str() {
                Some(s) => iso_date(s)?,
                None => iso_date("")?,
            };
            let dst = child(epub_dir, &[&format!("{rid}.epub")])?;
            let url = format!("https://www.legislation.gov.au/{rid}/{d}/{d}/text/original/epub");
            let Some(comp_reg) = a.get("compilationRegisterId") else {
                return Err(Exc::value(format!("{rid}: missing compilationRegisterId")));
            };
            let no_document = matches!(comp_reg, Json::Null);

            let side = child(epub_dir, &[&format!("{rid}.epub.meta.json")])?;
            if !no_document
                && Path::new(&dst).exists()
                && epub::valid_zip(&dst)
                && Path::new(&side).exists()
            {
                let prev = std::fs::read_to_string(&side)
                    .ok()
                    .and_then(|s| pyjson::loads_standard(&s).ok())
                    .unwrap_or_else(|| Json::Object(Vec::new()));
                let cached_id = prev.get("compilationRegisterId");
                let same_start = prev
                    .get("versionStart")
                    .is_some_and(|v| py_eq(v, &Json::str(&d)));
                let same_comp = match cached_id {
                    None | Some(Json::Null) => true,
                    Some(id) => py_eq(id, comp_reg),
                };
                if same_start && same_comp {
                    let sz = std::fs::metadata(&dst)
                        .map_err(|e| OsError::new(&e, Some(&dst), Style::Errno))?
                        .len();
                    let dst_name = PyPath::new(&dst).name().to_string();
                    manifest.push(dict_update(
                        a,
                        vec![
                            ("epub", Json::str(&dst_name)),
                            ("bytes", Json::Int(sz.into())),
                            ("status", Json::str("cached")),
                            ("sourceUrl", Json::str(&url)),
                            ("compilationRegisterId", comp_reg.clone()),
                            (
                                "isAuthorised",
                                prev.get("isAuthorised").cloned().unwrap_or(Json::Null),
                            ),
                            (
                                "fetched_at",
                                prev.get("fetched_at").cloned().unwrap_or(Json::Null),
                            ),
                        ],
                    ));
                    ok_n += 1;
                    total_bytes += sz;
                    continue;
                }
            }
            if no_document {
                let part = format!("{dst}.part");
                if Path::new(&part).exists() {
                    std::fs::remove_file(&part)
                        .map_err(|e| OsError::new(&e, Some(&part), Style::Errno))?;
                }
            }

            let mut rec = dict_update(a, vec![("sourceUrl", Json::str(&url))]);
            let (sz, label) = if no_document {
                fail_n += 1;
                rec = dict_update(
                    &rec,
                    vec![
                        ("epub", Json::Null),
                        ("bytes", Json::Int(0.into())),
                        ("status", Json::str("no_epub")),
                        ("reason", Json::str("current_version_has_no_document")),
                    ],
                );
                (0u64, "NO_EPUB")
            } else {
                snapshots.push(snapshot_paths(&[&dst, &side])?);
                let fetched = match fetch(http, &url, &dst, 3, MAX_DOWNLOAD_BYTES) {
                    Ok(fetched) => fetched,
                    Err(err) => {
                        let line = format!("{i:3}/{total:3} {rid:<12} ERROR    {err}");
                        run.print(&line);
                        append_log_line(&mut log, &line)?;
                        return Err(err);
                    }
                };
                if let Some(meta) = &fetched.meta {
                    set_field(
                        &mut rec,
                        "compilationRegisterId",
                        meta.get("registerId").cloned().unwrap_or(Json::Null),
                    );
                    set_field(
                        &mut rec,
                        "isAuthorised",
                        meta.get("isAuthorised").cloned().unwrap_or(Json::Null),
                    );
                }
                ok_n += 1;
                total_bytes += fetched.size;
                let fetched_at = http.utc_now();
                let dst_name = PyPath::new(&dst).name().to_string();
                rec = dict_update(
                    &rec,
                    vec![
                        ("epub", Json::str(&dst_name)),
                        ("bytes", Json::Int(fetched.size.into())),
                        ("status", Json::str("ok")),
                        ("fetched_at", Json::str(&fetched_at)),
                    ],
                );
                let sidecar = Json::obj([
                    ("versionStart", Json::str(&d)),
                    (
                        "compilationNumber",
                        get(&rec, "compilationNumber")?
                            .cloned()
                            .unwrap_or(Json::Null),
                    ),
                    (
                        "compilationRegisterId",
                        get(&rec, "compilationRegisterId")?
                            .cloned()
                            .unwrap_or(Json::Null),
                    ),
                    ("bytes", Json::Int(fetched.size.into())),
                    (
                        "isAuthorised",
                        get(&rec, "isAuthorised")?.cloned().unwrap_or(Json::Null),
                    ),
                    ("fetched_at", Json::str(&fetched_at)),
                ]);
                write_json_atomic(&side, &sidecar, None, OUTPUT_DEPTH)?;
                (fetched.size, "OK")
            };
            manifest.push(rec);

            let name = str_field(a, "name")?;
            let line = format!(
                "{i:3}/{total:3} {rid:<12} {label:<8} {sz:9}  {}",
                head_str(&name, 58)
            );
            run.print(&line);
            append_log_line(&mut log, &line)?;
            http.sleep(delay);
        }

        let target = pypath::join(scratch, "manifest_raw.json");
        write_json_atomic(&target, &Json::Array(manifest), Some(1), OUTPUT_DEPTH)?;
        Ok(())
    })();

    if let Err(err) = body {
        if rollback_snapshots(&snapshots).is_err() {
            return Err(Exc::Runtime(
                "download failed and rollback was incomplete".into(),
            ));
        }
        append_log_line(
            &mut log,
            &format!("ROLLBACK restored {} changed title(s)", snapshots.len()),
        )?;
        return Err(err);
    }

    let leftovers = discard_snapshots(&snapshots);
    if !leftovers.is_empty() {
        append_log_line(
            &mut log,
            &format!(
                "WARNING retained {} rollback file(s) after commit",
                leftovers.len()
            ),
        )?;
    }
    let summary = format!(
        "\nDONE ok={ok_n} no_epub={fail_n} total={:.1} MB",
        total_bytes as f64 / 1e6
    );
    run.print(&summary);
    append_log_line(&mut log, &summary)?;
    Ok(())
}

/// CLI entrypoint (`python -m fadden download`).
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let outcome = (|| -> Result<(), Exc> {
        let scratch = module_dir();
        let directory = script_dir_in(&scratch, "download.py")?;
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
