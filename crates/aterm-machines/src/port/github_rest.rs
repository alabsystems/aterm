// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machines controller's GitHub REST door (docs/DESIGN-init-system-2026-10-20.md S14, §6.1
//! row 22): one bounded request at a time over aterm-http to the API origin, with no gh.
//!
//! - The machine user's token goes into an `Authorization` header built in this process
//!   (aterm-http's scrub-on-drop secret header), never an argument, a variable or a log line.
//! - aterm-http follows no redirect, and this door refuses a 3xx answer outright: a followed
//!   redirect is how a bearer token reaches a host nobody named, so a redirect is never a
//!   second request.
//! - The body is bounded and read whole; one cut short (fewer bytes than its
//!   `Content-Length`) fails the request instead of becoming a shorter page.
//! - It carries the SSH-key writes (`POST` and `DELETE /user/keys`), which agh's M1
//!   transport refuses (agh design §16.4: step 5a moves them into `users().keys()`), and the
//!   reads beside them, so one controller session speaks through one client.

use std::time::Duration;

use aterm_http::{Client, ProxyMode, Trust};

use super::{GithubResponse, Secret};

/// The API origin every live request goes to.
pub(super) const API: &str = "https://api.github.com";

/// The bound on one answer's body: a key listing page of 100 is a few tens of KiB.
const RESPONSE_MAX: usize = 1 << 20;

/// The bound on one request, connect to last body byte.
const BUDGET: Duration = Duration::from_secs(60);

/// The REST version every request names (GitHub's `X-GitHub-Api-Version`).
const API_VERSION: &str = "2022-11-28";

/// A REST method the controller sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestMethod {
    /// A read.
    Get,
    /// A creation (`POST /user/keys`).
    Post,
    /// A deletion (`DELETE /user/keys/{id}`).
    Delete,
}

impl RestMethod {
    /// The method's token on the request line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }

    const fn http(self) -> aterm_http::Method {
        match self {
            Self::Get => aterm_http::Method::Get,
            Self::Post => aterm_http::Method::Post,
            Self::Delete => aterm_http::Method::Delete,
        }
    }
}

impl std::fmt::Display for RestMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The live client: the operating system's trust store, and the environment's proxy.
pub(super) fn live_client() -> Client {
    Client::new(Trust::PlatformVerifier, ProxyMode::Environment, BUDGET)
}

/// Send `method endpoint` to `origin` carrying `token`, with `body` as JSON when it is
/// given, and read the whole answer.
///
/// # Errors
/// An endpoint that is not one absolute path, a request that failed or was cut short, an
/// answer over the bound, or a redirect. No error carries the token.
pub(super) fn send(
    client: &Client,
    origin: &str,
    token: &Secret,
    method: RestMethod,
    endpoint: &str,
    body: Option<&[u8]>,
) -> Result<GithubResponse, String> {
    if !endpoint.starts_with('/')
        || endpoint.starts_with("//")
        || endpoint
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ' || byte == b'#')
    {
        return Err("invalid GitHub API endpoint".into());
    }
    let mut header = Vec::with_capacity(7 + token.bytes().len());
    header.extend_from_slice(b"Bearer ");
    header.extend_from_slice(token.bytes());
    let authorization = aterm_uds::secret::Secret::from_vec(header);
    let mut request = client
        .request(method.http(), &format!("{origin}{endpoint}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION)
        .header("User-Agent", "aterm-machines")
        .secret_header("Authorization", authorization)
        .limit(RESPONSE_MAX);
    if body.is_some() {
        request = request.header("Content-Type", "application/json");
    }
    let answer = request
        .send(body.unwrap_or_default())
        .map_err(|error| format!("GitHub {method} {endpoint}: {error}"))?;
    let status = answer.status();
    if (300..400).contains(&status) {
        return Err(format!(
            "GitHub {method} {endpoint} answered HTTP {status}, a redirect, which is never \
             followed with the machine user's token"
        ));
    }
    Ok(GithubResponse {
        status,
        headers: answer
            .headers()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        body: answer.body().to_vec(),
    })
}

/// A loopback GitHub for this crate's tests: a plain-HTTP listener on 127.0.0.1 that answers
/// each connection with the next scripted reply, raw, and records each request it read (its
/// request line, its headers and its body).
#[cfg(test)]
pub(crate) mod fixture {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    /// One request the fixture read.
    #[derive(Debug, Clone)]
    pub(crate) struct Seen {
        /// `GET /user/keys?per_page=100&page=1 HTTP/1.1`.
        pub(crate) line: String,
        /// Each header as sent, names lowercased.
        pub(crate) headers: Vec<(String, String)>,
        /// The body, by its `Content-Length`.
        pub(crate) body: Vec<u8>,
    }

    impl Seen {
        /// The first value of `name`.
        pub(crate) fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }
    }

    /// The listener and what it saw.
    pub(crate) struct Server {
        /// `http://127.0.0.1:<port>`.
        pub(crate) origin: String,
        seen: Arc<Mutex<Vec<Seen>>>,
        thread: Option<JoinHandle<()>>,
    }

    /// A complete JSON answer with `status`, `headers` and `body`.
    pub(crate) fn json(status: u16, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(body);
        out.into_bytes()
    }

    /// A head that declares `body`'s length plus `missing`, then only `body`: a page the
    /// connection ends inside.
    pub(crate) fn cut_short(body: &str, missing: usize) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len() + missing
        )
        .into_bytes()
    }

    impl Server {
        /// Serve `replies`, one connection each, in order; the listener stops after the last.
        pub(crate) fn start(replies: Vec<Vec<u8>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let record = Arc::clone(&seen);
            let thread = std::thread::spawn(move || {
                for reply in replies {
                    let Ok((stream, _)) = listener.accept() else {
                        return;
                    };
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    // An empty read is the drop's wake-up (or a client that hung up): stop.
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    let mut headers = Vec::new();
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                            break;
                        }
                        if let Some((name, value)) = header.trim_end().split_once(':') {
                            headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
                        }
                    }
                    let length = headers
                        .iter()
                        .find(|(name, _)| name == "content-length")
                        .and_then(|(_, value)| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    if reader.read_exact(&mut body).is_err() {
                        return;
                    }
                    record.lock().unwrap().push(Seen {
                        line: line.trim_end().to_owned(),
                        headers,
                        body,
                    });
                    let mut stream = reader.into_inner();
                    let _ = stream.write_all(&reply);
                    let _ = stream.flush();
                }
            });
            Self {
                origin,
                seen,
                thread: Some(thread),
            }
        }

        /// Every request read so far.
        pub(crate) fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // Unblock an accept still waiting for a connection no test will make.
            let _ = std::net::TcpStream::connect(self.origin.trim_start_matches("http://"));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{Server, cut_short, json};
    use super::*;

    const TOKEN: &[u8] = b"fixture_machine_user_token_only";

    fn loopback() -> Client {
        Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, BUDGET)
    }

    fn token() -> Secret {
        Secret::new(TOKEN).unwrap()
    }

    fn page(server: &Server, page: usize) -> Result<crate::github_pages::Page, String> {
        let reply = send(
            &loopback(),
            &server.origin,
            &token(),
            RestMethod::Get,
            &format!("/user/keys?per_page=2&page={page}"),
            None,
        )?;
        crate::github_pages::page(reply.status, reply.headers, &reply.body)
    }

    const NEXT: &str = "<https://api.github.com/user/keys?per_page=2&page=2>; rel=\"next\", <https://api.github.com/user/keys?per_page=2&page=2>; rel=\"last\"";

    /// Two pages read whole over a real socket, the token in the Authorization header alone.
    /// Negative control: the same listing whose second page ends inside its body is refused,
    /// not shortened.
    #[test]
    fn pagination_reads_every_page_and_a_page_cut_short_is_refused() {
        let server = Server::start(vec![
            json(200, &[("Link", NEXT)], r#"[{"id":1},{"id":2}]"#),
            json(200, &[], r#"[{"id":3}]"#),
        ]);
        let items = crate::github_pages::all("/user/keys?per_page=2", 2, |endpoint| {
            let page_no = endpoint.rsplit_once("page=").unwrap().1.parse().unwrap();
            page(&server, page_no)
        })
        .unwrap();
        assert_eq!(items.len(), 3);
        let seen = server.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].line, "GET /user/keys?per_page=2&page=2 HTTP/1.1");
        let bearer = format!("Bearer {}", std::str::from_utf8(TOKEN).unwrap());
        assert!(
            seen.iter()
                .all(|s| s.header("authorization") == Some(bearer.as_str()))
        );
        assert!(
            seen.iter()
                .all(|s| s.line.as_bytes().windows(TOKEN.len()).all(|w| w != TOKEN))
        );
        assert_eq!(seen[0].header("x-github-api-version"), Some(API_VERSION));

        let server = Server::start(vec![
            json(200, &[("Link", NEXT)], r#"[{"id":1},{"id":2}]"#),
            cut_short(r#"[{"id":3},"#, 9),
        ]);
        let why = crate::github_pages::all("/user/keys?per_page=2", 2, |endpoint| {
            let page_no = endpoint.rsplit_once("page=").unwrap().1.parse().unwrap();
            page(&server, page_no)
        })
        .unwrap_err();
        assert!(
            why.starts_with("GitHub GET /user/keys?per_page=2&page=2: "),
            "{why}"
        );
        assert!(!why.contains(std::str::from_utf8(TOKEN).unwrap()), "{why}");
    }

    /// A redirect is refused and never followed: the listener sees one request, and the
    /// place it named sees none. Negative control: the same request answered 200 is read.
    #[test]
    fn a_redirect_is_refused_and_never_followed() {
        let elsewhere = Server::start(vec![json(200, &[], "{}")]);
        for status in [301, 302, 307, 308] {
            let location = format!("{}/user", elsewhere.origin);
            let moved = json(status, &[("Location", &location)], "{}");
            let server = Server::start(vec![moved.clone(), moved]);
            for (method, body) in [
                (RestMethod::Get, None),
                (RestMethod::Post, Some(&b"{}"[..])),
            ] {
                let why =
                    send(&loopback(), &server.origin, &token(), method, "/user", body).unwrap_err();
                assert!(why.contains("a redirect"), "{why}");
            }
            assert_eq!(server.seen().len(), 2, "one request each, none repeated");
        }
        assert!(
            elsewhere.seen().is_empty(),
            "the redirect target was never asked"
        );
        let server = Server::start(vec![json(200, &[], r#"{"login":"fleetbot"}"#)]);
        let reply = send(
            &loopback(),
            &server.origin,
            &token(),
            RestMethod::Get,
            "/user",
            None,
        )
        .unwrap();
        assert_eq!(
            (reply.status, reply.body.as_slice()),
            (200, &br#"{"login":"fleetbot"}"#[..])
        );
    }

    #[test]
    fn a_write_carries_its_json_body_and_a_delete_none() {
        let server = Server::start(vec![json(201, &[], r#"{"id":9}"#), json(204, &[], "")]);
        let body = br#"{"title":"t","key":"k"}"#;
        let created = send(
            &loopback(),
            &server.origin,
            &token(),
            RestMethod::Post,
            "/user/keys",
            Some(body),
        )
        .unwrap();
        assert_eq!(created.status, 201);
        let deleted = send(
            &loopback(),
            &server.origin,
            &token(),
            RestMethod::Delete,
            "/user/keys/9",
            None,
        )
        .unwrap();
        assert_eq!(deleted.status, 204);
        let seen = server.seen();
        assert_eq!(seen[0].line, "POST /user/keys HTTP/1.1");
        assert_eq!(seen[0].body, body);
        assert_eq!(seen[0].header("content-type"), Some("application/json"));
        assert_eq!(seen[1].line, "DELETE /user/keys/9 HTTP/1.1");
        assert!(seen[1].body.is_empty());
    }

    /// Each endpoint that is not one path is refused by the guard itself, before a connection:
    /// a listener that answers any request sees none. Negative control: the same listener
    /// answers a plain path, so a request that got past the guard would show.
    #[test]
    fn an_endpoint_that_is_not_one_path_is_refused_before_any_connection() {
        let refused = [
            "https://elsewhere.invalid/user",
            "//elsewhere.invalid/user",
            "/user\n",
            "/user\r\nX-Injected: 1",
            "/user keys",
            "/user#fragment",
            "user",
            "",
        ];
        let server = Server::start(vec![json(200, &[], "{}"); refused.len() + 1]);
        for endpoint in refused {
            let why = send(
                &loopback(),
                &server.origin,
                &token(),
                RestMethod::Get,
                endpoint,
                None,
            )
            .unwrap_err();
            assert_eq!(why, "invalid GitHub API endpoint", "{endpoint:?}");
        }
        assert!(server.seen().is_empty(), "{:?}", server.seen());
        let reply = send(
            &loopback(),
            &server.origin,
            &token(),
            RestMethod::Get,
            "/user",
            None,
        )
        .unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(server.seen().len(), 1);
    }
}
