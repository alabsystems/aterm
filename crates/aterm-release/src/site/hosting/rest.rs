// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Firebase Hosting's REST API and Google's token endpoint, over the release's curl
//! ([`aterm_update_core::github_rest::curl`]: `/usr/bin/curl -q`, its configuration and
//! credential variables removed). The bearer token rides curl's stdin as `-H @-`; the token
//! request's assertion rides it as `--data-binary @-`; each answer comes back on stdout. No
//! credential is ever in an argument, the environment, or a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_json::Value;
use aterm_uds::secret::Secret;
use aterm_update_core::github_rest::Spawn;

use super::Failed;
use super::key::Key;

/// How long curl may take to connect.
const CONNECT_SECS: &str = "15";

/// How long one API call may take.
const API_SECS: &str = "120";

/// An upload stalls when it moves fewer than this many bytes a second…
const STALL_BYTES: &str = "1024";

/// …for this many seconds.
const STALL_SECS: &str = "60";

/// The largest answer read: Hosting's and the token endpoint's are a few kilobytes.
const ANSWER_CAP: &str = "8388608";

/// Where the calls go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origins {
    /// The one token endpoint a key's `token_uri` may name.
    pub token: String,
    /// The Hosting API.
    pub api: String,
    /// The origin `populateFiles`' `uploadUrl` must be on: the bearer token goes there.
    pub upload: String,
}

impl Origins {
    /// Google's.
    #[must_use]
    pub fn google() -> Origins {
        Origins {
            token: "https://oauth2.googleapis.com/token".into(),
            api: "https://firebasehosting.googleapis.com".into(),
            upload: "https://upload-firebasehosting.googleapis.com".into(),
        }
    }

    /// Whether every origin is HTTPS, so curl is held to it (`--proto =https`).
    fn https(&self) -> bool {
        [&self.token, &self.api, &self.upload]
            .iter()
            .all(|o| o.starts_with("https://"))
    }

    /// Whether `url` is on `origin`: the origin, then a `/`, and no byte curl would read
    /// otherwise than as a path.
    #[must_use]
    pub fn on(origin: &str, url: &str) -> bool {
        url.strip_prefix(origin)
            .is_some_and(|rest| rest.starts_with('/'))
            && url.bytes().all(|b| b > b' ' && b != 0x7f && b != b'\\')
    }

    /// `token_uri` when it is exactly [`Origins::token`].
    /// # Errors
    /// Any other endpoint, named: the signed assertion is a credential, and goes nowhere else.
    pub fn admit<'k>(&self, token_uri: &'k str) -> Result<&'k str, String> {
        if token_uri == self.token {
            Ok(token_uri)
        } else {
            Err(format!(
                "the service-account key's token_uri is {token_uri:?}; ship site exchanges a \
                 key only at {}",
                self.token
            ))
        }
    }
}

/// A request's body.
#[derive(Clone, Copy)]
pub enum Body<'a> {
    /// JSON, from a file in the client's private scratch directory.
    Json(&'a Value),
    /// Bytes to upload, from a file in the scratch directory.
    Bytes(&'a [u8]),
}

/// What an endpoint answered.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The body.
    pub body: Vec<u8>,
}

impl Answer {
    /// The body as JSON when the status is 2xx.
    /// # Errors
    /// Any other status, said with the API's own message; a 2xx that is not JSON.
    pub fn json(&self) -> Result<Value, String> {
        if !(200..300).contains(&self.status) {
            return Err(self.said());
        }
        aterm_json::from_slice(&self.body)
            .map_err(|e| format!("answered {} with a body that is not JSON: {e}", self.status))
    }

    /// The status and what the body says of it: Google's `error.message` (or the token
    /// endpoint's `error` and `error_description`), else its first line.
    #[must_use]
    pub fn said(&self) -> String {
        said(self.status, &self.body, true)
    }
}

/// `status` and what `body` says of it: its `error` fields, else (when `first_line`) the
/// first line of the body, else nothing more.
fn said(status: u16, body: &[u8], first_line: bool) -> String {
    let doc: Option<Value> = aterm_json::from_slice(body).ok();
    let message = doc
        .as_ref()
        .and_then(|d| match (d.get("error"), d.get("error_description")) {
            (Some(Value::Object(e)), _) => {
                e.get("message").and_then(Value::as_str).map(str::to_string)
            }
            (Some(Value::String(e)), Some(Value::String(d))) => Some(format!("{e}: {d}")),
            (Some(Value::String(e)), _) => Some(e.clone()),
            _ => None,
        });
    let message = message.unwrap_or_else(|| {
        if first_line {
            String::from_utf8_lossy(body)
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect()
        } else {
            String::new()
        }
    });
    if message.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {message}")
    }
}

/// Why a call has no answer, and whether its request can have reached the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// Nothing was sent: the request was refused here, its body could not be written, curl
    /// never started, or curl never reached the endpoint (it could not resolve or connect
    /// to it, or the TLS handshake failed).
    Unsent(String),
    /// curl started and may have sent the request before it failed.
    Lost(String),
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsent(why) | Self::Lost(why) => f.write_str(why),
        }
    }
}

/// curl's exits that say it never sent a request: an unsupported protocol (1), a malformed
/// URL (3), a proxy or host it could not resolve (5, 6), a connection it could not make (7),
/// and a TLS handshake that failed or a certificate it refused (35, 60), all before a byte of
/// HTTP goes out.
const NEVER_SENT: [i32; 7] = [1, 3, 5, 6, 7, 35, 60];

/// An access token, held as the header line curl reads from stdin.
pub struct Bearer(Secret);

impl std::fmt::Debug for Bearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bearer(<redacted>)")
    }
}

/// The client for one deploy: its origins, how it starts curl, and a private scratch
/// directory (0700) for request bodies, removed when the client is dropped.
pub struct Client {
    origins: Origins,
    spawn: Arc<dyn Spawn>,
    scratch: PathBuf,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

impl Client {
    /// A client calling `origins`, its curls started by `spawn`, its scratch directory made
    /// under `scratch_parent`.
    /// # Errors
    /// The scratch directory cannot be made.
    pub fn new(
        origins: Origins,
        spawn: Arc<dyn Spawn>,
        scratch_parent: &Path,
    ) -> Result<Client, String> {
        let scratch = crate::site::private_dir_in(scratch_parent, "aterm-hosting")?;
        Ok(Client {
            origins,
            spawn,
            scratch,
        })
    }

    /// The origins this client calls.
    #[must_use]
    pub const fn origins(&self) -> &Origins {
        &self.origins
    }

    /// The URL of `path` on the API origin.
    #[must_use]
    pub fn api(&self, path: &str) -> String {
        format!("{}/{}", self.origins.api, path.trim_start_matches('/'))
    }

    /// An access token for `key`, exchanged at its admitted `token_uri` (now as `now`).
    /// # Errors
    /// [`Failed::Credential`]: the `token_uri` is not admitted (nothing is spawned), the key
    /// cannot sign, or the endpoint refuses it (400, 401, 403, in its own words).
    /// [`Failed::NotReleased`]: curl reaches no answer, the endpoint fails otherwise, or it
    /// answers no usable token. An answer is said by its status and its `error` fields only,
    /// never its other bytes: a success's body is the token.
    pub fn token(&self, key: &Key, now: u64) -> Result<Bearer, Failed> {
        let unanswered = |why: String| Failed::NotReleased {
            why: format!("asking for a token: {why}"),
            version: None,
        };
        let url = self
            .origins
            .admit(&key.token_uri)
            .map_err(Failed::Credential)?;
        let grant = key.grant(now).map_err(Failed::Credential)?;
        let args = [
            "-X",
            "POST",
            "-H",
            "Content-Type: application/x-www-form-urlencoded",
            "-H",
            "Expect:",
            "--data-binary",
            "@-",
        ];
        let answer = self
            .run(url, &args, Some(&grant))
            .map_err(|e| unanswered(e.to_string()))?;
        let status = answer.status;
        let body = Secret::from_vec(answer.body);
        if matches!(status, 400 | 401 | 403) {
            return Err(Failed::Credential(format!(
                "the token endpoint refused the service-account key {}: {}",
                key.email,
                said(status, body.expose(), false)
            )));
        }
        if status != 200 {
            return Err(unanswered(said(status, body.expose(), false)));
        }
        let token = match aterm_json::from_slice::<Value>(body.expose()) {
            Ok(Value::Object(mut doc)) => match doc.remove("access_token") {
                Some(Value::String(token)) => Some(Secret::from_vec(token.into_bytes())),
                _ => None,
            },
            _ => None,
        }
        .filter(|t| !t.is_empty() && t.expose().iter().all(|b| (0x21..=0x7e).contains(b)))
        .ok_or_else(|| unanswered("the token endpoint answered no usable access_token".into()))?;
        let mut line = Vec::with_capacity(token.len() + 24);
        line.extend_from_slice(b"Authorization: Bearer ");
        line.extend_from_slice(token.expose());
        line.push(b'\n');
        Ok(Bearer(Secret::from_vec(line)))
    }

    /// `method` `url` (on the API or upload origin) with `body`, authorised by `bearer`.
    /// # Errors
    /// [`Unanswered::Unsent`]: a URL off both origins (nothing is spawned), a body that cannot
    /// be written, a curl that never started or never reached the endpoint;
    /// [`Unanswered::Lost`]: a curl that may have sent the request and reached no answer.
    pub fn call(
        &self,
        method: &str,
        url: &str,
        body: Body<'_>,
        bearer: &Bearer,
    ) -> Result<Answer, Unanswered> {
        if !Origins::on(&self.origins.api, url) && !Origins::on(&self.origins.upload, url) {
            return Err(Unanswered::Unsent(format!(
                "refused a Hosting request off the API and upload origins: {url}"
            )));
        }
        let mut args: Vec<String> = vec!["-X".into(), method.into()];
        let file = |name: &str, bytes: &[u8]| -> Result<String, Unanswered> {
            let path = self.scratch.join(name);
            std::fs::write(&path, bytes).map_err(|e| {
                Unanswered::Unsent(format!(
                    "cannot write the request body {}: {e}",
                    path.display()
                ))
            })?;
            path.to_str()
                .map(|p| format!("@{p}"))
                .ok_or_else(|| Unanswered::Unsent(format!("{} is not UTF-8", path.display())))
        };
        let timed = match body {
            Body::Json(json) => {
                let bytes = aterm_json::to_vec(json)
                    .map_err(|e| Unanswered::Unsent(format!("cannot write the request: {e}")))?;
                args.extend([
                    "-H".into(),
                    "Content-Type: application/json".into(),
                    "-H".into(),
                    "Expect:".into(),
                    "--data-binary".into(),
                    file("request.json", &bytes)?,
                ]);
                true
            }
            Body::Bytes(bytes) => {
                args.extend([
                    "-H".into(),
                    "Content-Type: application/octet-stream".into(),
                    "-H".into(),
                    "Expect:".into(),
                    "--data-binary".into(),
                    file("upload.gz", bytes)?,
                ]);
                false
            }
        };
        args.extend(["-H".into(), "@-".into()]);
        if !timed {
            args.extend([
                "--speed-limit".into(),
                STALL_BYTES.into(),
                "--speed-time".into(),
                STALL_SECS.into(),
            ]);
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let answered = self.run(url, &argv, Some(&bearer.0));
        let _ = std::fs::remove_file(self.scratch.join("request.json"));
        let _ = std::fs::remove_file(self.scratch.join("upload.gz"));
        answered
    }

    /// curl `url` with `args` (an upload's carry its stall limits, which stand in for a
    /// total time), `stdin` on its standard input, the answer on its stdout and the
    /// status after it.
    fn run(&self, url: &str, args: &[&str], stdin: Option<&Secret>) -> Result<Answer, Unanswered> {
        let uploads = args.contains(&"--speed-limit");
        let mut argv: Vec<&str> = vec![
            "-sS",
            "--globoff",
            "--connect-timeout",
            CONNECT_SECS,
            "--max-filesize",
            ANSWER_CAP,
            "-w",
            "\n%{http_code}",
        ];
        if !uploads {
            argv.extend(["--max-time", API_SECS]);
        }
        if self.origins.https() {
            argv.extend(["--proto", "=https"]);
        }
        argv.extend_from_slice(args);
        let mut command = aterm_update_core::github_rest::curl(&argv, url);
        let out = self
            .spawn
            .run(&mut command, stdin.map(Secret::expose))
            .map_err(|e| {
                if aterm_update_core::github_rest::never_started(&e) {
                    Unanswered::Unsent(format!("cannot run curl: {e}"))
                } else {
                    Unanswered::Lost(format!("curl did not finish: {e}"))
                }
            })?;
        if !out.status.success() {
            let said = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            let why = format!(
                "curl reached no answer from {} (exit {code}): {}",
                origin(url),
                said.trim().lines().last().unwrap_or("")
            );
            return Err(if NEVER_SENT.contains(&code) {
                Unanswered::Unsent(why)
            } else {
                Unanswered::Lost(why)
            });
        }
        let mut stdout = out.stdout;
        let split = stdout.iter().rposition(|b| *b == b'\n');
        let status = split
            .and_then(|at| std::str::from_utf8(&stdout[at + 1..]).ok())
            .and_then(|code| code.trim().parse::<u16>().ok())
            .filter(|code| (100..600).contains(code))
            .ok_or_else(|| {
                Unanswered::Lost(format!("curl said no HTTP status for {}", origin(url)))
            })?;
        stdout.truncate(split.unwrap_or(0));
        Ok(Answer {
            status,
            body: stdout,
        })
    }
}

/// `url`'s scheme and host, for a message.
fn origin(url: &str) -> &str {
    let after = url.find("://").map_or(0, |i| i + 3);
    let end = url[after..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| after + i);
    &url[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only Google's own token endpoint is admitted for a key. NEGATIVE CONTROL: the exact
    /// endpoint is.
    #[test]
    fn only_googles_token_endpoint_is_admitted() {
        let google = Origins::google();
        assert_eq!(google.token, "https://oauth2.googleapis.com/token");
        assert!(google.admit("https://oauth2.googleapis.com/token").is_ok());
        for off in [
            "https://oauth2.googleapis.com.attacker.example/token",
            "https://oauth2.googleapis.com/token/",
            "https://oauth2.googleapis.com/token?x=1",
            "http://oauth2.googleapis.com/token",
            "https://accounts.google.com/o/oauth2/token",
            "https://attacker.example/token",
            "",
        ] {
            let why = google.admit(off).unwrap_err();
            assert!(why.contains("token_uri"), "{why}");
        }
    }

    /// A URL is on an origin only when the origin's own path follows. NEGATIVE CONTROL:
    /// one that is passes.
    #[test]
    fn a_url_is_on_an_origin_only_by_its_path() {
        let api = "https://firebasehosting.googleapis.com";
        assert!(Origins::on(
            api,
            "https://firebasehosting.googleapis.com/v1beta1/x"
        ));
        for off in [
            "https://firebasehosting.googleapis.com.attacker.example/v1beta1/x",
            "https://firebasehosting.googleapis.com@attacker.example/x",
            "https://firebasehosting.googleapis.com",
            "https://firebasehosting.googleapis.com/a b",
            "https://firebasehosting.googleapis.com/a\\b",
        ] {
            assert!(!Origins::on(api, off), "{off}");
        }
    }

    /// An error body is said in the API's own words. NEGATIVE CONTROL: a 2xx is JSON.
    #[test]
    fn an_answer_says_the_apis_own_message() {
        let hosting = Answer {
            status: 403,
            body: br#"{"error":{"code":403,"message":"The caller does not have permission","status":"PERMISSION_DENIED"}}"#.to_vec(),
        };
        assert_eq!(
            hosting.said(),
            "HTTP 403: The caller does not have permission"
        );
        assert!(hosting.json().is_err());
        let token = Answer {
            status: 400,
            body: br#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#
                .to_vec(),
        };
        assert_eq!(
            token.said(),
            "HTTP 400: invalid_grant: Invalid JWT Signature."
        );
        let ok = Answer {
            status: 200,
            body: b"{\"a\":1}".to_vec(),
        };
        assert_eq!(ok.json().unwrap(), aterm_json::json!({"a": 1}));
    }
}
