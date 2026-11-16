// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! `fadden rulings_targets`: a saved, fabricated feed in the shape of the ATO's
//! 'All Rulings and Determinations' feed, and no network traffic.
use std::path::Path;

use tax_radar_au::fadden::http::{Failure, Http, Request, Response};
use tax_radar_au::fadden::rulings::{self, MAX_TARGETS, TargetsError};
use tax_radar_au::fadden::rulings_targets::{self, Entry};
use tax_radar_au::fadden::{Exc, Run};

const FEED: &str = include_str!("fixtures/rulings-feed.rss");
const FEED_URL: &str = "https://www.ato.gov.au/law/view/rss?fileid=pbr_all.rss";

/// Gives every request the same answer, recording what was asked.
struct Feed {
    answer: Result<Response, Failure>,
    asked: Vec<String>,
}

impl Http for Feed {
    fn get(
        &mut self,
        url: &str,
        _limit: Option<usize>,
        _request: &mut Request,
    ) -> Result<Response, Failure> {
        self.asked.push(url.to_string());
        self.answer.clone()
    }
    fn sleep(&mut self, _seconds: f64) {}
    fn today(&mut self) -> String {
        "2099-01-01".into()
    }
}

/// The feed URL answering with `body` as `content_type`, from `url`.
fn serving(url: &str, content_type: &str, body: &str) -> Feed {
    Feed {
        answer: Ok(Response {
            url: url.into(),
            content_type: content_type.into(),
            body: body.as_bytes().to_vec(),
        }),
        asked: Vec::new(),
    }
}

fn offline() -> Feed {
    Feed {
        answer: Err(Failure::Transport("URLError".into())),
        asked: Vec::new(),
    }
}

/// The stage on `feed` (a path or URL), writing `TARGETS.json` in `dir`.
fn run(http: &mut Feed, feed: &str, dir: &Path) -> (Result<i32, Exc>, String) {
    let mut out = Run::default();
    let target = dir.join("TARGETS.json");
    let result = rulings_targets::run(http, feed, target.to_str().unwrap(), &mut out);
    (result, String::from_utf8(out.stdout).unwrap())
}

fn saved(dir: &Path, text: &str) -> String {
    let path = dir.join("feed.rss");
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_string()
}

fn written(dir: &Path) -> Vec<String> {
    match rulings::load_targets(dir.join("TARGETS.json").to_str().unwrap()) {
        Ok(targets) => targets,
        Err(TargetsError::Refused(error)) => panic!("rulings refused the list: {error}"),
        Err(TargetsError::Uncaught(error)) => panic!("rulings could not read the list: {error}"),
    }
}

fn item(docid: &str) -> String {
    format!(
        "<item><title><![CDATA[x]]></title>\
         <link>https://www.ato.gov.au/law/view/document?docid={docid}</link></item>\n"
    )
}

fn feed_of(items: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<rss version=\"2.0\"><channel>\n{items}</channel></rss>\n"
    )
}

#[test]
fn exactly_the_accepted_families_become_targets_in_feed_order() {
    let dir = tempfile::tempdir().unwrap();
    let (result, stdout) = run(&mut offline(), &saved(dir.path(), FEED), dir.path());
    // A malformed item is reported and makes the stage fail; the rest are still listed.
    assert_eq!(result.unwrap(), 1);
    assert_eq!(
        written(dir.path()),
        [
            "TXR/TR20991/NAT/ATO/00001",
            "TXR/TR20991A1/NAT/ATO/00001",
            "GSD/GSTD20991/NAT/ATO/00001",
            "CLR/CR209989/NAT/ATO/00001",
            "COG/LCR20991/NAT/ATO/00001",
            "TXD/TD20991/NAT/ATO/00001",
            "PSR/PSLA20991/NAT/ATO/00001",
        ]
    );
    let lines: Vec<&str> = stdout.lines().skip(1).collect();
    assert_eq!(
        lines,
        [
            "refused TDP/TDP20991/NAT/ATO/00001: not a family rulings accepts",
            "refused DTR/TR2099D1/NAT/ATO/00001: a draft; rulings takes final documents only",
            "refused txr/tr20991/NAT/ATO/00001: repeats target 1",
            "malformed item 9: it has 0 <link> tags, not one element",
            "malformed item 10: its link names \"TXD/TD20992/NAT/ATO/00001&print=1\", which is not a Legal Database docid",
            "refused DPC/PCG2099D1/NAT/ATO/00001: a draft; rulings takes final documents only",
            &format!(
                "wrote 7 target(s) to {}; 4 refused, 2 malformed",
                dir.path().join("TARGETS.json").display()
            ),
        ]
    );
    assert!(stdout.starts_with("feed "), "{stdout}");
    assert!(
        stdout.lines().next().unwrap().ends_with(": 13 item(s)"),
        "{stdout}"
    );
}

#[test]
fn a_feed_with_nothing_malformed_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let feed = feed_of(&(item("TXR/TR20991/NAT/ATO/00001") + &item("CLR/CR20991/NAT/ATO/00001")));
    let (result, stdout) = run(&mut offline(), &saved(dir.path(), &feed), dir.path());
    assert_eq!(result.unwrap(), 0, "{stdout}");
    assert_eq!(
        written(dir.path()),
        ["TXR/TR20991/NAT/ATO/00001", "CLR/CR20991/NAT/ATO/00001"]
    );
}

#[test]
fn the_entry_past_the_cap_is_refused() {
    let items: String = (1..=MAX_TARGETS + 1)
        .map(|n| item(&format!("TXR/TR2099{n}/NAT/ATO/00001")))
        .collect();
    let entries = rulings_targets::entries(&feed_of(&items)).unwrap();
    assert_eq!(entries.len(), 101);
    assert!(entries[..100].iter().all(|e| matches!(e, Entry::Target(_))));
    assert_eq!(
        entries[100],
        Entry::Refused {
            docid: "TXR/TR2099101/NAT/ATO/00001".into(),
            reason: "past the cap of 100 targets a run".into(),
        }
    );
    let dir = tempfile::tempdir().unwrap();
    let (result, _) = run(
        &mut offline(),
        &saved(dir.path(), &feed_of(&items)),
        dir.path(),
    );
    assert_eq!(result.unwrap(), 0);
    assert_eq!(written(dir.path()).len(), MAX_TARGETS);
}

#[test]
fn a_draft_and_an_unknown_family_are_refused_with_their_reasons() {
    let feed = feed_of(&(item("TXR/TR2099D3/NAT/ATO/00001") + &item("ZZZ/ZZ20991/NAT/ATO/00001")));
    assert_eq!(
        rulings_targets::entries(&feed).unwrap(),
        [
            Entry::Refused {
                docid: "TXR/TR2099D3/NAT/ATO/00001".into(),
                reason: "a draft; rulings takes final documents only".into(),
            },
            Entry::Refused {
                docid: "ZZZ/ZZ20991/NAT/ATO/00001".into(),
                reason: "not a family rulings accepts".into(),
            },
        ]
    );
}

#[test]
fn malformed_items_are_each_reported() {
    let cases = [
        (
            "<item><link>a</link><link>b</link></item>",
            "it has 4 <link> tags, not one element",
        ),
        (
            "<item><link/></item>",
            "it has 1 <link> tags, not one element",
        ),
        (
            "<item></link><link></item>",
            "its <link> element is not closed",
        ),
        (
            "<item><link>https://<b>x</b></link></item>",
            "its <link> holds markup",
        ),
        (
            "<item><link>https://x&nbsp;</link></item>",
            "its <link> holds an entity this stage does not read",
        ),
        (
            "<item><link>http://www.ato.gov.au/law/view/document?docid=TXR/TR20991/NAT/ATO/00001</link></item>",
            "its link \"http://www.ato.gov.au/law/view/document?docid=TXR/TR20991/NAT/ATO/00001\" is not a Legal Database document link",
        ),
    ];
    for (body, reason) in cases {
        assert_eq!(
            rulings_targets::entries(&feed_of(body)).unwrap(),
            [Entry::Malformed(format!("item 1: {reason}"))],
            "{body}"
        );
    }
}

#[test]
fn a_feed_whose_items_cannot_be_told_apart_writes_nothing() {
    let cases = [
        (
            "<rss><channel><item><item></item></channel></rss>",
            "its <item> tags do not pair up at byte 20",
        ),
        (
            "<rss><channel><item></channel></rss>",
            "its last <item> is not closed",
        ),
        (
            "<rss><channel><![CDATA[ </channel></rss>",
            "it has an unclosed CDATA section",
        ),
        ("<html><body></body></html>", "it is not an RSS feed"),
    ];
    for (feed, reason) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = saved(dir.path(), feed);
        let (result, stdout) = run(&mut offline(), &path, dir.path());
        let Err(Exc::Value { message, .. }) = result else {
            panic!("{feed} was read: {stdout}");
        };
        assert_eq!(message, format!("{path} cannot be read: {reason}"));
        assert!(!dir.path().join("TARGETS.json").exists());
    }
}

#[test]
fn an_existing_list_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TARGETS.json"), "[]").unwrap();
    let (result, _) = run(&mut offline(), &saved(dir.path(), FEED), dir.path());
    assert!(
        matches!(result, Err(Exc::Value { message, .. }) if message.ends_with("already exists; name a new file"))
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("TARGETS.json")).unwrap(),
        "[]"
    );
}

#[test]
fn the_feed_url_is_the_only_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut http = serving(FEED_URL, "application/rss+xml", FEED);
    let (result, stdout) = run(&mut http, FEED_URL, dir.path());
    assert_eq!(result.unwrap(), 1);
    assert_eq!(http.asked, [FEED_URL]);
    assert!(
        stdout.starts_with(&format!("feed {FEED_URL} sha256 ")),
        "{stdout}"
    );
    assert_eq!(written(dir.path()).len(), 7);
}

#[test]
fn a_url_that_is_not_a_legal_database_feed_is_not_fetched() {
    for url in [
        "https://example.com/law/view/rss?fileid=pbr_all.rss",
        "http://www.ato.gov.au/law/view/rss?fileid=pbr_all.rss",
        "https://www.ato.gov.au/law/view/document?docid=TXR/TR20991/NAT/ATO/00001",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut http = offline();
        let (result, _) = run(&mut http, url, dir.path());
        assert!(
            matches!(&result, Err(Exc::Value { message, .. }) if message.ends_with("is not a Legal Database feed at https://www.ato.gov.au/law/view/rss")),
            "{url}"
        );
        assert!(http.asked.is_empty());
    }
}

#[test]
fn a_feed_that_fails_redirects_or_is_not_xml_writes_nothing() {
    let failed = || Feed {
        answer: Err(Failure::Status(404)),
        asked: Vec::new(),
    };
    let cases = [
        (failed(), "fetching the feed failed (HTTP 404)".to_string()),
        (offline(), "fetching the feed failed (URLError)".to_string()),
        (
            serving(
                "https://example.com/law/view/rss",
                "application/rss+xml",
                FEED,
            ),
            "the feed redirected to https://example.com/law/view/rss".to_string(),
        ),
        (
            serving(FEED_URL, "text/html", FEED),
            "the feed came as \"text/html\", not XML".to_string(),
        ),
    ];
    for (mut http, reason) in cases {
        let dir = tempfile::tempdir().unwrap();
        let (result, _) = run(&mut http, FEED_URL, dir.path());
        assert!(
            matches!(&result, Err(Exc::Value { message, .. }) if *message == reason),
            "{reason}: {:?}",
            result.map_err(|e| e.to_string())
        );
        // A 404 is not retried; a transport failure is, within the shared budget.
        assert_eq!(
            http.asked.len(),
            if reason.contains("URLError") { 3 } else { 1 }
        );
        assert!(!dir.path().join("TARGETS.json").exists());
    }
}
