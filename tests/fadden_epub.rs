// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `epub`'s admission and member reads against hand-built ZIP archives: name
//! encodings, extra fields, the directory and member budgets, and the local
//! data each read checks.
use tax_radar_au::fadden::Exc;
use tax_radar_au::fadden::epub::{self, MAX_EPUB_BYTES, MAX_EPUB_MEMBERS};
use tax_radar_au::pyzlib::crc32;

/// One member as the central directory and its local header describe it.
#[derive(Clone)]
struct Entry {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    version: u8,
    /// The bytes stored after the local header.
    data: Vec<u8>,
    crc: u32,
    size: u32,
    csize: u32,
    offset: Option<u32>,
    extra: Vec<u8>,
}

fn stored(name: &str, plain: &[u8]) -> Entry {
    Entry {
        name: name.as_bytes().to_vec(),
        flags: 0,
        method: 0,
        version: 20,
        data: plain.to_vec(),
        crc: crc32(plain, 0),
        size: plain.len() as u32,
        csize: plain.len() as u32,
        offset: None,
        extra: Vec::new(),
    }
}

/// A raw deflate stream of one stored block holding `plain`.
fn deflate_stored(plain: &[u8]) -> Vec<u8> {
    let n = plain.len() as u16;
    let mut out = vec![0x01];
    out.extend(n.to_le_bytes());
    out.extend((!n).to_le_bytes());
    out.extend(plain);
    out
}

fn deflated(name: &str, plain: &[u8], data: Vec<u8>) -> Entry {
    Entry {
        method: 8,
        csize: data.len() as u32,
        data,
        ..stored(name, plain)
    }
}

fn extra_field(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut out = kind.to_le_bytes().to_vec();
    out.extend((body.len() as u16).to_le_bytes());
    out.extend(body);
    out
}

/// The archive's bytes; `prefix` comes before the end of central directory record.
fn build_with(entries: &[Entry], prefix: &[u8]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for e in entries {
        let offset = e.offset.unwrap_or(out.len() as u32);
        out.extend(b"PK\x03\x04");
        for v in [20u16, e.flags, e.method, 0, 33] {
            out.extend(v.to_le_bytes());
        }
        for v in [e.crc, e.csize, e.size] {
            out.extend(v.to_le_bytes());
        }
        for v in [e.name.len() as u16, 0] {
            out.extend(v.to_le_bytes());
        }
        out.extend(&e.name);
        out.extend(&e.data);
        central.extend(b"PK\x01\x02");
        central.extend([20, 3, e.version, 0]);
        for v in [e.flags, e.method, 0, 33] {
            central.extend(v.to_le_bytes());
        }
        for v in [e.crc, e.csize, e.size] {
            central.extend(v.to_le_bytes());
        }
        for v in [e.name.len() as u16, e.extra.len() as u16, 0, 0, 0] {
            central.extend(v.to_le_bytes());
        }
        for v in [0u32, offset] {
            central.extend(v.to_le_bytes());
        }
        central.extend(&e.name);
        central.extend(&e.extra);
    }
    let (start, size, count) = (out.len() as u32, central.len() as u32, entries.len() as u16);
    out.extend(central);
    out.extend(prefix);
    out.extend(b"PK\x05\x06");
    for v in [0u16, 0, count, count] {
        out.extend(v.to_le_bytes());
    }
    for v in [size + prefix.len() as u32, start] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out
}

fn build(entries: &[Entry]) -> Vec<u8> {
    build_with(entries, &[])
}

fn refusal(data: Vec<u8>) -> String {
    match epub::from_bytes(data) {
        Ok(_) => panic!("admitted"),
        Err(e) => e.to_string(),
    }
}

fn read_refusal(data: Vec<u8>, name: &str) -> String {
    let book = epub::from_bytes(data).unwrap();
    book.read(name).unwrap_err().to_string()
}

#[test]
fn names_decode_as_cp437_unless_flagged_utf8() {
    // 0x82 is CP437's 'é'.
    let mut cp437 = stored("", b"x");
    cp437.name = b"caf\x82.xhtml".to_vec();
    let book = epub::from_bytes(build(&[cp437])).unwrap();
    assert_eq!(book.namelist().collect::<Vec<_>>(), ["café.xhtml"]);
    assert_eq!(book.read("café.xhtml").unwrap(), b"x");

    let mut utf8 = stored("", b"x");
    utf8.name = b"\xff".to_vec();
    utf8.flags = 0x800;
    match epub::from_bytes(build(&[utf8])) {
        Err(Exc::Value { class, .. }) => assert_eq!(class, "UnicodeDecodeError"),
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn a_member_needing_a_later_zip_version_is_not_implemented() {
    let mut entry = stored("a", b"x");
    entry.version = 64;
    match epub::from_bytes(build(&[entry])) {
        Err(Exc::Other { class, message }) => {
            assert_eq!((class, message.as_str()), ("NotImplementedError", "zip file version 6.4"));
        }
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn zip64_extra_fields_supply_the_sizes_and_offset() {
    let mut entry = stored("a", b"hello");
    entry.size = 0xffff_ffff;
    entry.csize = 0xffff_ffff;
    entry.offset = Some(0xffff_ffff);
    let mut body = Vec::new();
    for v in [5u64, 5, 0] {
        body.extend(v.to_le_bytes());
    }
    entry.extra = [extra_field(0x5455, b"time"), extra_field(0x0001, &body)].concat();
    let book = epub::from_bytes(build(&[entry.clone()])).unwrap();
    let m = &book.members[0];
    assert_eq!((m.file_size, m.compress_size, m.header_offset), (5, 5, 0));
    assert_eq!(book.read("a").unwrap(), b"hello");

    for (fields, missing) in [(0, "File size"), (1, "Compress size"), (2, "Header offset")] {
        entry.extra = extra_field(0x0001, &body[..8 * fields]);
        assert_eq!(
            refusal(build(&[entry.clone()])),
            format!("Corrupt zip64 extra field. {missing} not found.")
        );
    }
    // An offset past what Python's file offsets hold.
    entry.extra = extra_field(0x0001, &[5u64, 5, 1 << 63].map(u64::to_le_bytes).concat());
    assert_eq!(
        read_refusal(build(&[entry.clone()]), "a"),
        "cannot fit 'int' into an offset-sized integer"
    );
    entry.extra = vec![1, 0, 16, 0, 0];
    assert_eq!(refusal(build(&[entry])), "Corrupt extra field 0001 (size=16)");
}

#[test]
fn a_unicode_path_extra_field_renames_the_member() {
    let mut entry = stored("plain.xhtml", b"x");
    let crc = crc32(b"plain.xhtml", 0);
    let path = |version: u8, crc: u32, name: &[u8]| {
        let mut body = vec![version];
        body.extend(crc.to_le_bytes());
        body.extend(name);
        extra_field(0x7075, &body)
    };
    entry.extra = path(1, crc, "ünï.xhtml".as_bytes());
    let book = epub::from_bytes(build(&[entry.clone()])).unwrap();
    assert_eq!(book.namelist().collect::<Vec<_>>(), ["ünï.xhtml"]);
    assert_eq!(book.members[0].orig_filename, "plain.xhtml");
    assert_eq!(book.read("ünï.xhtml").unwrap(), b"x");
    // A stale CRC, another version or an empty name leaves the name alone.
    for extra in [path(1, crc ^ 1, b"other"), path(2, crc, b"other"), path(1, crc, b"")] {
        entry.extra = extra;
        let book = epub::from_bytes(build(&[entry.clone()])).unwrap();
        assert_eq!(book.namelist().collect::<Vec<_>>(), ["plain.xhtml"]);
    }
    entry.extra = path(1, crc, b"\xff");
    assert_eq!(
        refusal(build(&[entry.clone()])),
        "Corrupt unicode path extra field (0x7075): invalid utf-8 bytes"
    );
    entry.extra = extra_field(0x7075, &[1, 0, 0]);
    assert_eq!(refusal(build(&[entry])), "Corrupt unicode path extra field (0x7075)");
}

#[test]
fn directories_outside_the_supported_shape_are_refused() {
    let one = [stored("a", b"x")];
    let locator = [b"PK\x06\x07".as_slice(), &[0; 16]].concat();
    assert_eq!(
        refusal(build_with(&one, &locator)),
        "EPUB uses an unsupported ZIP64 directory"
    );
    let mut other_disk = build(&one);
    let eocd = other_disk.len() - 22;
    other_disk[eocd + 4] = 1;
    assert_eq!(
        refusal(other_disk),
        "EPUB exceeds the supported ZIP directory budget"
    );
    // More directory entries than the record counts, past the member budget.
    let many: Vec<Entry> = (0..=MAX_EPUB_MEMBERS)
        .map(|i| stored(&format!("{i}"), b""))
        .collect();
    let mut over = build(&many);
    let eocd = over.len() - 22;
    let count = (MAX_EPUB_MEMBERS as u16).to_le_bytes();
    over[eocd + 8..eocd + 10].copy_from_slice(&count);
    over[eocd + 10..eocd + 12].copy_from_slice(&count);
    assert_eq!(refusal(over), "EPUB exceeds the member count budget");
}

#[test]
fn declared_sizes_past_the_budgets_are_refused() {
    let mut big = stored("a", b"x");
    big.size = 64 * 1024 * 1024 + 1;
    assert_eq!(
        refusal(build(&[big])),
        "EPUB exceeds the uncompressed member budget"
    );
    let total: Vec<Entry> = (0..5)
        .map(|i| Entry {
            size: 60 * 1024 * 1024,
            ..stored(&format!("{i}"), b"x")
        })
        .collect();
    assert_eq!(
        refusal(build(&total)),
        "EPUB exceeds the total uncompressed budget"
    );
    let too_long = vec![0u8; MAX_EPUB_BYTES as usize + 1];
    assert_eq!(
        refusal(too_long),
        "EPUB exceeds the compressed input budget"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.epub");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_EPUB_BYTES + 1)
        .unwrap();
    let err = epub::open(path.to_str().unwrap()).err().unwrap();
    assert_eq!(err.to_string(), "EPUB exceeds the compressed input budget");
    assert!(!epub::valid_zip(path.to_str().unwrap()));
}

#[test]
fn reads_check_the_member_against_its_local_data() {
    let book = epub::from_bytes(build(&[stored("a", b"hello")])).unwrap();
    assert_eq!(
        book.read("missing").unwrap_err().to_string(),
        "\"There is no item named 'missing' in the archive\""
    );
    let member = book.members[0].clone();
    assert_eq!(book.read_member(&member, true).unwrap(), b"hello");
    assert_eq!(book.read_member(&member, false).unwrap(), b"");

    // A compressed size past the input budget, before the local header is read.
    let mut huge = stored("a", b"hello");
    huge.csize = 0xffff_fffe;
    assert_eq!(
        read_refusal(build(&[huge]), "a"),
        "EPUB member is outside the supported byte budget"
    );
    // A header offset past the end of the archive.
    let (a, mut b) = (stored("a", b"x"), stored("b", b"y"));
    b.offset = Some(0x7fff_0000);
    assert_eq!(
        read_refusal(build(&[a.clone(), b.clone()]), "b"),
        "EPUB member has an invalid local header"
    );
    // The next member's offset is past the end, so a long member runs out.
    let mut long = a;
    long.size = 10_000;
    long.csize = 10_000;
    assert_eq!(
        read_refusal(build(&[long, b]), "a"),
        "EPUB member data is truncated"
    );
}

#[test]
fn deflated_members_must_end_where_their_sizes_say() {
    let ok = deflated("a", b"hello", deflate_stored(b"hello"));
    assert_eq!(epub::from_bytes(build(&[ok])).unwrap().read("a").unwrap(), b"hello");
    let mut short = deflated("a", b"hello", deflate_stored(b"hello"));
    short.size = 3;
    assert_eq!(
        read_refusal(build(&[short]), "a"),
        "EPUB member exceeds its declared uncompressed length"
    );
    let trailing = deflated("a", b"hello", [deflate_stored(b"hello"), b"junk".to_vec()].concat());
    assert_eq!(
        read_refusal(build(&[trailing]), "a"),
        "EPUB member has trailing compressed data"
    );
    let invalid = deflated("a", b"hello", vec![0x07, 0, 0]);
    assert_eq!(
        read_refusal(build(&[invalid]), "a"),
        "EPUB member has invalid deflate data"
    );
}

#[test]
fn the_fuzz_invariant_skips_members_that_fail_to_read() {
    let mut bad_crc = stored("a", b"hello");
    bad_crc.crc ^= 1;
    let data = build(&[bad_crc, stored("b", b"ok")]);
    assert!(!epub::valid_zip_bytes(data.clone()));
    assert_eq!(epub::check_read(&data), Ok(()));
    assert_eq!(epub::check_read(b"not a zip"), Ok(()));
}
