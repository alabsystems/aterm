// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ordinary native GitHub client, with one bounded child per request.

use super::remote::{Answer, Api, ApiError};
use crate::{Error, Result};
use aterm_json::Value;
use aterm_uds::spawnfd::{Command, Stdio};
use aterm_update_core::github_rest::{self, Client, Method, Origins, Spawn, Token};
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub(super) type Current = Arc<dyn Fn() -> Result<()> + Send + Sync>;
// A complete release page includes every asset's metadata. Keep this below the
// transport's bound while allowing the ordinary hundred-release page to fit.
const JSON_CAP: usize = 8 * 1024 * 1024;

struct Until {
    deadline: Instant,
    current: Current,
}

impl Spawn for Until {
    fn run(&self, command: &mut Command, bytes: Option<&[u8]>) -> io::Result<std::process::Output> {
        if Instant::now() >= self.deadline {
            return Err(github_rest::unstarted(io::Error::new(
                io::ErrorKind::Interrupted,
                "probe deadline",
            )));
        }
        (self.current)().map_err(|_| {
            github_rest::unstarted(io::Error::new(
                io::ErrorKind::Interrupted,
                "probe authorization changed",
            ))
        })?;
        // Preload a bounded anonymous socket before starting curl. Nonblocking writes
        // refuse a full buffer before any request, so there is no feeder thread to outlive
        // the deadline and no credential file or credential-bearing argv.
        let input = if let Some(bytes) = bytes {
            if bytes.len() > 16 * 1024 + 64 {
                return Err(github_rest::unstarted(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "probe header cap",
                )));
            }
            let (reader, mut writer) = UnixStream::pair().map_err(not_started)?;
            writer.set_nonblocking(true).map_err(not_started)?;
            writer.write_all(bytes).map_err(not_started)?;
            writer
                .shutdown(std::net::Shutdown::Write)
                .map_err(not_started)?;
            Stdio::from(OwnedFd::from(reader))
        } else {
            Stdio::null()
        };
        if Instant::now() >= self.deadline {
            return Err(not_started(io::Error::new(
                io::ErrorKind::Interrupted,
                "probe deadline",
            )));
        }
        let started = std::cell::Cell::new(false);
        aterm_uds::deadline::output_until_complete_stdout_from_observed(
            command,
            input,
            self.deadline,
            || started.set(true),
        )
        .map_err(|error| match error {
            aterm_uds::deadline::CompleteOutputError::Run(
                aterm_uds::deadline::RunError::Spawn(error),
            ) if !started.get() => {
                github_rest::unstarted(io::Error::new(io::ErrorKind::Interrupted, error))
            }
            _ => io::Error::new(
                io::ErrorKind::TimedOut,
                "bounded probe request did not complete",
            ),
        })
    }
}

pub(super) struct Http(Client);

impl Http {
    pub(super) fn new(
        token: &crate::SecretBytes,
        deadline: Instant,
        current: Current,
    ) -> Result<Self> {
        let token = Token::new(token.as_slice())
            .map_err(|_| Error::new("probe GitHub credential is invalid"))?;
        Ok(Self(
            Client::new(Origins::github(), Some(token))
                .with_spawn(Arc::new(Until { deadline, current })),
        ))
    }

    pub(super) fn identity(&self) -> Result<String> {
        let answer = self
            .request(Method::Get, "/user", None)
            .inspect_err(|error| {
                crate::log_line(&format!(
                    "aterm release probe: read stage=github-identity transport={error:?}"
                ));
            })?;
        if answer.status != 200 {
            crate::log_line(&format!(
                "aterm release probe: read stage=github-identity status={}",
                answer.status
            ));
        }
        answer
            .body
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| super::model::owner(login) && answer.status == 200)
            .map(str::to_owned)
            .ok_or_else(|| Error::new("probe GitHub identity was not authenticated"))
    }

    pub(super) fn admits(&self, owner: &str) -> Result<()> {
        let answer = self
            .request(Method::Get, &format!("/repos/{owner}/aterm"), None)
            .inspect_err(|error| {
                crate::log_line(&format!(
                    "aterm release probe: read stage=github-permission transport={error:?}"
                ));
            })?;
        if answer.status != 200
            || answer
                .body
                .get("full_name")
                .and_then(Value::as_str)
                .is_none_or(|name| !name.eq_ignore_ascii_case(&format!("{owner}/aterm")))
            || answer
                .body
                .get("permissions")
                .and_then(|p| p.get("push"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            crate::log_line(&format!(
                "aterm release probe: read stage=github-permission status={}",
                answer.status
            ));
            return Err(Error::new(
                "probe GitHub repository write permission is not current",
            ));
        }
        Ok(())
    }
}

impl Api for Http {
    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<Answer, ApiError> {
        let answer = self.0.send(method, path, body).map_err(api_error)?;
        decode_response(answer.status, &answer.body, true)
    }

    fn upload(&self, path: &str, file: &Path) -> std::result::Result<Answer, ApiError> {
        let answer = self
            .0
            .upload(&self.0.uploads(path), file)
            .map_err(api_error)?;
        decode_response(answer.status, &answer.body, false)
    }

    fn download(&self, path: &str, file: &Path, cap: u64) -> Result<()> {
        self.0
            .download_asset(path, file, cap)
            .map_err(|_| Error::new("bounded probe asset download failed"))
    }
}

fn decode_response(
    status: u16,
    bytes: &[u8],
    empty_is_null: bool,
) -> std::result::Result<Answer, ApiError> {
    if bytes.len() > JSON_CAP {
        crate::log_line(&format!(
            "aterm release probe: read stage=github-response-size bytes={} limit={JSON_CAP}",
            bytes.len()
        ));
        return Err(ApiError::Uncertain);
    }
    let body = if empty_is_null && bytes.is_empty() {
        Value::Null
    } else {
        aterm_json::from_slice(bytes).map_err(|_| ApiError::Uncertain)?
    };
    Ok(Answer { status, body })
}

fn api_error(error: github_rest::RestError) -> ApiError {
    match error {
        github_rest::RestError::Local(_) => ApiError::NotStarted,
        _ => ApiError::Uncertain,
    }
}

fn not_started(error: io::Error) -> io::Error {
    github_rest::unstarted(io::Error::new(io::ErrorKind::Interrupted, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_responses_above_one_mib_are_decoded_by_both_paths() {
        let value = aterm_json::json!({"body":"x".repeat(1024 * 1024 + 1)});
        let bytes = aterm_json::to_vec(&value).unwrap();
        for empty_is_null in [true, false] {
            let answer = decode_response(200, &bytes, empty_is_null).unwrap();
            assert_eq!(answer.status, 200);
            assert_eq!(answer.body, value);
        }
    }

    #[test]
    fn response_cap_is_inclusive_and_empty_body_semantics_are_preserved() {
        let mut bytes = vec![b' '; JSON_CAP];
        bytes[..2].copy_from_slice(b"[]");
        for empty_is_null in [true, false] {
            assert_eq!(
                decode_response(200, &bytes, empty_is_null).unwrap().body,
                aterm_json::json!([])
            );
            bytes.push(b' ');
            assert!(matches!(
                decode_response(200, &bytes, empty_is_null),
                Err(ApiError::Uncertain)
            ));
            bytes.pop();
        }
        assert_eq!(decode_response(204, b"", true).unwrap().body, Value::Null);
        assert!(matches!(
            decode_response(201, b"", false),
            Err(ApiError::Uncertain)
        ));
        assert!(matches!(
            decode_response(200, b"not-json", true),
            Err(ApiError::Uncertain)
        ));
    }

    #[test]
    fn revoked_before_spawn_stays_typed_not_started() {
        let token = crate::SecretBytes::from(&b"synthetic-test-token"[..]);
        let client = Http::new(
            &token,
            Instant::now() + std::time::Duration::from_secs(60),
            Arc::new(|| Err(Error::new("fixture revoked"))),
        )
        .unwrap();
        assert!(matches!(
            client.request(
                Method::Post,
                "/repos/fixture/aterm/releases",
                Some(&aterm_json::json!({"draft":true}))
            ),
            Err(ApiError::NotStarted)
        ));
    }
}
