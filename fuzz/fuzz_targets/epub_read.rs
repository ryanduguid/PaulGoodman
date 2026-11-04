// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

#![no_main]
// The hand-written ZIP reader for downloaded EPUBs: arbitrary bytes are
// admitted or refused without a panic, and each member read back matches its
// declared length and CRC-32.
use libfuzzer_sys::fuzz_target;
use tax_radar_au::fadden::epub::check_read;

fuzz_target!(|data: &[u8]| {
    if let Err(problem) = check_read(data) {
        panic!("{problem}");
    }
});
