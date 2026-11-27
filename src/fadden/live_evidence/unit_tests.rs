// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `_candidate_bundle`, `_raw_current_row` and `_read_json` called directly,
//! as the Python stage's helpers can be: the export validates the capture
//! graph first, so most of these refusals cannot be reached through it.
use serde_json::{Value, json};

use super::*;

const RESPONSE: &str = "{\"@odata.context\":\"https://api.prod.legislation.gov.au/v1/\
$metadata#Versions(titleId,start,compilationNumber,registerId,\
isCurrent,status,registeredAt)\",\"value\":[{\"titleId\":\"F2022L00347\",\
\"start\":\"2026-08-18T00:00:00\",\"compilationNumber\":\"20\",\
\"registerId\":\"F2026C00838\",\"isCurrent\":true,\"status\":\"InForce\",\
\"registeredAt\":\"2026-08-27T17:31:41.1234567+10:00\"}]}";

/// A change to the title, result and observation before the result digest is taken.
type Edit = Box<dyn FnOnce(&mut Value, &mut Value, &mut Value)>;

fn py(value: &Value) -> Json {
    pyjson::loads(&value.to_string()).unwrap()
}

fn raised(error: &Exc) -> String {
    match live_message(error) {
        Some(message) => message.to_string(),
        None => format!("{}: {error}", error.class()),
    }
}

/// One SUPERSEDED title, its capture result and its observation, whose
/// retained response is `RESPONSE`. The observation's `capture_result_sha256`
/// is null until [`fill_digest`] sets it.
fn parts() -> (Value, Value, Value) {
    let digest = sha256_id(RESPONSE.as_bytes());
    let title = json!({
        "register_id": "F2022L00347",
        "name": "Taxation Administration Regulations 2017",
        "collection": "LegislativeInstrument",
        "compilation_number": "19",
        "compilation_date": "2026-08-05",
        "version_is_current": true,
        "current_version_start": null,
        "retrieved": "2026-08-05",
        "source_url": "https://www.legislation.gov.au/F2022L00347/2026-08-05/2026-08-05/text/original/epub",
        "register_page": "https://www.legislation.gov.au/F2022L00347/latest/text"
    });
    let result = json!({
        "register_id": "F2022L00347",
        "collection": "LegislativeInstrument",
        "checked_at": "2026-08-28T00:00:00Z",
        "state": "SUPERSEDED",
        "error_category": null,
        "requests": [{
            "role": "current",
            "url": "https://api.prod.legislation.gov.au/v1/versions",
            "checked_at": "2026-08-28T00:00:00Z",
            "http_status": 200,
            "transport_error_category": null,
            "attempt_count": 1,
            "response_headers": {"content-type": "application/json"},
            "response_length": RESPONSE.len(),
            "response_sha256": digest,
            "evidence_path": format!("evidence/sha256-{}.json", &digest[7..])
        }]
    });
    let observation = json!({
        "register_id": "F2022L00347",
        "collection": "LegislativeInstrument",
        "state": "SUPERSEDED",
        "evidence_id": "frl:F2022L00347:0",
        "observed_compilation_number": "20",
        "observed_compilation_date": "2026-08-18",
        "observed_register_document_id": "F2026C00838",
        "current_version_start": null,
        "evidence_url": "https://www.legislation.gov.au/F2022L00347/latest/text",
        "checked_at": "2026-08-28T00:00:00Z",
        "error_category": null,
        "capture_result_sha256": null,
        "primary_response_sha256": digest,
        "primary_response_media_type": "application/json"
    });
    (title, result, observation)
}

/// Set a null `capture_result_sha256` to the projected result's digest.
fn fill_digest(result: &Value, observation: &mut Value) {
    if observation["capture_result_sha256"].is_null()
        && let Ok(projected) = project_capture_result(&py(result))
    {
        observation["capture_result_sha256"] = json!(sha256_id(&json_bytes(&projected).unwrap()));
    }
}

/// `_candidate_bundle` over [`parts`] after `edit`.
fn candidate(edit: Edit) -> Result<(String, Json), Exc> {
    let (mut title, mut result, mut observation) = parts();
    edit(&mut title, &mut result, &mut observation);
    fill_digest(&result, &mut observation);
    let run = Run4 {
        version: "1.2.3".into(),
        baseline_sha256: "sha256:b".into(),
        observation_sha256: "sha256:o".into(),
        observed_at: Json::str("2026-08-28T00:00:00Z"),
    };
    candidate_bundle(
        &py(&title),
        &py(&result),
        &py(&observation),
        &run,
        RESPONSE.as_bytes(),
    )
}

fn refusal(edit: Edit) -> String {
    raised(&candidate(edit).unwrap_err())
}

#[test]
fn the_unedited_candidate_is_accepted() {
    let (id, bundle) = candidate(Box::new(|_, _, _| {})).unwrap();
    assert_eq!(id, "bundle-frl-f2022l00347-f2026c00838-r1");
    assert!(is(get(&bundle, "schema_version"), "evidence-bundle.v2"));
}

#[test]
fn each_candidate_field_is_checked() {
    let other = format!("sha256:{}", "0".repeat(64));
    let cases: Vec<(Edit, &str)> = vec![
        (
            Box::new(|t, _, _| t["register_id"] = json!("f2022l00347")),
            "baseline title register_id is invalid.",
        ),
        (
            Box::new(|_, _, o| o["observed_register_document_id"] = json!(7)),
            "observed register document id is invalid.",
        ),
        (
            Box::new(|_, r, _| r["requests"] = json!("current")),
            "capture result requests must be an array.",
        ),
        (
            Box::new(|_, r, _| r["requests"] = json!([5])),
            "capture request must be an object.",
        ),
        (
            Box::new(|_, r, _| {
                let request = r["requests"][0].clone();
                r["requests"] = json!([request.clone(), request]);
            }),
            "SUPERSEDED capture result must have one request.",
        ),
        (
            Box::new(|_, r, _| r["requests"] = json!([])),
            "SUPERSEDED capture result must have one request.",
        ),
        (
            Box::new(|_, r, _| r["requests"][0]["response_sha256"] = json!(5)),
            "response SHA-256 must be non-empty text.",
        ),
        (
            Box::new(|_, r, _| r["requests"][0]["response_sha256"] = json!("sha256:ABC")),
            "response SHA-256 must be a lowercase SHA-256 identifier.",
        ),
        (
            Box::new(move |_, r, _| r["requests"][0]["response_sha256"] = json!(other)),
            "retained response digest does not match the capture request.",
        ),
        (
            Box::new(|_, r, _| r["requests"][0]["response_length"] = json!(RESPONSE.len() + 1)),
            "retained response length does not match the capture request.",
        ),
        (
            Box::new(|_, _, o| o["primary_response_sha256"] = json!("sha256:0")),
            "observation response digest does not match the capture request.",
        ),
        (
            Box::new(|_, r, _| r["state"] = json!("UNCHANGED")),
            "candidate state is not SUPERSEDED.",
        ),
        (
            Box::new(|_, _, o| o["state"] = json!("UNCHANGED")),
            "candidate state is not SUPERSEDED.",
        ),
        (
            Box::new(|_, r, _| r["requests"][0]["role"] = json!("previous")),
            "candidate current request is invalid.",
        ),
        (
            Box::new(|_, r, _| r["requests"][0]["http_status"] = json!(304)),
            "candidate current request is invalid.",
        ),
        (
            Box::new(|_, _, o| o["primary_response_media_type"] = json!("text/html")),
            "candidate primary response media type is invalid.",
        ),
        (
            Box::new(|_, _, o| o["observed_compilation_number"] = json!("19")),
            "retained response does not prove the observed compilation.",
        ),
        (
            Box::new(|_, _, o| o["capture_result_sha256"] = json!("sha256:0")),
            "observation capture result digest is invalid.",
        ),
    ];
    for (edit, expected) in cases {
        assert_eq!(refusal(edit), expected);
    }
}

#[test]
fn the_retained_response_must_be_one_current_odata_row() {
    let context = "https://api.prod.legislation.gov.au/v1/$metadata#Versions(titleId,start,\
                   compilationNumber,registerId,isCurrent,status,registeredAt)";
    let row = r#"{"titleId":"F2022L00347","start":"2026-08-18T00:00:00","compilationNumber":"20","registerId":null,"isCurrent":true,"status":"InForce","registeredAt":"2026-08-27T17:31:41.1234567+10:00"}"#;
    for (response, expected) in [
        (
            "[]".to_string(),
            "retained response OData shape is invalid.",
        ),
        (
            format!(r#"{{"@odata.context":"{context}","value":[]}}"#),
            "retained response OData shape is invalid.",
        ),
        (
            format!(r#"{{"@odata.context":"{context}","value":[5]}}"#),
            "retained response OData row is invalid.",
        ),
        (
            format!(r#"{{"@odata.context":"{context}","value":[{row}]}}"#),
            "retained response current document identifier is missing.",
        ),
        // CPython's int() limit escapes as ValueError, not as a refusal.
        (
            format!(
                r#"{{"@odata.context":"{context}","value":[{}]}}"#,
                "1".repeat(5000)
            ),
            "ValueError: Exceeds the limit (4300 digits) for integer string conversion: \
             value has 5000 digits; use sys.set_int_max_str_digits() to increase the limit",
        ),
    ] {
        let error = raw_current_row(response.as_bytes(), "F2022L00347").unwrap_err();
        assert_eq!(raised(&error), expected, "{response}");
    }
}

#[test]
fn json_bytes_refuses_non_finite_floats_as_cpython_does() {
    for (value, expected) in [
        (
            Json::Float(f64::NAN),
            "ValueError: Out of range float values are not JSON compliant: nan",
        ),
        (
            Json::Array(vec![Json::Object(vec![(
                PyStr::Text("a".into()),
                Json::Float(f64::NEG_INFINITY),
            )])]),
            "ValueError: Out of range float values are not JSON compliant: -inf",
        ),
    ] {
        assert_eq!(raised(&json_bytes(&value).unwrap_err()), expected);
    }
}

#[test]
fn read_json_refuses_duplicates_non_objects_and_escapes_the_digit_limit() {
    let dir = tempfile::tempdir().unwrap();
    for (content, expected) in [
        (
            r#"{"a":1,"a":2}"#.to_string(),
            "baseline could not be read as UTF-8 JSON.",
        ),
        ("[1]".to_string(), "baseline must be an object."),
        (
            format!("[{}]", "1".repeat(5000)),
            "ValueError: Exceeds the limit (4300 digits) for integer string conversion: \
             value has 5000 digits; use sys.set_int_max_str_digits() to increase the limit",
        ),
    ] {
        let path = dir.path().join("baseline.json");
        std::fs::write(&path, &content).unwrap();
        let error = read_json(path.to_str().unwrap(), "baseline").unwrap_err();
        assert_eq!(raised(&error), expected, "{content}");
    }
}

/// `_build_validated_snapshot` over a graph of [`parts`] after `edit`: the
/// baseline, the capture and the observation documents. Null digests are
/// filled in after `edit`.
fn snapshot(
    edit: impl FnOnce(&mut Value, &mut Value, &mut Value),
) -> Result<(String, Vec<(String, Json)>), Exc> {
    let (title, result, mut item) = parts();
    fill_digest(&result, &mut item);
    let mut baseline = json!({"titles": [title]});
    let mut capture = json!({
        "schema_version": "au-tax-register-capture.v1",
        "mode": "live",
        "baseline_sha256": null,
        "results": [result]
    });
    let mut observation = json!({
        "schema_version": "au-tax-register-observation.v4",
        "mode": "live",
        "scope_id": "au-primary-tax-legislation.v4",
        "complete": true,
        "run_status": "VERIFIED",
        "observed_at": "2026-08-28T00:00:00Z",
        "baseline_sha256": null,
        "capture_sha256": null,
        "observations": [item]
    });
    edit(&mut baseline, &mut capture, &mut observation);
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, value: &Value| {
        let bytes = value.to_string().into_bytes();
        std::fs::write(dir.path().join(name), &bytes).unwrap();
        sha256_id(&bytes)
    };
    let baseline_sha256 = write("monitor-baseline.json", &baseline);
    for document in [&mut capture, &mut observation] {
        if document["baseline_sha256"].is_null() {
            document["baseline_sha256"] = json!(baseline_sha256);
        }
    }
    let capture_sha256 = write("register-capture.json", &capture);
    if observation["capture_sha256"].is_null() {
        observation["capture_sha256"] = json!(capture_sha256);
    }
    write("register-observation.json", &observation);
    let digest = sha256_id(RESPONSE.as_bytes());
    std::fs::create_dir(dir.path().join("evidence")).unwrap();
    std::fs::write(
        dir.path()
            .join("evidence")
            .join(format!("sha256-{}.json", &digest[7..])),
        RESPONSE,
    )
    .unwrap();
    build_validated_snapshot(dir.path().to_str().unwrap())
}

#[test]
fn the_snapshot_is_checked_before_any_candidate() {
    type GraphEdit = Box<dyn FnOnce(&mut Value, &mut Value, &mut Value)>;
    let cases: Vec<(GraphEdit, &str)> = vec![
        (
            Box::new(|_, c, _| c["baseline_sha256"] = json!("sha256:0")),
            "capture graph digests are inconsistent.",
        ),
        (
            Box::new(|b, _, _| b["titles"] = json!({})),
            "capture graph collections must be arrays.",
        ),
        (
            Box::new(|b, _, _| b["titles"][0]["register_id"] = json!("bad")),
            "baseline title register_id is invalid.",
        ),
        (
            Box::new(|_, c, _| c["results"][0]["register_id"] = json!("bad")),
            "capture result register_id is invalid.",
        ),
        (
            Box::new(|_, c, _| c["results"][0]["requests"] = json!([])),
            "SUPERSEDED capture result must have a request.",
        ),
        (
            Box::new(|_, c, _| {
                c["results"][0]["requests"][0]["evidence_path"] = json!("evidence/other.json");
            }),
            "capture evidence path is invalid.",
        ),
        (
            Box::new(|_, _, o| {
                let item = o["observations"][0].clone();
                o["observations"] = json!([item.clone(), item]);
            }),
            "candidate identity is duplicated.",
        ),
    ];
    for (edit, expected) in cases {
        assert_eq!(raised(&snapshot(edit).unwrap_err()), expected);
    }
}

#[test]
fn a_repeated_title_or_result_is_replaced_by_the_last() {
    let (tag, candidates) = snapshot(|b, c, _| {
        let title = b["titles"][0].clone();
        let mut first = title.clone();
        first["name"] = json!("an earlier entry");
        b["titles"] = json!([first, title]);
        let result = c["results"][0].clone();
        let mut first = result.clone();
        first["requests"] = json!([]);
        c["results"] = json!([first, result]);
    })
    .unwrap();
    assert!(tag.starts_with("live-evidence-v2-"));
    assert_eq!(candidates.len(), 1);
    let title = get(&candidates[0].1, "baseline_title");
    assert!(is(
        get(title, "name"),
        "Taxation Administration Regulations 2017"
    ));
}
