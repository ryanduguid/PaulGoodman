# Rust port notes

The `download`, `retry13` and `quick` stages now run in the Rust `fadden`
binary. `python -m fadden <stage>` runs the binary for them through
`RUST_STAGES` in `fadden/__main__.py`, with the same command lines, outputs,
files and exit statuses.

| Retired Python module | Rust replacement |
| --- | --- |
| `fadden/download.py` | `src/fadden/download.rs`, with the rollback journal in `download/snapshot.rs` and the JSON envelope decoder in `download/envelope.rs` |
| `fadden/retry13.py` | `src/fadden/retry13.rs` |
| `fadden/quick.py` | `src/fadden/quick.rs`, with `--in-force-on` compilations in `quick/dated.rs` |

## How parity is checked

`tests/golden/download.cases.json`, `retry13.cases.json` and
`quick.cases.json` hold fabricated cases run through the Python stages at
1913ba9, the last revision that had them, on Windows and on Linux. The Python
side answered each curl call from the same scripted responses the binary's
`fake-http` feature reads, so `tests/fadden_golden.rs` checks that the binary
exits as Python did, prints what Python printed and leaves every file with the
bytes Python wrote, the download log and request log included. The cases cover
fresh, cached, unpublished and envelope downloads, a resumed transfer, retries
after a server error and a missing body, invalid responses, rollback after a
failed title, retry recovery and its manifest patch, and `quick`'s argument and
destination checks.

`tests/fadden_download.rs`, `tests/fadden_retry13.rs` and
`tests/fadden_quick.rs` carry the retired Python unit tests' cases, including
the transfer limits, HTTPS-only redirects, rollback failures and a full 9-Act
`quick` build over scripted responses.

## Intended differences

- **No curl.** The binary downloads in process over HTTPS only, with the same
  192 MiB transfer and 128 MiB decoded EPUB limits, and resumes a truncated
  transfer with a `Range` request. It makes 3 attempts 5 seconds apart, where
  each curl attempt could also retry twice itself, and it bounds each read at
  90 seconds rather than a whole transfer at 600. A transport failure names
  the error (`URLError`, `IncompleteRead`) where Python gave curl's exit status.
- **`fetched_at` precision.** Download times are recorded to the second, as
  `2026-11-26T00:00:00+00:00`; Python's `isoformat()` added microseconds.
- **`quick`'s local stages.** They run as child processes of the same build.
  `python -m fadden quick` still picks that build as before (`FADDEN_BIN`, a
  checkout build, then `PATH`).

## What stays in Python

- `check_current.py`, with `corpus_paths.py` and `http_fetch.py`, which it
  imports. `finalize` copies all 3 to a finished corpus's root so anyone
  holding the corpus can check it for staleness with Python's standard library
  alone, without this repository or a Rust build.
- `capture_register.py` and `export_live_evidence_bundles.py`. The second uses
  Windows file locking and transactions through `ctypes`, and both have their
  own Windows live-evidence CI.
