//! `Net` against scripted servers on loopback: the request it writes, the
//! response rules it reads by, its redirects, proxies and tunnels, and the
//! failures each step reports.
use std::io::Cursor;
use std::net::TcpListener;
use std::thread::JoinHandle;

use super::*;

/// What a scripted server does on one connection.
enum Turn {
    /// Read the request head, then write the reply.
    Reply(Vec<u8>),
    /// Read the request head, write the reply, then read what the client
    /// sends next and write `after`.
    Tunnel(Vec<u8>, Vec<u8>),
    /// Read what the client sends first and write `after`.
    Raw(Vec<u8>),
}

/// Serve each turn on its own connection, in order, on loopback. The base
/// URL, and a handle whose join gives what each connection's client sent.
fn serve(turns: Vec<Turn>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        turns
            .into_iter()
            .map(|turn| {
                let (mut socket, _) = listener.accept().unwrap();
                match turn {
                    Turn::Reply(reply) => {
                        let head = read_head(&mut socket);
                        let _ = socket.write_all(&reply);
                        head
                    }
                    Turn::Tunnel(reply, after) => {
                        let head = read_head(&mut socket);
                        let _ = socket.write_all(&reply);
                        let mut hello = [0u8; 4096];
                        let _ = socket.read(&mut hello);
                        let _ = socket.write_all(&after);
                        head
                    }
                    Turn::Raw(after) => {
                        let mut hello = [0u8; 4096];
                        let n = socket.read(&mut hello).unwrap_or(0);
                        let _ = socket.write_all(&after);
                        // A TLS record starts with its content type, 22.
                        format!("{} bytes from 0x{:02x}", n, hello[0])
                    }
                }
            })
            .collect()
    });
    (base, handle)
}

fn read_head(socket: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && socket.read(&mut byte).unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

fn reply(text: &str) -> Turn {
    Turn::Reply(text.as_bytes().to_vec())
}

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

fn get(net: &mut Net, url: &str, limit: Option<usize>) -> Result<Response, Failure> {
    net.get(url, limit, &mut Request::default())
}

fn failure(name: &str) -> Failure {
    Failure::Transport(name.into())
}

#[test]
fn a_get_writes_http_clients_request_and_reads_the_body() {
    let (base, server) = serve(vec![
        reply("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\nhello"),
        reply("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"),
        reply("HTTP/1.0 200\r\n\r\nto the end"),
    ]);
    let mut net = net(&[]);
    let response = get(&mut net, &format!("{base}/a?q=1#frag"), None).unwrap();
    assert_eq!(response.url, format!("{base}/a?q=1#frag"));
    assert_eq!(response.content_type, "text/html");
    assert_eq!(response.body, b"hello");
    // A limit reads only that much of a longer body; no Content-Type is "".
    let short = get(&mut net, &format!("{base}/b"), Some(3)).unwrap();
    assert_eq!((short.content_type.as_str(), &short.body[..]), ("", &b"hel"[..]));
    // Without a length the body runs to the close.
    assert_eq!(get(&mut net, &base, None).unwrap().body, b"to the end");
    let host = base.trim_start_matches("http://");
    assert_eq!(
        server.join().unwrap(),
        [
            format!(
                "GET /a?q=1 HTTP/1.1\r\nAccept-Encoding: identity\r\nHost: {host}\r\n\
                 User-Agent: {UA}\r\nConnection: close\r\n\r\n"
            ),
            format!(
                "GET /b HTTP/1.1\r\nAccept-Encoding: identity\r\nHost: {host}\r\n\
                 User-Agent: {UA}\r\nConnection: close\r\n\r\n"
            ),
            format!(
                "GET / HTTP/1.1\r\nAccept-Encoding: identity\r\nHost: {host}\r\n\
                 User-Agent: {UA}\r\nConnection: close\r\n\r\n"
            ),
        ]
    );
}

#[test]
fn statuses_framing_and_interim_responses_read_as_http_client_does() {
    let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   5\r\nhello\r\n6;x=1\r\n world\r\n0\r\nTrailer: 1\r\n\r\n";
    let (base, server) = serve(vec![
        reply("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"),
        reply(chunked),
        reply(chunked),
        reply("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"),
        reply("HTTP/1.1 100 Continue\r\nX: 1\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
        reply("HTTP/1.1 204 No Content\r\nContent-Length: 9\r\n\r\nignored"),
        reply("HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort"),
        reply("HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\nall"),
    ]);
    let mut net = net(&[]);
    let mut get = |path: &str, limit| get(&mut net, &format!("{base}{path}"), limit);
    assert_eq!(get("/404", None).unwrap_err(), Failure::Status(404));
    assert_eq!(get("/chunked", None).unwrap().body, b"hello world");
    // A limit inside the second chunk stops there.
    assert_eq!(get("/chunked", Some(7)).unwrap().body, b"hello w");
    assert_eq!(get("/bad-chunk", None).unwrap_err(), failure("IncompleteRead"));
    assert_eq!(get("/continue", None).unwrap().body, b"ok");
    assert_eq!(get("/204", None).unwrap().body, b"");
    // An unbounded read short of Content-Length is IncompleteRead; a bounded
    // one returns what arrived.
    assert_eq!(get("/short", None).unwrap_err(), failure("IncompleteRead"));
    // A negative length is ignored, so the body runs to the close.
    assert_eq!(get("/negative", None).unwrap().body, b"all");
    assert_eq!(server.join().unwrap().len(), 8);
}

#[test]
fn a_bounded_read_of_a_short_body_returns_what_arrived() {
    let (base, server) = serve(vec![reply(
        "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort",
    )]);
    let body = get(&mut net(&[]), &base, Some(100)).unwrap().body;
    assert_eq!(body, b"short");
    server.join().unwrap();
}

#[test]
fn malformed_responses_fail_as_http_client_raises() {
    let long = format!("HTTP/1.1 200 {}\r\n\r\n", "x".repeat(MAXLINE));
    let many = format!(
        "HTTP/1.1 200 OK\r\n{}\r\n",
        "X: 1\r\n".repeat(MAXHEADERS + 1)
    );
    let interim = "HTTP/1.1 100 Continue\r\n\r\n".repeat(MAXINTERIM);
    let cases = [
        ("", "RemoteDisconnected"),
        ("HTTX/1.1 200 OK\r\n\r\n", "BadStatusLine"),
        ("HTTP/1.1\r\n\r\n", "BadStatusLine"),
        ("HTTP/1.1 abc OK\r\n\r\n", "BadStatusLine"),
        ("HTTP/1.1 99 Low\r\n\r\n", "BadStatusLine"),
        ("HTTP/2.0 200 OK\r\n\r\n", "UnknownProtocol"),
        (long.as_str(), "LineTooLong"),
        (many.as_str(), "HTTPException"),
        (interim.as_str(), "HTTPException"),
    ];
    let (base, server) = serve(cases.iter().map(|(text, _)| reply(text)).collect());
    let mut net = net(&[]);
    for (text, name) in cases {
        let got = get(&mut net, &base, None).unwrap_err();
        assert_eq!(got, failure(name), "{:?}", &text[..text.len().min(40)]);
    }
    server.join().unwrap();
}

#[test]
fn heads_and_chunked_bodies_hold_http_clients_limits() {
    let long_header = format!("X: {}\r\n", "y".repeat(MAXLINE));
    assert_eq!(
        begin(&mut Cursor::new(format!("HTTP/1.1 200 OK\r\n{long_header}\r\n"))).err(),
        Some(failure("LineTooLong"))
    );
    let long_interim = format!("HTTP/1.1 100 Continue\r\n{long_header}\r\n");
    assert_eq!(
        begin(&mut Cursor::new(long_interim)).err(),
        Some(failure("LineTooLong"))
    );
    // A 1xx or 304 response has no body, whatever its Content-Length says.
    let head = begin(&mut Cursor::new("HTTP/1.1 304 Not Modified\r\nContent-Length: 4\r\n\r\n"))
        .unwrap();
    assert_eq!((head.status, head.length, head.chunked), (304, Some(0), false));
    let head = begin(&mut Cursor::new("HTTP/1.1 200 OK\r\nContent-Length: 1e3\r\n\r\n")).unwrap();
    assert_eq!(head.length, None);
    let head = begin(&mut Cursor::new(format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        "9".repeat(30)
    )))
    .unwrap();
    assert_eq!(head.length, Some(u64::MAX));

    let trailer = "X: 1\r\n".repeat(MAXHEADERS + 1);
    let (base, server) = serve(vec![
        reply(&format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{}\r\n",
            "0".repeat(MAXLINE + 1)
        )),
        reply(&format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n{}\r\n",
            "t".repeat(MAXLINE + 1)
        )),
        reply(&format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n{trailer}\r\n"
        )),
        reply("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel"),
    ]);
    let mut net = net(&[]);
    for name in ["LineTooLong", "LineTooLong", "HTTPException", "IncompleteRead"] {
        assert_eq!(get(&mut net, &base, None).unwrap_err(), failure(name));
    }
    server.join().unwrap();
}

#[test]
fn redirects_are_followed_as_http_redirect_handler_does() {
    let found = |location: &str| {
        reply(&format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 4\r\n\r\nbody"
        ))
    };
    let (base, server) = serve(vec![
        found("/next;p?q=a b"),
        reply("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
        reply("HTTP/1.1 301 Moved\r\nURI: /uri\r\n\r\n"),
        reply("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nuri"),
        reply("HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n"),
        found("mailto:someone@example.invalid"),
        found("ftp://127.0.0.1:1/file"),
        found("http://[::1"),
    ]);
    let mut net = net(&[]);
    let response = get(&mut net, &format!("{base}/start"), None).unwrap();
    // The target is quoted, then joined to the request's URL.
    assert_eq!(response.url, format!("{base}/next;p?q=a%20b"));
    assert_eq!(response.body, b"ok");
    let response = get(&mut net, &format!("{base}/old"), None).unwrap();
    assert_eq!((response.url, response.body), (format!("{base}/uri"), b"uri".to_vec()));
    // No target, or a scheme urllib refuses, raises the redirect's status.
    assert_eq!(get(&mut net, &base, None).unwrap_err(), Failure::Status(302));
    assert_eq!(get(&mut net, &base, None).unwrap_err(), Failure::Status(302));
    // An FTP target is followed, then fails as urllib's FTP handler is not ported.
    assert_eq!(get(&mut net, &base, None).unwrap_err(), failure("URLError"));
    // A malformed target is the ValueError nothing catches.
    assert!(matches!(
        get(&mut net, &base, None),
        Err(Failure::Uncaught { class: "ValueError", .. })
    ));
    let seen = server.join().unwrap();
    assert!(seen[1].starts_with("GET /next;p?q=a%20b HTTP/1.1\r\n"), "{}", seen[1]);
    assert!(seen[3].starts_with("GET /uri HTTP/1.1\r\n"), "{}", seen[3]);
}

#[test]
fn a_redirect_loop_is_refused_after_4_visits() {
    let again = "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n";
    let (base, server) = serve((0..5).map(|_| reply(again)).collect());
    let got = get(&mut net(&[]), &format!("{base}/loop"), None);
    assert_eq!(got.unwrap_err(), Failure::Status(302));
    assert_eq!(server.join().unwrap().len(), 5);
}

#[test]
fn an_exchange_follows_no_redirect_and_reports_every_status() {
    let (base, server) = serve(vec![
        reply("HTTP/1.1 200 OK\r\nETag: \"e\"\r\nContent-Length: 5\r\n\r\nhello"),
        reply("HTTP/1.1 301 Moved\r\nLocation: /elsewhere\r\n\r\n"),
        reply("HTTP/1.1 500 Oops\r\nRetry-After: 9\r\n\r\n"),
        reply("HTTP/1.1 204 No Content\r\n\r\n"),
    ]);
    let mut net = net(&[]);
    let mut request = Request::default();
    let ok = net.exchange(&base, 3, &mut request).unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(ok.headers[0], ("ETag".into(), "\"e\"".into()));
    assert_eq!(ok.body.as_deref(), Some(&b"hel"[..]));
    for (status, header) in [(301, "Location"), (500, "Retry-After")] {
        let got = net.exchange(&base, 3, &mut Request::default()).unwrap();
        assert_eq!((got.status, got.headers[0].0.as_str(), got.body), (status, header, None));
    }
    let empty = net.exchange(&base, 3, &mut Request::default()).unwrap();
    assert_eq!((empty.status, empty.body), (204, None));
    assert_eq!(server.join().unwrap().len(), 4);
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", closed.local_addr().unwrap());
    drop(closed);
    assert_eq!(
        net.exchange(&url, 3, &mut Request::default()).unwrap_err(),
        failure("URLError")
    );
}

#[test]
fn a_download_keeps_the_status_of_a_refused_response() {
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("x.part");
    let part = part.to_str().unwrap();
    let (base, server) = serve(vec![reply(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 4\r\n\r\ngone",
    )]);
    let transfer = net(&[])
        .download(&base, part, 100, None, false, &mut Request::default())
        .unwrap();
    assert_eq!(
        transfer,
        Transfer {
            status: 404,
            content_type: "text/plain".into(),
            exceeds_budget: false,
            transport_error: None,
        }
    );
    assert_eq!(std::fs::read(part).unwrap(), b"gone");
    server.join().unwrap();
}

#[test]
fn requests_urllib_cannot_send_fail_before_connecting() {
    let mut net = net(&[]);
    let cases = [
        ("http:///nohost", "URLError"),
        ("http:relative", "URLError"),
        ("ftp://127.0.0.1:1/file", "URLError"),
        ("a_b://host/", "URLError"),
        ("http://127.0.0.1:x/", "InvalidURL"),
        ("http://127.0.0.1:1/a b", "InvalidURL"),
        ("http://h\u{1}st/", "InvalidURL"),
        ("http://127.0.0.1:70000/", "URLError"),
    ];
    for (url, name) in cases {
        assert_eq!(get(&mut net, url, None).unwrap_err(), failure(name), "{url}");
    }
    assert_eq!(
        get(&mut net, "http://127.0.0.1:1/caf\u{e9}", None).unwrap_err(),
        Failure::Uncaught {
            class: "UnicodeEncodeError",
            message: r"'ascii' codec can't encode character '\xe9' in position 8: ordinal not in range(128)"
                .into(),
        }
    );
    assert!(matches!(
        get(&mut net, "nothing", None),
        Err(Failure::Uncaught { class: "ValueError", .. })
    ));
}

#[test]
fn an_http_proxy_gets_the_full_url_and_the_credentials() {
    let (proxy, server) = serve(vec![
        reply("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nvia"),
        reply("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
    ]);
    let hostport = proxy.trim_start_matches("http://");
    let with_user = format!("http://us%65r:p%77@{hostport}");
    let mut net = net(&[("http", &with_user)]);
    let response = get(&mut net, "http://origin.invalid/page", None).unwrap();
    assert_eq!(response.body, b"vi");
    // A bare host:port proxy takes the request's scheme.
    let mut bare = self::net(&[("http", hostport)]);
    assert_eq!(get(&mut bare, "http://origin.invalid/", None).unwrap().body, b"ok");
    let seen = server.join().unwrap();
    assert_eq!(
        seen[0],
        format!(
            "GET http://origin.invalid/page HTTP/1.1\r\nAccept-Encoding: identity\r\n\
             Host: origin.invalid\r\nUser-Agent: {UA}\r\n\
             Proxy-Authorization: Basic dXNlcjpwdw==\r\nConnection: close\r\n\r\n"
        )
    );
    assert!(seen[1].starts_with("GET http://origin.invalid/ HTTP/1.1\r\n"));
    assert!(!seen[1].contains("Proxy-Authorization"));
}

#[test]
fn no_proxy_sends_matching_hosts_direct() {
    let (base, server) = serve(vec![reply("HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\ndirect")]);
    let mut net = net(&[
        ("http", "http://127.0.0.1:1"),
        ("no", " , .example.invalid,127.0.0.1"),
    ]);
    assert_eq!(get(&mut net, &base, None).unwrap().body, b"direct");
    server.join().unwrap();
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
fn a_proxy_of_another_scheme_opens_the_request_again() {
    // An https:// proxy for http requests is reached over TLS.
    let (proxy, server) = serve(vec![Turn::Raw(b"not tls".to_vec())]);
    let tls_proxy = proxy.replace("http://", "https://");
    let mut net = net(&[("http", &tls_proxy)]);
    assert_eq!(
        get(&mut net, "http://origin.invalid/", None).unwrap_err(),
        failure("URLError")
    );
    let seen = server.join().unwrap();
    assert!(seen[0].ends_with("from 0x16"), "{}", seen[0]);
    // A proxy URL with a slash but no authority is urllib's ValueError.
    let mut net = self::net(&[("http", "http:/proxy")]);
    assert_eq!(
        get(&mut net, "http://origin.invalid/", None).unwrap_err(),
        Failure::Uncaught {
            class: "ValueError",
            message: "proxy URL with no authority: 'http:/proxy'".into(),
        }
    );
}

#[test]
fn https_through_a_proxy_opens_a_connect_tunnel() {
    let tunnel_ok = || b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec();
    let (proxy, server) = serve(vec![
        Turn::Reply(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n".to_vec()),
        Turn::Reply(b"junk\r\n\r\n".to_vec()),
        Turn::Tunnel(tunnel_ok(), Vec::new()),
        Turn::Tunnel(tunnel_ok(), b"HTTP/1.1 400 Not TLS\r\n\r\n".to_vec()),
        Turn::Reply(tunnel_ok()),
    ]);
    let hostport = proxy.trim_start_matches("http://");
    let with_user = format!("http://u:p@{hostport}");
    let mut net = net(&[("https", &with_user)]);
    let mut get = |url: &str| get(&mut net, url, None).unwrap_err();
    // A refused tunnel is the OSError urllib wraps in URLError.
    assert_eq!(get("https://origin.invalid/a"), failure("URLError"));
    // A malformed proxy reply passes through as http.client raised it.
    assert_eq!(get("https://origin.invalid/a"), failure("BadStatusLine"));
    // The proxy closes, or answers in plain text, during the handshake.
    assert_eq!(get("https://[::1]:8443/a"), failure("URLError"));
    assert_eq!(get("https://origin.invalid/a"), failure("URLError"));
    // A tunnel host that is no DNS name cannot be a TLS server name.
    assert_eq!(get("https://a..b/"), failure("URLError"));
    assert_eq!(get("https://origin.invalid:x/"), failure("InvalidURL"));
    let seen = server.join().unwrap();
    assert_eq!(
        seen[0],
        "CONNECT origin.invalid:443 HTTP/1.1\r\n\
         Proxy-Authorization: Basic dTpw\r\nHost: origin.invalid:443\r\n\r\n"
    );
    assert!(seen[2].starts_with("CONNECT [::1]:8443 HTTP/1.1\r\n"), "{}", seen[2]);
    assert_eq!(seen.len(), 5);
    // A control character in the tunnel host is refused before CONNECT.
    let (proxy, server) = serve(vec![Turn::Raw(Vec::new())]);
    let mut net = self::net(&[("https", &proxy)]);
    let got = self::get(&mut net, "https://a\u{1}b/", None).unwrap_err();
    assert_eq!(
        got,
        Failure::Uncaught {
            class: "ValueError",
            message: "Tunnel host can't contain control characters 'a\\x01b'".into(),
        }
    );
    assert_eq!(server.join().unwrap(), ["0 bytes from 0x00"]);
}

#[test]
fn a_direct_https_handshake_with_a_plain_server_fails_as_url_error() {
    let (base, server) = serve(vec![Turn::Raw(b"HTTP/1.1 400 Not TLS\r\n\r\n".to_vec())]);
    let url = base.replace("http://", "https://");
    let mut net = net(&[]);
    assert_eq!(get(&mut net, &url, None).unwrap_err(), failure("URLError"));
    assert!(net.tls.is_some());
    assert!(server.join().unwrap()[0].ends_with("from 0x16"));
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
        env(&[("HTTP_PROXY", "upper"), ("http_proxy", "lower"), ("PATH", "/bin")]),
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

#[test]
fn the_network_clock_runs_and_dates_are_local() {
    let mut net = net(&[]);
    let before = net.monotonic();
    net.sleep(0.01);
    assert!(net.monotonic() >= before + 0.01);
    let today = net.today();
    assert_eq!(today.len(), 10);
    assert!(py::Date::fromisoformat(&today).is_some(), "{today}");
    let now = net.utc_now();
    assert!(now.ends_with('Z') && now.len() == 20, "{now}");
    assert!(now.as_str() > "2026-01-01T00:00:00Z", "{now}");
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
    assert_eq!(fake.exchange("u", 1, &mut request).unwrap_err(), failure("URLError"));
    assert_eq!(
        fake.download("u", "p", 1, None, true, &mut request).unwrap_err(),
        failure("URLError")
    );
    assert_eq!(fake.monotonic(), 0.0);
    assert_eq!(fake.utc_now(), "1970-01-01T00:00:00Z");
    assert_eq!(fake.today(), "2026-11-28");
}

#[test]
fn fetch_json_stops_the_stage_on_the_errors_it_does_not_catch() {
    let uncaught = |class| Err(Failure::Uncaught { class, message: "m".into() });
    let mut fake = Scripted(vec![uncaught("UnicodeEncodeError")]);
    assert!(matches!(fetch_json(&mut fake, "u"), Err(Exc::UnicodeEncode(m)) if m == "m"));
    let mut fake = Scripted(vec![uncaught("ValueError")]);
    assert!(matches!(
        fetch_json(&mut fake, "u"),
        Err(Exc::Value { class: "ValueError", .. })
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
