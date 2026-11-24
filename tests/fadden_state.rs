// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden state` and state corpora in `fadden search`. The Queensland fixture
//! is an excerpt of a register title (attribution in its closing comment and in
//! NOTICE); the other XML is fabricated. No test reaches the network.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use tax_radar_au::fadden::http::{Failure, Http, Request, Response};
use tax_radar_au::fadden::state::{self, Robots, Source};
use tax_radar_au::fadden::{Run, search};
use tax_radar_au::pypath::PyPath;

const EXCERPT: &str = include_str!("fixtures/qld-sl-2021-0019-excerpt.xml");
const REG: &str = "sl-2021-0019";
const REG_NAME: &str = "Land Tax Regulation 2021";
const ROBOTS: &str = "https://www.legislation.qld.gov.au/robots.txt";

/// Queensland, reading only the fixture's title.
const ONE_TITLE: Source = Source {
    titles: &[(REG, REG_NAME)],
    ..state::SOURCES[0]
};

/// Answers from a map of URL to body (or status), recording requests and waits.
#[derive(Default)]
struct Register {
    answers: BTreeMap<String, Result<String, u16>>,
    asked: Vec<String>,
    slept: Vec<f64>,
}

impl Register {
    fn new(robots: Result<&str, u16>, titles: &[(&str, &str)]) -> Register {
        let mut answers = BTreeMap::new();
        answers.insert(ROBOTS.to_string(), robots.map(str::to_string));
        for (id, xml) in titles {
            answers.insert(state::SOURCES[0].xml_url(id), Ok((*xml).to_string()));
        }
        Register {
            answers,
            ..Register::default()
        }
    }
}

impl Http for Register {
    fn get(
        &mut self,
        url: &str,
        _limit: Option<usize>,
        _request: &mut Request,
    ) -> Result<Response, Failure> {
        self.asked.push(url.to_string());
        match self.answers.get(url) {
            Some(Ok(body)) => Ok(Response {
                url: url.into(),
                content_type: "application/xml".into(),
                body: body.as_bytes().to_vec(),
            }),
            Some(Err(code)) => Err(Failure::Status(*code)),
            None => Err(Failure::Status(404)),
        }
    }
    fn sleep(&mut self, seconds: f64) {
        self.slept.push(seconds);
    }
    fn today(&mut self) -> String {
        "2099-01-02".into()
    }
}

fn run(http: &mut Register, source: &Source, out: &Path) -> Result<i32, String> {
    let mut printed = Run::default();
    state::run(
        http,
        source,
        &PyPath::new(&out.display().to_string()),
        &mut printed,
    )
    .map_err(|e| e.to_string())
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn the_excerpt_parses_to_section_rows_with_the_registers_as_at_date() {
    let parsed = state::parse(EXCERPT, REG, REG_NAME).unwrap();
    assert_eq!(parsed.name, REG_NAME);
    assert_eq!(parsed.collection, "SubordinateLegislation");
    assert_eq!(parsed.as_at, "2023-07-01");
    assert_eq!(parsed.in_force, "allinforce");
    let sections: Vec<_> = parsed
        .rows
        .iter()
        .map(|r| r.section.clone().unwrap())
        .collect();
    assert_eq!(sections, ["1", "2", "5"]);
    let first = &parsed.rows[0];
    assert_eq!(first.heading, "1 Short title");
    assert_eq!(
        first.text,
        "This regulation may be cited as the Land Tax Regulation 2021."
    );
    assert_eq!(first.anchor.as_deref(), Some("sec.1"));
    let second = &parsed.rows[1];
    assert_eq!(second.heading, "2 Primary production activities—Act, s 53");
    assert!(second.text.starts_with("For section 53"), "{}", second.text);
    // A list item's number stays on its own line with its text.
    assert!(
        second.text.lines().any(|l| l.starts_with("- (a) ")),
        "{}",
        second.text
    );
    assert_eq!(
        parsed.rows[2].text,
        "The Land Tax Regulation 2010, SL No. 109 is repealed."
    );
    let disclaimer = parsed.disclaimer.unwrap();
    assert!(disclaimer.starts_with("Queensland does not accept responsibility"));
    assert!(disclaimer.ends_with("This notice must not be erased."));
}

const STRUCTURED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<act title="Sample Duties Act 2099" id="act-2099-001" first.valid.date="2099-07-01T00:00:00+10:00" in.force="allinforce">
<coverdata><subtitle>Cover text</subtitle></coverdata>
<wrapper><front><shorttitle>Sample Duties Act 2099</shorttitle></front><body>
<chapter id="ch.1"><no>Chapter 1</no><heading>Preliminary</heading>
<part id="ch.1-pt.1"><no>Part 1</no><heading>Liability</heading>
<division id="ch.1-pt.1-div.2"><no>Division 2</no><heading>Rates</heading>
<clause id="sec.8"><no>8</no><heading>Rate of duty</heading>
<subclause id="sec.8-ssec.1"><no>(1)</no><block><txt>Duty is payable at the rate—</txt>
<list><li><no>(a)</no><block><txt>in table 1; or</txt></block></li><li><no>(b)</no><block><txt>set by regulation.</txt></block></li></list></block></subclause>
<subclause id="sec.8-ssec.2"><no>(2)</no><block><txt>See <defterm>dutiable value</defterm>&#160;in section&#160;9.</txt></block></subclause>
<historynote><txt>s 8 amd 2099 No. 2</txt></historynote>
</clause></division></part></chapter>
</body></wrapper>
<schedules><schedule id="sch.1"><no>Schedule 1</no><heading>Rates</heading>
<block><table><tgroup cols="2"><colspec colname="1"/><tbody>
<row><entry><block><txt>Value</txt></block><block><txt>up to $10</txt></block></entry><entry><block><txt>nil</txt></block></entry></row>
<row><entry><block><txt>more</txt></block></entry><entry><block><txt>$1</txt></block></entry></row>
</tbody></tgroup></table></block></schedule></schedules>
</act>"#;

#[test]
fn structure_becomes_containers_headings_and_joined_text() {
    let parsed = state::parse(STRUCTURED, "act-2099-001", "Sample Duties Act 2099").unwrap();
    assert_eq!(parsed.collection, "Act");
    assert_eq!(parsed.as_at, "2099-07-01");
    assert_eq!(parsed.disclaimer, None);
    let section = &parsed.rows[0];
    assert_eq!(section.section.as_deref(), Some("8"));
    assert_eq!(section.heading, "8 Rate of duty");
    assert_eq!(section.container.as_deref(), Some("Division 2 Rates"));
    assert_eq!(
        section.text,
        "(1) Duty is payable at the rate—\n\n- (a) in table 1; or\n\n- (b) set by regulation.\n\n\
         (2) See dutiable value in section 9.\n\ns 8 amd 2099 No. 2"
    );
    let schedule = &parsed.rows[1];
    assert_eq!(schedule.kind, "container");
    assert_eq!(schedule.section, None);
    assert_eq!(schedule.heading, "Schedule 1 Rates");
    assert_eq!(schedule.anchor.as_deref(), Some("sch.1"));
    assert_eq!(schedule.text, "Value up to $10 | nil\n\nmore | $1");
    assert_eq!(parsed.rows.len(), 2);
    let source = &state::SOURCES[0];
    let markdown = state::markdown(source, &parsed, "2099-01-02");
    let headings: Vec<&str> = markdown.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(
        headings,
        [
            "# Sample Duties Act 2099",
            "## Chapter 1 Preliminary",
            "### Part 1 Liability",
            "#### Division 2 Rates",
            "##### 8 Rate of duty",
            "## Schedule 1 Rates",
        ]
    );
    assert!(!markdown.contains("Cover text"));
}

#[test]
fn a_title_the_list_does_not_expect_is_refused() {
    let wrong_name = state::parse(EXCERPT, REG, "Land Tax Regulation 2010").unwrap_err();
    assert!(
        wrong_name.to_string().contains("review the list"),
        "{wrong_name}"
    );
    let wrong_id = state::parse(EXCERPT, "sl-2021-0020", REG_NAME).unwrap_err();
    assert!(
        wrong_id.to_string().contains("the XML is title"),
        "{wrong_id}"
    );
    let undated = STRUCTURED.replace("2099-07-01T", "soon");
    let undated = state::parse(&undated, "act-2099-001", "Sample Duties Act 2099").unwrap_err();
    assert!(undated.to_string().contains("no as-at date"), "{undated}");
}

#[test]
fn robots_rules_follow_the_longest_match_and_the_agents_own_group() {
    let robots = Robots::parse(
        "User-agent: *\nDisallow: /admin/\nDisallow: /view/pdf/\nAllow: /view/pdf/ok$\n\
         Disallow: /x*y\nCrawl-delay: 2\n\nUser-agent: GPTBot\nDisallow: /\n",
    );
    assert!(robots.allows("/view/whole/xml/inforce/current/act-2010-015"));
    assert!(!robots.allows("/admin/"));
    assert!(!robots.allows("/view/pdf/act"));
    assert!(robots.allows("/view/pdf/ok"));
    assert!(!robots.allows("/x/abc/y"));
    assert_eq!(robots.crawl_delay, Some(2.0));
    let named = Robots::parse(
        "User-agent: *\nDisallow: /\n\nUser-agent: au-legislation-corpus\nDisallow: /secret\n",
    );
    assert!(named.allows("/view/whole/xml/inforce/current/x"));
    assert!(!named.allows("/secret/x"));
    assert!(Robots::parse("").allows("/anything"));
}

#[test]
fn the_stage_writes_a_corpus_that_search_build_reads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("qld");
    let mut register = Register::new(Ok("User-agent: *\nCrawl-delay: 9\n"), &[(REG, EXCERPT)]);
    assert_eq!(run(&mut register, &ONE_TITLE, &root), Ok(0));
    assert_eq!(
        register.asked,
        [ROBOTS.to_string(), state::SOURCES[0].xml_url(REG)]
    );
    // One request at a time, after the crawl delay robots.txt asks for.
    assert_eq!(register.slept, [9.0]);
    assert_eq!(
        std::fs::read(root.join(format!("raw/{REG}.xml"))).unwrap(),
        EXCERPT.as_bytes()
    );
    let sources: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("sources.json")).unwrap()).unwrap();
    assert_eq!(sources["source"], "qld");
    assert_eq!(sources["user_agent"], "au-legislation-corpus/1.0");
    assert_eq!(
        sources["attribution"],
        "Based on content from the Queensland Legislation website at 2099-01-02. For the latest \
         information on Queensland Government legislation please go to \
         https://www.legislation.qld.gov.au."
    );
    let title = &sources["titles"][0];
    assert_eq!(title["register_id"], REG);
    assert_eq!(title["compilation_date"], "2023-07-01");
    assert_eq!(title["jsonl_rows"], 3);
    assert!(
        title["disclaimer"]
            .as_str()
            .unwrap()
            .ends_with("This notice must not be erased.")
    );
    let rows = lines(&root.join(format!("markdown/{REG}/sections.jsonl")));
    assert_eq!(rows[1]["row_id"], format!("{REG}:0002:2"));
    assert_eq!(
        rows[1]["register_page"],
        format!("https://www.legislation.qld.gov.au/view/whole/html/inforce/current/{REG}#sec.2")
    );
    assert_eq!(rows[1]["authorised"], false);
    assert_eq!(rows[1]["licence"], "CC BY 4.0");
    let markdown = std::fs::read_to_string(root.join(format!("markdown/{REG}/{REG}.md"))).unwrap();
    assert!(markdown.contains("This notice must not be erased."));
    assert!(markdown.contains("attribution: \"Based on content from the Queensland Legislation"));

    let index = dir.path().join("qld.sqlite");
    let counts = search::build(&root.display().to_string(), &index.display().to_string()).unwrap();
    assert_eq!(counts[..2], [1, 3]);
    let connection = search::open_index(&index.display().to_string()).unwrap();
    assert_eq!(
        search::index_source(&connection).unwrap().map(|s| s.code),
        Some("qld")
    );
}

#[test]
fn a_disallowed_path_or_missing_title_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("qld");
    let mut blocked = Register::new(
        Ok("User-agent: *\nDisallow: /view/whole/\n"),
        &[(REG, EXCERPT)],
    );
    let error = run(&mut blocked, &ONE_TITLE, &root).unwrap_err();
    assert!(error.contains("robots.txt disallows"), "{error}");
    assert_eq!(blocked.asked, [ROBOTS]);
    assert!(!root.exists());

    let mut challenged = Register::new(Err(403), &[(REG, EXCERPT)]);
    let error = run(&mut challenged, &ONE_TITLE, &root).unwrap_err();
    assert!(error.contains("HTTP 403; nothing fetched"), "{error}");
    assert!(!root.exists());

    // No robots.txt at all is no rule; a missing title stops the run.
    let mut missing = Register::new(Err(404), &[]);
    let error = run(&mut missing, &ONE_TITLE, &root).unwrap_err();
    assert!(error.contains("HTTP 404; nothing written"), "{error}");
    assert_eq!(missing.slept, [state::REQUEST_SPACING]);
    assert!(!root.exists());
}

fn search_cli(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("search")
        .args(args)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn a_state_index_is_searched_only_when_its_source_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("qld");
    let mut register = Register::new(Ok(""), &[(REG, EXCERPT)]);
    run(&mut register, &ONE_TITLE, &root).unwrap();
    let index = dir.path().join("qld.sqlite").display().to_string();
    search::build(&root.display().to_string(), &index).unwrap();

    for command in [
        vec!["query", &index, "repealed"],
        vec!["define", &index, "anything"],
        vec!["refs", &index, "sl-2021-0019:0001:1"],
    ] {
        let (code, stdout, stderr) = search_cli(&command);
        assert_eq!(code, 1, "{command:?}: {stdout}");
        assert!(
            stderr.contains("the index holds qld legislation, not commonwealth; pass --source qld"),
            "{stderr}"
        );
    }
    let (code, stdout, stderr) = search_cli(&["query", &index, "repealed", "--source", "qld"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("1. Land Tax Regulation 2021: 5 Repeal"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "{REG}:0003:5, as at 2023-07-01, retrieved 2099-01-02"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains("Based on content from the Queensland Legislation website at 2099-01-02."),
        "{stdout}"
    );
    assert!(!stdout.contains("EPUB"), "{stdout}");

    let (code, stdout, _) = search_cli(&[
        "query",
        &index,
        "regulation",
        "--source",
        "qld",
        "--title",
        REG,
        "--json",
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains(REG), "{stdout}");
    let (code, _, stderr) =
        search_cli(&["refs", &index, &format!("{REG}:0002:2"), "--source", "qld"]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = search_cli(&[
        "query",
        &index,
        "regulation",
        "--source",
        "qld",
        "--as-at",
        "2099-01-01",
    ]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("--as-at reads the Commonwealth build"),
        "{stderr}"
    );
    let (code, _, stderr) = search_cli(&["quote", &index, &format!("{REG}:0001:1"), "cited"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("quote reads only commonwealth indexes"),
        "{stderr}"
    );
}

#[test]
fn rulings_runs_are_refused_with_a_state_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("qld");
    let mut register = Register::new(Ok(""), &[(REG, EXCERPT)]);
    run(&mut register, &ONE_TITLE, &root).unwrap();
    let error = search::build_with_rulings(
        &root.display().to_string(),
        &dir.path().join("x.sqlite").display().to_string(),
        &["run".to_string()],
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("rulings runs cite Commonwealth law"),
        "{error}"
    );
}

#[test]
fn a_sources_file_naming_an_unknown_source_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("qld");
    let mut register = Register::new(Ok(""), &[(REG, EXCERPT)]);
    run(&mut register, &ONE_TITLE, &root).unwrap();
    let path = root.join("sources.json");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"qld\"", "\"vic\"");
    std::fs::write(&path, text).unwrap();
    let error = search::build(
        &root.display().to_string(),
        &dir.path().join("x.sqlite").display().to_string(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("sources.json names an unknown source: 'vic'"),
        "{error}"
    );
}
