// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

#![no_main]
// The hand-written ZIP reader for downloaded EPUBs: arbitrary bytes are
// admitted or refused without a panic, and each member read back matches its
// declared length and CRC-32.
use libfuzzer_sys::fuzz_target;
use tax_radar_au::fadden::epub;
use tax_radar_au::pyzlib::crc32;

fuzz_target!(|data: &[u8]| {
    let Ok(book) = epub::from_bytes(data.to_vec()) else {
        return;
    };
    for info in &book.members {
        if let Ok(content) = book.read(&info.filename) {
            let read = (content.len() as u64, crc32(&content, 0));
            assert_eq!(read, (info.file_size, info.crc), "{:?}", info.filename);
        }
    }
});
