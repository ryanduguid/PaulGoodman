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
use super::http::{self, Failure, Http, Request, Transfer};
use super::{Exc, Run, get, getitem, head_str, iterate, json_error};
use crate::argparse::Exit;
use crate::pyjson::{self, Json, PyStr, set_field};
use crate::pyos::{OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyvalue::py_eq;

mod envelope;
mod snapshot;

pub use envelope::{Envelope, decode_base64_strict, decode_envelope};
pub use snapshot::{Snapshot, discard_snapshots, rollback_snapshots, snapshot_paths};

pub const CRAWL_DELAY: f64 = 10.0;
pub const RETRY_DELAY: f64 = 5.0;
pub const MAX_EPUB_BYTES: u64 = epub::MAX_EPUB_BYTES;
pub const MAX_DOWNLOAD_BYTES: u64 = 192 * 1024 * 1024;
const OUTPUT_DEPTH: usize = 990;

/// Result of one validated download attempt.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub code: String,
    pub content_type: String,
    pub size: u64,
    pub meta: Option<Json>,
}

/// `DownloadError(message)`: raised when the Register did not return a usable
/// document.
pub fn download_error(message: impl Into<String>) -> Exc {
    Exc::Other {
        class: "fadden.download.DownloadError",
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

/// Check one completed transfer in `part` and move a valid EPUB to `dst`;
/// `Ok(Err(problem))` when the attempt failed.
fn admit(
    part: &str,
    dst: &str,
    transfer: Transfer,
    max_download_bytes: u64,
) -> Result<Result<Fetched, String>, Exc> {
    let code = diagnostic(&transfer.status.to_string(), 10);
    let content_type = diagnostic(&transfer.content_type, 80);
    if let Some(err) = transfer.transport_error {
        return Ok(Err(err));
    }
    if !(200..300).contains(&transfer.status) {
        return Ok(Err(format!("HTTP {code}")));
    }
    if transfer.exceeds_budget
        || std::fs::metadata(part).is_ok_and(|m| m.len() > max_download_bytes)
    {
        return Ok(Err("download exceeds the response budget".into()));
    }
    if !Path::new(part).exists() {
        return Ok(Err("missing response body".into()));
    }
    let mut meta = None;
    if sniff(part) == Some(b'{') {
        match decode_envelope(part) {
            Envelope::Invalid => return Ok(Err("invalid JSON envelope".into())),
            Envelope::Valid(m) => meta = Some(m),
        }
    }
    if !epub::valid_zip(part) {
        return Ok(Err("invalid EPUB response".into()));
    }
    let size = std::fs::metadata(part)
        .map_err(|e| OsError::new(&e, Some(part), Style::Errno))?
        .len();
    crate::pyos::replace(part, dst)?;
    Ok(Ok(Fetched {
        code,
        content_type,
        size,
        meta,
    }))
}

/// One attempt's outcome: the fetched EPUB, or the content type and problem
/// the failure message reports.
fn settle(
    part: &str,
    dst: &str,
    outcome: Result<Transfer, Failure>,
    max_download_bytes: u64,
) -> Result<Result<Fetched, (String, String)>, Exc> {
    let none = || diagnostic("", 80);
    match outcome {
        Err(Failure::Uncaught { class, message }) => Err(http::uncaught(class, message)),
        Err(Failure::Transport(name)) => Ok(Err((none(), name))),
        Err(Failure::Status(status)) => {
            let code = diagnostic(&status.to_string(), 10);
            Ok(Err((none(), format!("HTTP {code}"))))
        }
        Ok(transfer) => {
            let ctype = diagnostic(&transfer.content_type, 80);
            Ok(admit(part, dst, transfer, max_download_bytes)?.map_err(|p| (ctype, p)))
        }
    }
}

/// Remove `part` if anything, even a dangling link, is there.
fn remove_part(part: &str) -> Result<(), Exc> {
    if std::fs::symlink_metadata(part).is_ok() {
        std::fs::remove_file(part).map_err(|e| OsError::new(&e, Some(part), Style::Errno))?;
    }
    Ok(())
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
    remove_part(&part)?;
    let mut ctype = String::new();
    let mut problem = String::from("no response");
    for attempt in 0..tries {
        let resume = attempt > 0 && Path::new(&part).exists() && sniff(&part) == Some(b'P');
        let existing = resume.then(|| std::fs::metadata(&part).map_or(0, |m| m.len()));
        let existing = existing.unwrap_or(0);
        let Some(remaining) = max_download_bytes.checked_sub(existing).filter(|r| *r > 0) else {
            problem = "download exceeds the response budget".into();
            break;
        };
        let mut request = Request::default();
        let range = resume.then_some(existing);
        let outcome = http.download(url, &part, remaining, range, true, &mut request);
        match settle(&part, dst, outcome, max_download_bytes)? {
            Ok(fetched) => return Ok(fetched),
            Err((found_type, found)) => (ctype, problem) = (found_type, found),
        }
        if attempt + 1 < tries {
            http.sleep(RETRY_DELAY);
        }
    }
    let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    remove_part(&part)?;
    Err(download_error(format!(
        "{problem} after {tries} attempt{} (content-type {ctype}, {size} bytes)",
        if tries == 1 { "" } else { "s" }
    )))
}

fn append_log_line(log: &mut File, line: &str) -> Result<(), Exc> {
    #[cfg(windows)]
    let written = {
        let normalised = line.replace("\r\n", "\n").replace('\n', "\r\n");
        log.write_all(normalised.as_bytes())
            .and_then(|()| log.write_all(b"\r\n"))
    };
    #[cfg(not(windows))]
    let written = log
        .write_all(line.as_bytes())
        .and_then(|()| log.write_all(b"\n"));
    written
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

/// One title's Register URL and local paths.
struct Target {
    rid: String,
    d: String,
    dst: String,
    side: String,
    url: String,
}

/// Counters and rollback journal for one download run.
#[derive(Default)]
struct Crawl {
    manifest: Vec<Json>,
    ok_n: usize,
    fail_n: usize,
    total_bytes: u64,
    snapshots: Vec<Snapshot>,
}

/// The manifest row for an EPUB already held for this version and
/// compilation, with its size.
fn cached(a: &Json, t: &Target, comp_reg: &Json) -> Result<Option<(Json, u64)>, Exc> {
    if !(Path::new(&t.dst).exists() && epub::valid_zip(&t.dst) && Path::new(&t.side).exists()) {
        return Ok(None);
    }
    let prev = std::fs::read_to_string(&t.side)
        .ok()
        .and_then(|s| pyjson::loads_standard(&s).ok())
        .unwrap_or_else(|| Json::Object(Vec::new()));
    let same_start = prev
        .get("versionStart")
        .is_some_and(|v| py_eq(v, &Json::str(&t.d)));
    let same_comp = match prev.get("compilationRegisterId") {
        None | Some(Json::Null) => true,
        Some(id) => py_eq(id, comp_reg),
    };
    if !(same_start && same_comp) {
        return Ok(None);
    }
    let sz = std::fs::metadata(&t.dst)
        .map_err(|e| OsError::new(&e, Some(&t.dst), Style::Errno))?
        .len();
    let field = |k: &str| prev.get(k).cloned().unwrap_or(Json::Null);
    let row = dict_update(
        a,
        vec![
            ("epub", Json::str(PyPath::new(&t.dst).name())),
            ("bytes", Json::Int(sz.into())),
            ("status", Json::str("cached")),
            ("sourceUrl", Json::str(&t.url)),
            ("compilationRegisterId", comp_reg.clone()),
            ("isAuthorised", field("isAuthorised")),
            ("fetched_at", field("fetched_at")),
        ],
    );
    Ok(Some((row, sz)))
}

/// Record a downloaded EPUB in `rec` and write its sidecar.
fn record_fetched(
    http: &mut dyn Http,
    rec: &mut Json,
    t: &Target,
    fetched: &Fetched,
) -> Result<(), Exc> {
    if let Some(meta) = &fetched.meta {
        let field = |k: &str| meta.get(k).cloned().unwrap_or(Json::Null);
        set_field(rec, "compilationRegisterId", field("registerId"));
        set_field(rec, "isAuthorised", field("isAuthorised"));
    }
    // Python's `datetime.now(timezone.utc).isoformat()` spells UTC `+00:00`.
    let now = http.utc_now();
    let fetched_at = now
        .strip_suffix('Z')
        .map_or_else(|| now.clone(), |stamp| format!("{stamp}+00:00"));
    *rec = dict_update(
        rec,
        vec![
            ("epub", Json::str(PyPath::new(&t.dst).name())),
            ("bytes", Json::Int(fetched.size.into())),
            ("status", Json::str("ok")),
            ("fetched_at", Json::str(&fetched_at)),
        ],
    );
    let field = |k: &str| -> Result<Json, Exc> { Ok(get(rec, k)?.cloned().unwrap_or(Json::Null)) };
    let sidecar = Json::obj([
        ("versionStart", Json::str(&t.d)),
        ("compilationNumber", field("compilationNumber")?),
        ("compilationRegisterId", field("compilationRegisterId")?),
        ("bytes", Json::Int(fetched.size.into())),
        ("isAuthorised", field("isAuthorised")?),
        ("fetched_at", Json::str(&fetched_at)),
    ]);
    write_json_atomic(&t.side, &sidecar, None, OUTPUT_DEPTH)
}

/// Where one resolved title comes from and goes to.
fn target(a: &Json, epub_dir: &str) -> Result<Target, Exc> {
    let rid = register_id(getitem(a, "id")?)?;
    let d = iso_date(getitem(a, "versionStart")?.as_str().unwrap_or(""))?;
    let dst = child(epub_dir, &[&format!("{rid}.epub")])?;
    let url = format!("https://www.legislation.gov.au/{rid}/{d}/{d}/text/original/epub");
    let side = child(epub_dir, &[&format!("{rid}.epub.meta.json")])?;
    Ok(Target {
        rid,
        d,
        dst,
        side,
        url,
    })
}

/// Shared state of one download run.
struct Pass<'a> {
    http: &'a mut dyn Http,
    epub_dir: &'a str,
    delay: f64,
    total: usize,
    log: &'a mut File,
    run: &'a mut Run,
    crawl: Crawl,
}

impl Pass<'_> {
    /// Print a progress line and append it to the download log.
    fn report(&mut self, line: &str) -> Result<(), Exc> {
        self.run.print(line);
        append_log_line(self.log, line)
    }

    /// The manifest row for a title whose current version has no document;
    /// a stale earlier EPUB stays, unreferenced, in case this run fails.
    fn unpublished(&mut self, t: &Target, rec: &Json) -> Result<Json, Exc> {
        let part = format!("{}.part", t.dst);
        if Path::new(&part).exists() {
            std::fs::remove_file(&part).map_err(|e| OsError::new(&e, Some(&part), Style::Errno))?;
        }
        self.crawl.fail_n += 1;
        Ok(dict_update(
            rec,
            vec![
                ("epub", Json::Null),
                ("bytes", Json::Int(0.into())),
                ("status", Json::str("no_epub")),
                ("reason", Json::str("current_version_has_no_document")),
            ],
        ))
    }

    /// Download title `i` into `rec`, journalled for rollback; returns its size.
    fn download(&mut self, i: usize, t: &Target, rec: &mut Json) -> Result<u64, Exc> {
        self.crawl
            .snapshots
            .push(snapshot_paths(&[&t.dst, &t.side])?);
        let fetched = match fetch(self.http, &t.url, &t.dst, 3, MAX_DOWNLOAD_BYTES) {
            Ok(fetched) => fetched,
            Err(err) => {
                let (total, rid) = (self.total, &t.rid);
                self.report(&format!("{i:3}/{total:3} {rid:<12} ERROR    {err}"))?;
                return Err(err);
            }
        };
        self.crawl.ok_n += 1;
        self.crawl.total_bytes += fetched.size;
        record_fetched(self.http, rec, t, &fetched)?;
        Ok(fetched.size)
    }

    /// Download, or reuse, title `i` of the run.
    fn title(&mut self, i: usize, a: &Json) -> Result<(), Exc> {
        let t = target(a, self.epub_dir)?;
        let Some(comp_reg) = a.get("compilationRegisterId") else {
            let rid = &t.rid;
            return Err(Exc::value(format!("{rid}: missing compilationRegisterId")));
        };
        let no_document = matches!(comp_reg, Json::Null);
        if !no_document && let Some((row, sz)) = cached(a, &t, comp_reg)? {
            self.crawl.manifest.push(row);
            self.crawl.ok_n += 1;
            self.crawl.total_bytes += sz;
            return Ok(());
        }
        let mut rec = dict_update(a, vec![("sourceUrl", Json::str(&t.url))]);
        let (sz, label) = if no_document {
            rec = self.unpublished(&t, &rec)?;
            (0u64, "NO_EPUB")
        } else {
            (self.download(i, &t, &mut rec)?, "OK")
        };
        self.crawl.manifest.push(rec);
        let name = str_field(a, "name")?;
        let (total, rid) = (self.total, &t.rid);
        self.report(&format!(
            "{i:3}/{total:3} {rid:<12} {label:<8} {sz:9}  {}",
            head_str(&name, 58)
        ))?;
        self.http.sleep(self.delay);
        Ok(())
    }
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
    let acts = iterate(&pyjson::loads_standard(&text).map_err(json_error)?)?;
    let log_path = pypath::join(scratch, "download_log.txt");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| OsError::new(&e, Some(&log_path), Style::Errno))?;
    let mut pass = Pass {
        http,
        epub_dir,
        delay,
        total: acts.len(),
        log: &mut log,
        run,
        crawl: Crawl::default(),
    };
    let body = (|| -> Result<(), Exc> {
        for (idx, a) in acts.iter().enumerate() {
            pass.title(idx + 1, a)?;
        }
        let target = pypath::join(scratch, "manifest_raw.json");
        let manifest = Json::Array(std::mem::take(&mut pass.crawl.manifest));
        write_json_atomic(&target, &manifest, Some(1), OUTPUT_DEPTH)
    })();
    let Crawl {
        ok_n,
        fail_n,
        total_bytes,
        snapshots,
        ..
    } = pass.crawl;
    let summary = run_summary(ok_n, fail_n, total_bytes);
    finish(body, &snapshots, pass.log, summary, pass.run)
}

fn run_summary(ok_n: usize, fail_n: usize, total_bytes: u64) -> String {
    format!(
        "\nDONE ok={ok_n} no_epub={fail_n} total={:.1} MB",
        total_bytes as f64 / 1e6
    )
}

/// Roll back after a failure, or commit and print the summary.
fn finish(
    body: Result<(), Exc>,
    snapshots: &[Snapshot],
    log: &mut File,
    summary: String,
    run: &mut Run,
) -> Result<(), Exc> {
    if let Err(err) = body {
        if rollback_snapshots(snapshots).is_err() {
            return Err(Exc::Runtime(
                "download failed and rollback was incomplete".into(),
            ));
        }
        let line = format!("ROLLBACK restored {} changed title(s)", snapshots.len());
        append_log_line(log, &line)?;
        return Err(err);
    }
    let leftovers = discard_snapshots(snapshots);
    if !leftovers.is_empty() {
        let line = format!(
            "WARNING retained {} rollback file(s) after commit",
            leftovers.len()
        );
        append_log_line(log, &line)?;
    }
    run.print(&summary);
    append_log_line(log, &summary)
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

#[cfg(test)]
mod tests {
    use super::append_log_line;

    #[test]
    fn append_log_line_normalises_embedded_newlines_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.txt");
        let mut file = std::fs::File::create(&path).unwrap();
        append_log_line(&mut file, "\nDONE ok=1").unwrap();
        append_log_line(&mut file, "a\r\nb\nc").unwrap();
        drop(file);
        let bytes = std::fs::read(&path).unwrap();
        #[cfg(windows)]
        assert_eq!(bytes, b"\r\nDONE ok=1\r\na\r\nb\r\nc\r\n");
        #[cfg(not(windows))]
        assert_eq!(bytes, b"\nDONE ok=1\na\r\nb\nc\n");
    }

    #[test]
    fn sniff_settle_and_str_field_edge_cases() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.part");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(sniff(&empty.to_string_lossy()), None);
        assert!(
            settle(
                "p",
                "d",
                Err(Failure::Uncaught {
                    class: "RuntimeError",
                    message: "x".into(),
                }),
                100,
            )
            .is_err()
        );
        let http_err = settle("p", "d", Err(Failure::Status(404)), 100)
            .unwrap()
            .unwrap_err();
        assert_eq!(http_err.1, "HTTP 404");
        let obj = Json::Object(vec![
            (
                PyStr::Text("lone".into()),
                Json::Str(PyStr::Lone(vec![0xd800])),
            ),
            (PyStr::Text("num".into()), Json::Int(7.into())),
        ]);
        assert_eq!(str_field(&obj, "lone").unwrap(), "'\\ud800'");
        assert_eq!(str_field(&obj, "num").unwrap(), "7");
    }

    #[test]
    fn finish_reports_incomplete_rollback_and_retained_backups() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t.epub");
        std::fs::write(&target, b"old").unwrap();
        let tp = target.to_string_lossy().into_owned();
        let snap = snapshot_paths(&[&tp]).unwrap();
        let backup = format!("{tp}.rollback");
        std::fs::remove_file(&backup).unwrap();
        let mut log = std::fs::File::create(dir.path().join("l.txt")).unwrap();
        let mut run = Run::default();
        let err = finish(
            Err(Exc::Runtime("boom".into())),
            std::slice::from_ref(&snap),
            &mut log,
            "DONE".into(),
            &mut run,
        )
        .unwrap_err();
        assert!(err.to_string().contains("rollback was incomplete"));
        std::fs::create_dir(&backup).unwrap();
        std::fs::write(std::path::Path::new(&backup).join("x"), b"1").unwrap();
        finish(Ok(()), &[snap], &mut log, "DONE".into(), &mut run).unwrap();
        assert!(
            std::fs::read_to_string(dir.path().join("l.txt"))
                .unwrap()
                .contains("WARNING retained 1 rollback")
        );
    }
}
