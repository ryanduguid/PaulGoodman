// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden check_current`: ported from `CheckCurrentTests` (`tests/corpus/test_pipeline_paths.py`),
//! `StalenessBucketTests`, `CheckCurrentStatusTests`, `PathBoundaryTests::test_check_current...`
//! (`tests/corpus/test_regressions.py`), and `tests/corpus/test_output_security.py`.
use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};
use tax_radar_au::fadden::http::{Failure, Http, Request, Response};
use tax_radar_au::fadden::{Exc, Run, check_current};

#[derive(Default)]
struct CheckHttp {
    answers: BTreeMap<String, Vec<Option<Vec<Value>>>>,
    events: Vec<String>,
}

impl Http for CheckHttp {
    fn get(
        &mut self,
        url: &str,
        _limit: Option<usize>,
        _request: &mut Request,
    ) -> Result<Response, Failure> {
        self.events.push(format!("GET {url}"));
        let marker = "titleId%20eq%20%27";
        let rid = url
            .find(marker)
            .map(|at| {
                let rest = &url[at + marker.len()..];
                rest.split("%27").next().unwrap_or("")
            })
            .unwrap_or("");
        let queue = self.answers.get_mut(rid);
        let next = queue.and_then(|q| (!q.is_empty()).then(|| q.remove(0)));
        match next {
            None | Some(None) => Err(Failure::Status(503)),
            Some(Some(rows)) => {
                let body = json!({
                    "value": rows.into_iter().map(|mut r| {
                        if r.get("titleId").is_none() {
                            r.as_object_mut().unwrap().insert("titleId".into(), json!(rid));
                        }
                        r
                    }).collect::<Vec<_>>()
                })
                .to_string();
                Ok(Response {
                    url: url.into(),
                    content_type: "application/json".into(),
                    body: body.into_bytes(),
                })
            }
        }
    }

    fn sleep(&mut self, seconds: f64) {
        self.events.push(format!("sleep {seconds}"));
    }

    fn today(&mut self) -> String {
        "2099-02-01".into()
    }
}

fn title(number: usize, collection: &str) -> Value {
    json!({
        "register_id": format!("C2099A{number:05}"),
        "name": format!("Synthetic Act {number}"),
        "collection": collection,
        "compilation_number": "1",
        "compilation_date": "2099-01-01",
    })
}

fn run_root(
    titles: Vec<Value>,
    answers: BTreeMap<String, Vec<Option<Vec<Value>>>>,
    want: Option<&str>,
) -> (Result<(), Exc>, String, CheckHttp) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("sources.json"),
        json!({"retrieved": "2099-06-01", "titles": titles}).to_string(),
    )
    .unwrap();
    let mut http = CheckHttp {
        answers,
        events: Vec::new(),
    };
    let mut out = Run::default();
    let outcome = check_current::run_root(&mut http, &dir.path().to_string_lossy(), want, &mut out);
    (
        outcome,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        http,
    )
}

#[test]
fn every_outcome_is_reported_in_its_own_bucket() {
    let unchanged = json!({
        "start": "2099-01-01T00:00:00",
        "compilationNumber": "1",
        "registerId": "C2099C00001",
        "status": "InForce",
    });
    let titles: Vec<Value> = (1..=25).map(|n| title(n, "Act")).collect();
    let mut answers: BTreeMap<String, Vec<Option<Vec<Value>>>> = titles
        .iter()
        .map(|t| {
            (
                t["register_id"].as_str().unwrap().to_string(),
                vec![Some(vec![unchanged.clone()])],
            )
        })
        .collect();
    // 1: lookup fails -> 3 retry attempts
    answers.insert("C2099A00001".into(), vec![None, None, None]);
    // 2: empty current, history returns row -> no longer in force
    answers.insert(
        "C2099A00002".into(),
        vec![Some(vec![]), Some(vec![json!({"start": "2098-01-01"})])],
    );
    // 3: empty current, history fails -> lookup failed
    answers.insert("C2099A00003".into(), vec![Some(vec![]), None, None, None]);
    // 4: empty current, empty history -> lookup failed
    answers.insert("C2099A00004".into(), vec![Some(vec![]), Some(vec![])]);
    // 5: invalid date -> lookup failed
    let mut bad_date = unchanged.clone();
    bad_date["start"] = json!("2099-02-30");
    answers.insert("C2099A00005".into(), vec![Some(vec![bad_date])]);
    // 6: newer compilation published -> superseded
    let mut newer = unchanged.clone();
    newer["compilationNumber"] = json!("2");
    newer["registerId"] = json!("C2099C00002");
    answers.insert("C2099A00006".into(), vec![Some(vec![newer])]);

    let (res, stdout, http) = run_root(titles, answers, None);
    res.unwrap();
    assert!(stdout.contains("Roughly 1 minutes."), "{stdout}");
    assert!(stdout.contains("  checked 25/25"), "{stdout}");
    assert!(
        stdout.contains(
            "unchanged: 19   superseded: 1   no compilation published: 0   no longer in force: 1   lookup failed: 4"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("SUPERSEDED, re-download these:\n  C2099A00006  comp 1 (2099-01-01)"),
        "{stdout}"
    );
    assert!(stdout.contains("NO LONGER IN FORCE"), "{stdout}");
    assert!(stdout.contains("LOOKUP FAILED:"), "{stdout}");
    assert!(
        http.events[0].contains("compilationNumber,registerId,status"),
        "{}",
        http.events[0]
    );
}

#[test]
fn repealed_and_unpublished_status_buckets_are_distinct() {
    let ftitle = json!({
        "register_id": "F2016L01256",
        "name": "Fabricated repealed instrument",
        "collection": "LegislativeInstrument",
        "compilation_number": "1",
        "compilation_date": "2016-08-02",
    });

    // Repealed row with no registerId goes to NO LONGER IN FORCE, not NO COMPILATION PUBLISHED.
    let mut ans1 = BTreeMap::new();
    ans1.insert(
        "F2016L01256".into(),
        vec![Some(vec![json!({
            "titleId": "F2016L01256",
            "start": "2026-08-12T00:00:00Z",
            "compilationNumber": null,
            "registerId": null,
            "isCurrent": true,
            "status": "Repealed",
        })])],
    );
    let (res, stdout, _) = run_root(vec![ftitle.clone()], ans1, None);
    res.unwrap();
    assert!(stdout.contains("no longer in force: 1"), "{stdout}");
    assert!(stdout.contains("no compilation published: 0"), "{stdout}");
    assert!(stdout.contains("NO LONGER IN FORCE"), "{stdout}");
    assert!(!stdout.contains("NO COMPILATION PUBLISHED"), "{stdout}");

    // InForce row with registerId=null goes to NO COMPILATION PUBLISHED, not SUPERSEDED.
    let mut ans2 = BTreeMap::new();
    ans2.insert(
        "F2016L01256".into(),
        vec![Some(vec![json!({
            "titleId": "F2016L01256",
            "start": "2026-08-12T00:00:00Z",
            "compilationNumber": "2",
            "registerId": null,
            "isCurrent": true,
            "status": "InForce",
        })])],
    );
    let (res, stdout, _) = run_root(vec![ftitle.clone()], ans2, None);
    res.unwrap();
    assert!(stdout.contains("no compilation published: 1"), "{stdout}");
    assert!(stdout.contains("NO COMPILATION PUBLISHED"), "{stdout}");
    assert!(
        !stdout.contains("SUPERSEDED, re-download these"),
        "{stdout}"
    );
    assert!(stdout.contains("superseded: 0"), "{stdout}");

    // Wrong titleId row is a lookup failure (not no longer in force).
    let mut ans3 = BTreeMap::new();
    ans3.insert(
        "F2016L01256".into(),
        vec![Some(vec![json!({
            "titleId": "F9999L99999",
            "start": "2026-07-01T00:00:00Z",
            "compilationNumber": "5",
            "registerId": "F2026C00099",
        })])],
    );
    let (res, stdout, _) = run_root(vec![ftitle], ans3, None);
    res.unwrap();
    assert!(stdout.contains("no longer in force: 0"), "{stdout}");
    assert!(stdout.contains("lookup failed: 1"), "{stdout}");
}

#[test]
fn collection_filter_and_empty_collection_report_cleanly() {
    let titles = vec![title(1, "Act"), title(2, "LegislativeInstrument")];
    let mut ans = BTreeMap::new();
    ans.insert(
        "C2099A00001".into(),
        vec![Some(vec![json!({
            "start": "2099-01-01T00:00:00",
            "compilationNumber": "1",
            "registerId": "C2099C00001",
            "status": "InForce",
        })])],
    );
    let (res, stdout, http) = run_root(titles.clone(), ans, Some("Act"));
    res.unwrap();
    assert!(
        stdout.contains("Checking 1 titles against the Register"),
        "{stdout}"
    );
    assert!(stdout.contains("unchanged: 1"), "{stdout}");
    assert_eq!(
        http.events.iter().filter(|e| e.starts_with("GET")).count(),
        1
    );

    let (res, stdout, http) = run_root(titles, BTreeMap::new(), Some("NotifiableInstrument"));
    res.unwrap();
    assert_eq!(stdout, "No titles in collection 'NotifiableInstrument'.\n");
    assert!(http.events.is_empty());
}

#[test]
fn filter_injection_and_installed_layout_are_refused_before_requests() {
    let err = run_root(
        vec![json!({"register_id": "C2099A00001' or isCurrent eq true"})],
        BTreeMap::new(),
        None,
    )
    .0
    .unwrap_err();
    assert_eq!(err.class(), "ValueError");

    // Installed layout (not a checkout or deployed root with sources.json) fails closed.
    let dir = tempfile::tempdir().unwrap();
    let fadden = dir.path().join("fadden");
    std::fs::create_dir(&fadden).unwrap();
    let mut http = CheckHttp::default();
    let mut out = Run::default();
    let err = check_current::run(&mut http, &fadden.to_string_lossy(), None, &mut out).unwrap_err();
    assert_eq!(err.class(), "RuntimeError");
    assert!(err.to_string().contains("source checkout"), "{err}");
}

#[test]
fn root_selection_prefers_local_sources_json_then_corpus_root() {
    let dir = tempfile::tempdir().unwrap();
    // Deployed root: sources.json beside module dir.
    let deployed = dir.path().join("deployed");
    std::fs::create_dir(&deployed).unwrap();
    std::fs::write(
        deployed.join("sources.json"),
        json!({"retrieved": "2099-01-01", "titles": [title(1, "Act")]}).to_string(),
    )
    .unwrap();
    let mut ans = BTreeMap::new();
    ans.insert(
        "C2099A00001".into(),
        vec![Some(vec![json!({
            "start": "2099-01-01T00:00:00",
            "compilationNumber": "1",
            "registerId": "C2099C00001",
            "status": "InForce",
        })])],
    );
    let mut http = CheckHttp {
        answers: ans.clone(),
        events: Vec::new(),
    };
    let mut out = Run::default();
    check_current::run(&mut http, &deployed.to_string_lossy(), None, &mut out).unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("unchanged: 1"));

    // Checkout root: <tmp>/proj/fadden reads <tmp>/proj/corpus/sources.json.
    let checkout_proj = dir.path().join("proj");
    let scratch = checkout_proj.join("fadden");
    let corpus = checkout_proj.join("corpus");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(checkout_proj.join("pyproject.toml"), "x\n").unwrap();
    std::fs::write(checkout_proj.join("VERSION"), "x\n").unwrap();
    std::fs::write(
        corpus.join("sources.json"),
        json!({"retrieved": "2099-01-01", "titles": [title(1, "Act")]}).to_string(),
    )
    .unwrap();
    let mut http = CheckHttp {
        answers: ans,
        events: Vec::new(),
    };
    let mut out = Run::default();
    check_current::run(&mut http, &scratch.to_string_lossy(), None, &mut out).unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("unchanged: 1"));
    let _ = PathBuf::new();
}

#[cfg(feature = "fake-http")]
#[test]
fn cli_binary_checks_current_titles_over_fake_http() {
    let dir = tempfile::tempdir().unwrap();
    let deployed = dir.path().join("corpus");
    std::fs::create_dir(&deployed).unwrap();
    std::fs::write(
        deployed.join("sources.json"),
        json!({
            "retrieved": "2099-06-01",
            "titles": [{
                "register_id": "C2099A00001",
                "name": "Demo Act",
                "collection": "Act",
                "compilation_number": "1",
                "compilation_date": "2099-01-01",
            }]
        })
        .to_string(),
    )
    .unwrap();
    let script = dir.path().join("script.json");
    let log = dir.path().join("http.log");
    std::fs::write(
        &script,
        json!({
            "today": "2099-06-02",
            "responses": [{
                "status": 200,
                "type": "application/json",
                "body": json!({"value": [{
                    "titleId": "C2099A00001",
                    "start": "2099-01-01T00:00:00Z",
                    "compilationNumber": "1",
                    "registerId": "C2099C00001",
                    "status": "InForce",
                }]}).to_string(),
            }]
        })
        .to_string(),
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fadden"))
        .args(["check_current", "Act"])
        .env("FADDEN_MODULE_DIR", &deployed)
        .env("FADDEN_FAKE_HTTP", &script)
        .env("FADDEN_FAKE_HTTP_LOG", &log)
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("unchanged: 1   superseded: 0"), "{stdout}");
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("sleep 1.5"), "{logged}");
}
