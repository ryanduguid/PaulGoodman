// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden`'s own command line: help, a missing or unknown stage, and an
//! argument that is not valid Unicode.
use std::process::{Command, Output};

fn fadden(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fadden"))
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .replace("\r\n", "\n")
}

#[test]
fn help_prints_the_usage_and_exits_0() {
    for flag in ["-h", "--help"] {
        let out = fadden(&[flag, "ignored"]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let stdout = text(&out.stdout);
        assert!(
            stdout.starts_with("usage: fadden {as_at,authorities,"),
            "{stdout}"
        );
        assert!(
            stdout.ends_with(
                "\n\nRun one ported corpus-builder stage; `python -m fadden` forwards these stages here.\n"
            ),
            "{stdout}"
        );
        assert!(out.stderr.is_empty());
    }
}

#[test]
fn a_missing_or_unknown_stage_names_the_stages_and_exits_2() {
    for args in [&[][..], &["nonsense", "--help"][..]] {
        let out = fadden(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = text(&out.stderr);
        assert!(stderr.starts_with("usage: fadden {"), "{stderr}");
        assert!(
            stderr.ends_with(
                "\nfadden: error: choose a stage: as_at, authorities, capture_register, \
                 compare_builds, discover, dist, dist_verify, download, \
                 export_live_evidence_bundles, export_monitor_contract, \
                 export_publication_bundles, extract, finalize, pii_scan, pii_scan2, probe13, \
                 quick, rates, retry13, rulings, rulings_targets, search, state, versions, witness\n"
            ),
            "{stderr}"
        );
        assert!(out.stdout.is_empty());
    }
}

/// Both binaries refuse an argument that is not valid Unicode before running.
#[cfg(unix)]
#[test]
fn an_argument_that_is_not_unicode_is_refused_with_status_2() {
    use std::os::unix::ffi::OsStrExt;
    let bad = std::ffi::OsStr::from_bytes(b"caf\xe9");
    for (binary, message) in [
        (
            env!("CARGO_BIN_EXE_fadden"),
            "fadden: every argument must be valid Unicode.\n",
        ),
        (
            env!("CARGO_BIN_EXE_tax-radar-au"),
            "tax-radar-au: blocked: every argument must be valid Unicode.\n",
        ),
    ] {
        let out = Command::new(binary)
            .arg("search")
            .arg(bad)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{binary}");
        assert_eq!(text(&out.stderr), message);
        assert!(out.stdout.is_empty());
    }
}
