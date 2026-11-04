// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: FSL-1.1-ALv2

#![no_main]
// The port of CPython's HTMLParser that reads legislation and ruling pages: no
// page may panic it, and handler calls arrive in source order.
use libfuzzer_sys::fuzz_target;
use tax_radar_au::htmlparser::check_positions;

fuzz_target!(|data: &[u8]| {
    if let Err(problem) = check_positions(data) {
        panic!("{problem}");
    }
});
