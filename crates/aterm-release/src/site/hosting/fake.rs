// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A loopback fake of Google's token endpoint and Firebase Hosting's REST API, for the
//! deploy's tests: HTTP/1.1 on 127.0.0.1, one connection at a time, every request recorded.
//! The token endpoint verifies each assertion's RS256 signature against the test key's
//! public half and its claims; every Hosting call must carry the token it issued.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aterm_json::Value;

use super::rest::Origins;

/// The access token the fake issues.
pub const TOKEN: &str = "ya29.fake-hosting-token-4c1d";

/// One request the fake answered.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    /// The path and query, as sent.
    pub target: String,
    /// The headers, names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// What a test can change and read back.
#[derive(Default)]
pub struct State {
    /// Every request, in order.
    pub seen: Vec<Seen>,
    /// Hashes Hosting already holds: populateFiles never asks for them.
    pub held: BTreeSet<String>,
    /// Members uploaded, by hash.
    pub uploaded: BTreeMap<String, Vec<u8>>,
    /// The config each created version carried, by version name.
    pub configs: BTreeMap<String, Value>,
    /// The files each version was populated with, by version name.
    pub populated: BTreeMap<String, BTreeMap<String, String>>,
    /// The version the live site serves.
    pub live: Option<String>,
    /// Statuses to answer instead: the first route whose name a key starts is answered by it.
    pub faults: BTreeMap<&'static str, u16>,
    /// The `uploadUrl` populateFiles answers, when not the fake's own.
    pub upload_url: Option<String>,
    /// The issuer the token endpoint accepts.
    pub issuer: String,
    /// The status the token endpoint answers a good grant with, when not 200 (the token in
    /// the body either way).
    pub token_status: Option<u16>,
    versions: u32,
}

/// The fake, serving until dropped.
pub struct Fake {
    pub origins: Origins,
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fake {
    /// A fake whose token endpoint accepts assertions from `issuer` signed by the key whose
    /// DER RSAPublicKey is `public`.
    pub fn start(public: Vec<u8>, issuer: &str) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let origins = Origins {
            token: format!("{base}/token"),
            api: base.clone(),
            upload: format!("{base}/upload"),
        };
        let state = Arc::new(Mutex::new(State {
            issuer: issuer.to_string(),
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stop, origins) = (Arc::clone(&state), Arc::clone(&stop), origins.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        serve(stream, &state, &origins, &public);
                    }
                }
            })
        };
        Fake {
            origins,
            state,
            stop,
            port,
            thread: Some(thread),
        }
    }

    /// The state, locked.
    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The requests seen, as `METHOD target` lines, the upload hashes cut to `upload`.
    pub fn calls(&self) -> Vec<String> {
        self.state()
            .seen
            .iter()
            .map(|s| {
                let target = match s.target.find("/upload/") {
                    Some(_) => "upload".to_string(),
                    None => s.target.clone(),
                };
                format!("{} {target}", s.method)
            })
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One request off `stream`, answered.
fn serve(stream: TcpStream, state: &Mutex<State>, origins: &Origins, public: &[u8]) {
    let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (
        parts.next().unwrap_or("").to_string(),
        parts.next().unwrap_or("").to_string(),
    );
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() {
            return;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    if header("expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
        let _ = writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
    let length = header("content-length")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let mut st = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (status, answer) = route(&mut st, origins, public, &method, &target, &header, &body);
    st.seen.push(Seen {
        method,
        target,
        headers: headers.clone(),
        body,
    });
    drop(st);
    let text = answer.to_string();
    let _ = write!(
        writer,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = writer.flush();
}

/// The route `method` `target` names, for fault matching.
fn name(method: &str, path: &str) -> &'static str {
    if path == "/token" {
        "token"
    } else if path.starts_with("/upload/") {
        "upload"
    } else if path.ends_with(":populateFiles") {
        "populate"
    } else if method == "PATCH" {
        "finalize"
    } else if path.ends_with("/releases") {
        "release"
    } else if path.ends_with("/versions") {
        "create"
    } else {
        "other"
    }
}

fn error(status: u16, message: &str) -> (u16, Value) {
    (
        status,
        aterm_json::json!({"error": {"code": status, "message": message, "status": "FAKE"}}),
    )
}

#[allow(clippy::too_many_lines)]
fn route(
    st: &mut State,
    origins: &Origins,
    public: &[u8],
    method: &str,
    target: &str,
    header: &dyn Fn(&str) -> Option<String>,
    body: &[u8],
) -> (u16, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let route = name(method, path);
    if let Some(status) = st.faults.get(route) {
        return error(*status, &format!("fault injected on {route}"));
    }
    if route == "token" {
        let form = String::from_utf8_lossy(body);
        let mut grant = None;
        let mut assertion = None;
        for pair in form.split('&') {
            match pair.split_once('=') {
                Some(("grant_type", v)) => grant = Some(v.to_string()),
                Some(("assertion", v)) => assertion = Some(v.to_string()),
                _ => {}
            }
        }
        let refused = (
            400,
            aterm_json::json!({"error": "invalid_grant", "error_description": "Invalid JWT Signature."}),
        );
        if method != "POST"
            || grant.as_deref() != Some("urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer")
        {
            return refused;
        }
        let Some(claims) = assertion.and_then(|a| super::key::tests::verified(&a, public)) else {
            return refused;
        };
        let good = claims.get("aud").and_then(Value::as_str) == Some(origins.token.as_str())
            && claims.get("iss").and_then(Value::as_str) == Some(st.issuer.as_str())
            && claims.get("scope").and_then(Value::as_str) == Some(super::key::SCOPE)
            && claims
                .get("exp")
                .and_then(Value::as_u64)
                .zip(claims.get("iat").and_then(Value::as_u64))
                .is_some_and(|(exp, iat)| exp > iat && exp - iat <= 3600);
        if !good {
            return refused;
        }
        return (
            st.token_status.unwrap_or(200),
            aterm_json::json!({"access_token": TOKEN, "expires_in": 3599, "token_type": "Bearer"}),
        );
    }
    if header("authorization").as_deref() != Some(&format!("Bearer {TOKEN}")) {
        return error(401, "Request had invalid authentication credentials.");
    }
    let json: Option<Value> = aterm_json::from_slice(body).ok();
    match (method, route) {
        ("POST", "create") => {
            let Some(site) = path
                .strip_prefix("/v1beta1/sites/")
                .and_then(|r| r.strip_suffix("/versions"))
            else {
                return error(404, "no such site");
            };
            st.versions += 1;
            let name = format!("sites/{site}/versions/v{}", st.versions);
            let config = json
                .as_ref()
                .and_then(|j| j.get("config").cloned())
                .unwrap_or(Value::Null);
            st.configs.insert(name.clone(), config);
            (200, aterm_json::json!({"name": name, "status": "CREATED"}))
        }
        ("POST", "populate") => {
            let version = path
                .trim_start_matches("/v1beta1/")
                .trim_end_matches(":populateFiles")
                .to_string();
            if !st.configs.contains_key(&version) {
                return error(404, "no such version");
            }
            let files: BTreeMap<String, String> = json
                .as_ref()
                .and_then(|j| j.get("files"))
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|h| (k.clone(), h.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let asked: BTreeSet<String> = files
                .values()
                .filter(|h| !st.held.contains(*h) && !st.uploaded.contains_key(*h))
                .cloned()
                .collect();
            st.populated
                .entry(version.clone())
                .or_default()
                .extend(files);
            let url = st
                .upload_url
                .clone()
                .unwrap_or_else(|| format!("{}/{version}/files", origins.upload));
            (
                200,
                aterm_json::json!({
                    "uploadRequiredHashes": asked.into_iter().collect::<Vec<_>>(),
                    "uploadUrl": url,
                }),
            )
        }
        ("POST", "upload") => {
            let Some(hash) = path.rsplit('/').next() else {
                return error(404, "no hash");
            };
            let sum = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(body));
            if sum != hash {
                return error(400, "the body does not hash to its name");
            }
            st.uploaded.insert(hash.to_string(), body.to_vec());
            (200, aterm_json::json!({}))
        }
        ("PATCH", "finalize") => {
            let version = path.trim_start_matches("/v1beta1/").to_string();
            let asked = json
                .as_ref()
                .and_then(|j| j.get("status"))
                .and_then(Value::as_str);
            if query != "updateMask=status" || asked != Some("FINALIZED") {
                return error(400, "only status=FINALIZED is patched");
            }
            let Some(files) = st.populated.get(&version) else {
                return error(404, "no such version");
            };
            let missing = files
                .values()
                .any(|h| !st.held.contains(h) && !st.uploaded.contains_key(h));
            if missing {
                return error(400, "a file of the version was never uploaded");
            }
            (
                200,
                aterm_json::json!({"name": version, "status": "FINALIZED"}),
            )
        }
        ("POST", "release") => {
            let version = query
                .strip_prefix("versionName=")
                .map(|v| v.replace("%2F", "/"))
                .unwrap_or_default();
            if !st.configs.contains_key(&version) {
                return error(400, "no such version");
            }
            let site = path
                .trim_start_matches("/v1beta1/sites/")
                .trim_end_matches("/releases");
            st.live = Some(version.clone());
            (
                200,
                aterm_json::json!({"name": format!("sites/{site}/releases/r1"), "version": {"name": version}}),
            )
        }
        _ => error(404, "no such route"),
    }
}
