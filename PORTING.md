# Rust port notes

This repository now runs every Register corpus-pipeline stage except live Register capture and live-evidence bundle export in the Rust `fadden` binary (`python -m fadden <stage>` delegates directly through `RUST_STAGES` in `fadden/__main__.py`).

## Item correspondence (Wave 2: `download`, `retry13`, `quick`, `check_current`)

| Retired Python item | Rust replacement | Notes |
| --- | --- | --- |
| `fadden/download.py` | `src/fadden/download.rs` | Pure-Rust streaming download (`download::run`, `download::fetch`, `download::main`), rollback snapshots (`Snapshot`, `snapshot_paths`, `rollback_snapshots`, `discard_snapshots`), base64 JSON envelope check (`decode_envelope`, `decode_base64_strict`), ZIP validity check via `epub::valid_zip` |
| `fadden/retry13.py` | `src/fadden/retry13.rs` | Retries `status == "no_epub"` items using `probe13.json` (`retry13::run`, `retry13::main`), writing `retry13_patch.json`, patching `manifest_raw.json`, writing 4-key sidecars, and rolling back EPUBs, sidecars and manifests atomically on later failures |
| `fadden/check_current.py` | `src/fadden/check_current.rs` | Read-only staleness lookup across `sources.json` (`check_current::run_root`, `check_current::run`, `check_current::main`) classifying `unchanged`, `superseded`, `no compilation published`, `no longer in force` and `lookup failed` |
| `fadden/quick.py` | `src/fadden/quick.rs` | Single-command 9-Act build (`quick::build`, `quick::run_cli`, `quick::main`, `quick::prepare`, `quick::lay_out`, `quick::check_preset`, `quick::check_windows`, `quick::Runner`, `quick::RealRunner`) with `--out` and `--in-force-on`, sharing one `Http` session across in-process HTTP stages and spawning `current_exe()` for local stages |
| Streaming & ZIP validation helpers | `src/fadden/http.rs`, `src/fadden/epub.rs`, `src/fadden/versions.rs`, `src/fadden/mod.rs` | Added `Http::download`, `Transfer`, `Net::download`, `http::net_download`, `epub::valid_zip`, `epub::valid_zip_bytes`, `versions::run_cli`, `fadden::head_str`, `fadden::round_half_even` |
| `fadden/finalize.py` (earlier Wave 1 port) checker copy | `src/fadden/finalize.rs`, `src/fadden/dist.rs` | Updated `finalize` after retiring `check_current.py`: no longer copies Python checker scripts into `./corpus/`, writes `shipped_tools.files: []` in `build-record.json`, and documents `python -m fadden check_current` in generated `README.md` |

## Test correspondence

| Retired Python tests | Rust & retained tests | Notes |
| --- | --- | --- |
| `tests/corpus/test_download_paths.py`, `tests/corpus/test_security_remediation.py`, `tests/corpus/test_regressions.py` (`ArchiveLifecycleTests`, `Retry13*`, `Download*`) | `tests/fadden_download.rs`, `tests/fadden_retry13.rs` | Unit, rollback, `Range: bytes=<offset>-` HTTP `206` resume, HTTPS-only & redirect refusal over real loopback TCP sockets, JSON envelope pre-checks, 4-key sidecar layout, and `--features fake-http` binary runs |
| Staleness classification tests | `tests/fadden_check_current.rs` | Deployed vs checkout root resolution, collection filters, injected filter rejection, history vs status repeal fallback, all 5 buckets, 25-title progress lines, and binary execution under `FADDEN_FAKE_HTTP` |
| `tests/corpus/test_quick.py` | `tests/fadden_quick.rs` | `mock Runner` stage sequencing, omission validation, window overlap checks, partial `.building` rollback cleanup, and full end-to-end 9-Act `--features fake-http` build with `--in-force-on` dated compilations and SQLite index verification |
| Updated finalisation & golden suite | `tests/fadden_finalize.rs`, `tests/fadden_dist.rs`, `tests/fadden_golden.rs`, `tests/golden/finalize.cases.json`, `tests/data/dist-finalize/readme-sections.md` | Updated expected outputs for `shipped_tools.files == []`, no copied `check_current.py` / `corpus_paths.py` / `http_fetch.py` in `./corpus/`, and the updated `README.md` staleness section |

## Intended divergences

- **No external `curl` dependency in `download`, `retry13` or `quick`:** Python's `download.py` required external `curl >= 8.4.0` with `--proto =https --proto-redir =https`. Rust uses in-process `http::Net::download` over `rustls`, refusing non-`https://` URLs and non-HTTPS redirects directly, enforcing the `192 MiB` transfer cap (`MAX_DOWNLOAD_BYTES`) and `128 MiB` decoded EPUB cap (`MAX_EPUB_BYTES`), supporting `Range: bytes=<offset>-` + HTTP `206` resume on transient failures, and validating archives via `epub::valid_zip`.
- **No copied checker scripts in finalised corpora (`ROOT_TOOLS` retired):** Because `check_current` now lives inside the compiled `fadden` binary, `finalize.rs` (`ROOT_TOOLS` removed) no longer copies `check_current.py`, `corpus_paths.py` or `http_fetch.py` into `./corpus/`, records `shipped_tools.files: []` in `build/build-record.json`, and points `README.md` (`## Checking staleness`) at `python -m fadden check_current`. Correspondingly, `quick::SUPPORT` holds only `["pii_contact_allowlist.json"]` instead of copying those three `.py` helpers into `build/build/`.
- **Timestamp suffix:** `http::utc_text` writes `...Z` rather than `+00:00` in manifest `fetched_at` timestamps; `corpus_paths::version_date` and ISO parsers accept both identically.

## Quirks preserved

- `download` rejects missing `compilationRegisterId` whilst treating explicit `compilationRegisterId: null` as `no_epub` (`reason: "current_version_has_no_document"`), keeping any existing stale `.epub` and `.meta.json` untouched on disk.
- `download` uses pure-Python-compatible strict base64 validation (`decode_base64_strict`: accepts unmasked trailing bits such as `AB==`, rejects whitespace, newlines, unpadded tails or extra `=`) and detects authorisation refusal even when `isAuthorised` is explicitly `false`.
- `retry13` omits `fetched_at` in its 4-key `.epub.meta.json` sidecar (`versionStart`, `compilationNumber`, `compilationRegisterId`, `bytes`) and merges recoveries into `manifest_raw.json` in place whilst journaling all modified EPUBs, sidecars and manifests for rollback on later failures.
- `check_current` keeps the `1.5` second delay on empty `versions` responses (twice when the history fallback query is also empty) and uses banker's rounding (`fadden::round_half_even`) for percent summaries.
- `quick` refuses missing, non-empty or already-staged (`.building`) destinations, cleans empty `compilations/` directories cleanly, leaves pre-existing non-empty `--out` untouched on failure, and prints `"argument --in-force-on: <message>"` with exit status `2` on invalid dates.

## Retained Python modules (Constraint 2 & scope)

Two pipeline stages and their shared helpers remain in Python in `fadden/`:

- `fadden/capture_register.py` (`capture_register`) and `fadden/export_live_evidence_bundles.py` (`export_live_evidence_bundles`) remain in Python and were kept out of Wave 2 by scope and Constraint 2 (`export_live_evidence_bundles.py` relies on Windows transaction/file-lock APIs via `ctypes.WinDLL` / `msvcrt` with dedicated dual-runner Windows live-evidence CI).
- `fadden/corpus_paths.py` and `fadden/http_fetch.py` are shared library modules imported directly by `capture_register.py` and `export_live_evidence_bundles.py` and tested alongside them (`tests/corpus/test_http_fetch.py`, `tests/corpus/test_capture_register_paths.py`, `tests/corpus/test_live_evidence*.py`, `tests/corpus/test_output_security.py`), so both remain in `fadden/`.
