// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Bounded retry for credential-less Git smart-HTTP reads.
//!
//! Git keeps an HTTP response's `Retry-After` out of its ordinary diagnostic. Callers therefore
//! receive a private, headers-only curl trace path for each attempt. The trace is redacted and
//! deleted here; it can narrow a failure to the final HTTP status and valid `Retry-After`, but
//! never broadens arbitrary remote prose into transport evidence.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use aterm_types::http_retry::{Cause, GitObservation, Policy, Retry, RunError};

/// Evidence from one failed Git read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    kind: FailureKind,
    words: String,
    observation: GitObservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    /// Git's stderr, classified only by [`Cause::from_git_diagnostic`].
    Diagnostic,
    /// A caller-observed child deadline, not prose containing the word "timeout".
    Timeout,
    /// Spawn, local I/O, or another failure no repeated network read can repair.
    Local,
}

impl Failure {
    /// A failed Git child's unmodified stderr.
    #[must_use]
    pub fn diagnostic(words: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Diagnostic,
            words: words.into(),
            observation: GitObservation::default(),
        }
    }

    /// A child the caller's hang detector ended.
    #[must_use]
    pub fn timeout(words: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Timeout,
            words: words.into(),
            observation: GitObservation::default(),
        }
    }

    /// A local or setup failure. This is always final.
    #[must_use]
    pub fn local(words: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Local,
            words: words.into(),
            observation: GitObservation::default(),
        }
    }

    fn with_observation(mut self, observation: GitObservation) -> Self {
        self.observation = observation;
        self
    }

    fn cause(&self) -> Option<Cause> {
        match self.kind {
            FailureKind::Diagnostic => self.observation.cause(&self.words),
            FailureKind::Timeout => match self.observation.status() {
                Some(400..=499) => None,
                _ => self.observation.cause("").or(Some(Cause::Timeout)),
            },
            FailureKind::Local => None,
        }
    }

    /// The failed child's diagnostic.
    #[must_use]
    pub fn words(&self) -> &str {
        &self.words
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words)
    }
}

/// One attempt's private Git curl trace.
#[derive(Clone, Copy, Debug)]
pub struct Trace<'a> {
    path: Option<&'a Path>,
}

impl Trace<'_> {
    /// Configure a Git child to write only redacted HTTP headers and to ask libcurl to end a
    /// connected transfer after 60 seconds below one byte/second. A transfer that keeps making
    /// progress has no latency deadline; the shared policy separately bounds retry pauses to 200
    /// seconds. Callers may add a process-progress watchdog around Git itself.
    pub fn configure(&self, command: &mut aterm_uds::spawnfd::Command) {
        for (name, value) in self.environment() {
            command.env(name, value);
        }
    }

    /// The same environment for an injected Git runner.
    #[must_use]
    pub fn environment(&self) -> Vec<(&'static str, String)> {
        let Some(path) = self.path else {
            return Vec::new();
        };
        vec![
            ("GIT_TRACE_CURL", path.to_string_lossy().into_owned()),
            ("GIT_TRACE_CURL_NO_DATA", "1".to_string()),
            ("GIT_TRACE_REDACT", "1".to_string()),
            ("GIT_HTTP_LOW_SPEED_LIMIT", "1".to_string()),
            ("GIT_HTTP_LOW_SPEED_TIME", "60".to_string()),
            ("GIT_TERMINAL_PROMPT", "0".to_string()),
        ]
    }

    /// Record a headers-only, redacted Git curl trace relayed by another host. The caller must
    /// have set `GIT_TRACE_CURL_NO_DATA=1` and `GIT_TRACE_REDACT=1` on that Git child.
    ///
    /// # Errors
    /// The private local trace cannot be written, or this is a one-shot non-HTTPS read with no
    /// trace scratch.
    pub fn record_redacted_headers(&self, headers: &[u8]) -> Result<(), String> {
        let path = self
            .path
            .ok_or("anonymous Git HTTPS trace scratch is unavailable")?;
        std::fs::write(path, headers)
            .map_err(|error| format!("write relayed Git HTTP trace: {error}"))
    }

    /// The trace path, for an injected transport fixture.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path
    }
}

/// Whether `url` spells a credential-less HTTPS remote rather than SSH, file, userinfo, or a
/// malformed URL. Callers still decide that the repository is intended to be public.
#[must_use]
pub fn is_anonymous_https_url(url: &str) -> bool {
    let Some(scheme) = url.get(.."https://".len()) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https://") {
        return false;
    }
    let rest = &url["https://".len()..];
    let authority = rest.split('/').next().unwrap_or_default();
    !authority.is_empty()
        && !authority.contains('@')
        && !url
            .bytes()
            .any(|byte| byte <= b' ' || byte == 0x7f || matches!(byte, b'?' | b'#' | b'\\'))
}

/// Run an injected idempotent Git read under the shared anonymous-HTTPS policy.
///
/// `anonymous_https` is an authority decision made by the caller. When false, exactly one attempt
/// runs with no trace. When true, each retry is logged here; `on_retry` performs the caller's wait
/// (and may additionally put the record in a release transcript).
pub fn read<T>(
    anonymous_https: bool,
    attempt: impl FnMut(u32, Trace<'_>) -> Result<T, Failure>,
    on_retry: impl FnMut(Retry),
) -> Result<T, RunError<Failure>> {
    read_with_scratch(
        anonymous_https,
        || {
            aterm_tempfile::Builder::new()
                .prefix("git-http-")
                .suffix(".noindex")
                .tempdir()
        },
        attempt,
        on_retry,
    )
}

fn read_with_scratch<T>(
    anonymous_https: bool,
    make_scratch: impl FnOnce() -> std::io::Result<aterm_tempfile::TempDir>,
    mut attempt: impl FnMut(u32, Trace<'_>) -> Result<T, Failure>,
    mut on_retry: impl FnMut(Retry),
) -> Result<T, RunError<Failure>> {
    let policy = if anonymous_https {
        Policy::STANDARD
    } else {
        Policy::NONE
    };
    let scratch = if anonymous_https {
        Some(make_scratch().map_err(|error| RunError {
            error: Failure::local(format!(
                "cannot create private Git HTTP trace scratch: {error}"
            )),
            attempts: 0,
            exhausted: false,
            bound: policy.bound(),
        })?)
    } else {
        None
    };
    let trace_path = scratch.as_ref().map(|dir| dir.path().join("headers.trace"));
    aterm_types::http_retry::run(
        policy,
        |number| {
            if let Some(path) = &trace_path {
                let _ = std::fs::remove_file(path);
            }
            let trace = Trace {
                path: trace_path.as_deref(),
            };
            attempt(number, trace).map_err(|failure| {
                failure
                    .with_observation(trace_path.as_deref().map(observe_trace).unwrap_or_default())
            })
        },
        |failure| anonymous_https.then(|| failure.cause()).flatten(),
        |retry| {
            crate::http::note_retry(format!("anonymous Git HTTPS read: {retry}"));
            on_retry(retry);
        },
    )
}

/// Read only the tail of Git's headers-only trace, then remove it. A trace failure removes
/// evidence; it never turns another class into a retry.
fn observe_trace(path: &Path) -> GitObservation {
    const KEEP: u64 = 256 * 1024;
    let mut bytes = Vec::new();
    let read = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        if len > KEEP {
            file.seek(SeekFrom::Start(len - KEEP))?;
        }
        file.read_to_end(&mut bytes)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(path);
    if read.is_err() {
        return GitObservation::default();
    }
    GitObservation::from_trace_at(&String::from_utf8_lossy(&bytes), aterm_time::unix_secs())
}

#[cfg(test)]
#[path = "git_https_tests.rs"]
mod tests;
