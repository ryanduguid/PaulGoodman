// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: FSL-1.1-ALv2

#![no_main]
// The raw-deflate inflater that reads EPUB members, against miniz_oxide: both
// must agree on whether a stream is complete and on its bytes. Decoding in
// small output steps, as the EPUB reader's loop does, must match decoding at
// once.
use libfuzzer_sys::fuzz_target;
use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_with_limit};
use tax_radar_au::pyzlib::Decompress;

/// Above deflate's 1032:1 bound for the 4 KiB inputs the workflow passes.
const LIMIT: usize = 1 << 23;
const STEP: usize = 7;

fuzz_target!(|data: &[u8]| {
    let mut whole = Decompress::default();
    let ours = whole.decompress(data, LIMIT);
    match (&ours, decompress_to_vec_with_limit(data, LIMIT)) {
        (_, Err(e)) if e.status == TINFLStatus::HasMoreOutput => return,
        (Ok(out), Ok(expected)) => {
            assert!(whole.eof, "miniz_oxide finished a stream pyzlib did not");
            assert!(
                *out == expected,
                "pyzlib and miniz_oxide decoded different bytes"
            );
        }
        (Ok(_), Err(_)) => assert!(!whole.eof, "pyzlib finished a stream miniz_oxide refused"),
        (Err(_), Ok(_)) => panic!("pyzlib refused a stream miniz_oxide decoded"),
        (Err(_), Err(_)) => {}
    }

    let mut step = Decompress::default();
    let mut streamed = Vec::new();
    let mut input = data.to_vec();
    let refused = loop {
        let Ok(piece) = step.decompress(&input, STEP) else {
            break true;
        };
        streamed.extend_from_slice(&piece);
        input = step.unconsumed_tail.clone();
        if step.eof || (input.is_empty() && piece.len() < STEP) {
            break false;
        }
    };
    match ours {
        Err(_) => assert!(refused, "stepped decoding accepted a refused stream"),
        Ok(out) => {
            assert!(!refused, "stepped decoding refused an accepted stream");
            assert!(step.eof == whole.eof, "stepped decoding ended differently");
            assert!(streamed == out, "stepped decoding gave different bytes");
            assert!(
                step.unused_data == whole.unused_data,
                "stepped decoding kept different trailing data"
            );
        }
    }
});
