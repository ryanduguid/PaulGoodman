//! The opener's rules that need no socket: response heads, proxy variables
//! and `no_proxy`, the names failures carry, the trait's defaults and
//! `fetch_json`'s retries.
use std::io::Cursor;

use super::*;

fn net(proxies: &[(&str, &str)]) -> Net {
    Net {
        proxies: proxies
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        tls: None,
        started: Instant::now(),
        agent: UA,
    }
}

fn failure(name: &str) -> Failure {
    Failure::Transport(name.into())
}

#[test]
fn heads_hold_http_clients_limits() {
    let long_header = format!("X: {}\r\n", "y".repeat(MAXLINE));
    assert_eq!(
        begin(&mut Cursor::new(format!(
            "HTTP/1.1 200 OK\r\n{long_header}\r\n"
        )))
        .err(),
        Some(failure("LineTooLong"))
    );
    let long_interim = format!("HTTP/1.1 100 Continue\r\n{long_header}\r\n");
    assert_eq!(
        begin(&mut Cursor::new(long_interim)).err(),
        Some(failure("LineTooLong"))
    );
    // A 1xx or 304 response has no body, whatever its Content-Length says.
    let head = begin(&mut Cursor::new(
        "HTTP/1.1 304 Not Modified\r\nContent-Length: 4\r\n\r\n",
    ))
    .unwrap();
    assert_eq!(
        (head.status, head.length, head.chunked),
        (304, Some(0), false)
    );
    let head = begin(&mut Cursor::new(
        "HTTP/1.1 200 OK\r\nContent-Length: 1e3\r\n\r\n",
    ))
    .unwrap();
    assert_eq!(head.length, None);
    let head = begin(&mut Cursor::new(format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        "9".repeat(30)
    )))
    .unwrap();
    assert_eq!(head.length, Some(u64::MAX));
}

#[test]
fn no_proxy_matches_hosts_as_proxy_bypass_does() {
    let net = net(&[("no", " , .example.invalid,127.0.0.1")]);
    for (host, bypassed) in [
        ("example.invalid", true),
        ("www.Example.invalid:8080", true),
        ("127.0.0.1:99", true),
        ("other.invalid", false),
        ("notexample.invalid", false),
    ] {
        assert_eq!(net.bypass(host), bypassed, "{host}");
    }
    assert!(self::net(&[("no", "*")]).bypass("any"));
    assert!(!self::net(&[]).bypass("any"));
}

#[test]
fn proxies_come_from_the_environment_as_getproxies_reads_them() {
    let env = |pairs: &[(&str, &str)]| {
        let pairs: Vec<_> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let mut got: Vec<_> = proxies_from(&pairs).into_iter().collect();
        got.sort();
        got
    };
    let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    assert_eq!(
        env(&[
            ("HTTP_PROXY", "upper"),
            ("http_proxy", "lower"),
            ("PATH", "/bin")
        ]),
        [pair("http", "lower")]
    );
    // An empty lowercase variable removes the uppercase one's proxy.
    assert_eq!(env(&[("HTTPS_PROXY", "p"), ("https_proxy", "")]), []);
    // A bare `_proxy` names the empty scheme.
    assert_eq!(
        env(&[("No_Proxy", "h"), ("_proxy", "x")]),
        [pair("", "x"), pair("no", "h")]
    );
    // Under CGI, HTTP_PROXY is the client's header, so it is ignored.
    assert_eq!(env(&[("HTTP_PROXY", "p"), ("REQUEST_METHOD", "GET")]), []);
    assert_eq!(
        env(&[("http_proxy", "p"), ("REQUEST_METHOD", "GET")]),
        [pair("http", "p")]
    );
}

#[test]
fn socket_errors_take_the_names_python_gives_them() {
    use io::ErrorKind::*;
    for (kind, name) in [
        (ConnectionReset, "ConnectionResetError"),
        (ConnectionAborted, "ConnectionAbortedError"),
        (BrokenPipe, "BrokenPipeError"),
        (TimedOut, "TimeoutError"),
        (WouldBlock, "TimeoutError"),
        (InvalidData, "SSLError"),
        (NotFound, "OSError"),
    ] {
        assert_eq!(os_error(&io::Error::from(kind)), failure(name));
    }
    assert_eq!(fail_name(failure("X")), "X");
    assert_eq!(fail_name(Failure::Status(503)), "HTTP503");
    assert_eq!(fail_name(value_error("m".into())), "ValueError");
    assert_eq!(in_request(failure("OSError")), failure("URLError"));
    assert_eq!(in_request(failure("LineTooLong")), failure("LineTooLong"));
}

/// A fake that scripts `get` alone, as the stages' simplest fakes do.
struct Scripted(Vec<Result<Response, Failure>>);

impl Http for Scripted {
    fn get(&mut self, _: &str, _: Option<usize>, _: &mut Request) -> Result<Response, Failure> {
        self.0.remove(0)
    }
    fn sleep(&mut self, _: f64) {}
    fn today(&mut self) -> String {
        "2026-11-28".into()
    }
}

fn body(text: &str) -> Result<Response, Failure> {
    Ok(Response {
        url: String::new(),
        content_type: String::new(),
        body: text.as_bytes().to_vec(),
    })
}

#[test]
fn the_trait_defaults_fail_every_exchange_and_stand_the_clock_still() {
    let mut fake = Scripted(Vec::new());
    let mut request = Request::default();
    assert_eq!(
        fake.exchange("u", 1, &mut request).unwrap_err(),
        failure("URLError")
    );
    assert_eq!(
        fake.download("u", "p", 1, None, true, &mut request)
            .unwrap_err(),
        failure("URLError")
    );
    assert_eq!(fake.monotonic(), 0.0);
    assert_eq!(fake.utc_now(), "1970-01-01T00:00:00Z");
    assert_eq!(fake.today(), "2026-11-28");
}

#[test]
fn fetch_json_stops_the_stage_on_the_errors_it_does_not_catch() {
    let uncaught = |class| {
        Err(Failure::Uncaught {
            class,
            message: "m".into(),
        })
    };
    let mut fake = Scripted(vec![uncaught("UnicodeEncodeError")]);
    assert!(matches!(fetch_json(&mut fake, "u"), Err(Exc::UnicodeEncode(m)) if m == "m"));
    let mut fake = Scripted(vec![uncaught("ValueError")]);
    assert!(matches!(
        fetch_json(&mut fake, "u"),
        Err(Exc::Value {
            class: "ValueError",
            ..
        })
    ));
    let mut fake = Scripted(vec![body(&format!("{{\"n\": {}}}", "1".repeat(4301)))]);
    assert!(matches!(
        fetch_json(&mut fake, "u"),
        Err(Exc::Value { message, .. }) if message.starts_with("Exceeds the limit")
    ));
    let mut fake = Scripted(vec![body(&"[".repeat(2 * pyjson::MAX_DEPTH))]);
    assert!(matches!(fetch_json(&mut fake, "u"), Err(Exc::Recursion(_))));
    // A failed request, an error member and a non-object are all retried.
    let mut fake = Scripted(vec![
        Err(failure("URLError")),
        body("{\"error\": 1}"),
        body("{\"ok\": 1}"),
    ]);
    assert!(fetch_json(&mut fake, "u").unwrap().is_some());
    let mut fake = Scripted(vec![body("[]"), Err(Failure::Status(500)), body("x")]);
    assert!(fetch_json(&mut fake, "u").unwrap().is_none());
}

#[test]
fn small_parsers_follow_urllib_and_http_client() {
    // A proxy without a scheme or an authority slash is all host:port.
    assert_eq!(
        parse_proxy("proxyhost").unwrap(),
        (None, None, None, "proxyhost")
    );
    assert_eq!(
        parse_proxy("http://user@p:1").unwrap(),
        (Some("http".into()), Some("user"), None, "p:1")
    );
    // A header block cut off before its line break still yields the field.
    assert_eq!(
        parse_header_lines(&[b"A: 1".to_vec()]),
        [("A".into(), "1".into())]
    );
    // A query straight after the host gets a leading slash.
    let req = Req::new("http://h?q=1", Vec::new()).unwrap();
    assert_eq!((req.host.as_str(), req.selector.as_str()), ("h", "/?q=1"));
    let mut headers = Vec::new();
    set_header(&mut headers, "A", "1");
    set_header(&mut headers, "B", "2");
    set_header(&mut headers, "A", "3");
    assert_eq!(
        headers,
        [("A".to_owned(), "3".to_owned()), ("B".into(), "2".into())]
    );
}

#[test]
fn redirect_targets_are_split_quoted_and_joined_as_urllib_does() {
    let target = |location| redirect_target("http://h/x/y", location).unwrap();
    assert_eq!(target("a;p").as_deref(), Some("http://h/x/a;p"));
    assert_eq!(target("http://other").as_deref(), Some("http://other/"));
    assert_eq!(target("/p;").as_deref(), Some("http://h/p"));
    assert_eq!(target("data:x"), None);
}
