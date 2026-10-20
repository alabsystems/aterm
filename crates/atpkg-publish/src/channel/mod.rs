// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signed index and everything it pins.
//!
//! | module | does |
//! |---|---|
//! | [`index`] | `index`: the next signed index, a typed [`crate::api::Delta`] over the verified head |
//! | [`mirror`] | `mirror`: exactly the signed packs an index pins, made public |
//! | [`group`] | `publish-group rustc`: the trust bundle and its siblings from one seal |
//! | [`seed`] | the seed pin: the re-pin a SEED-MINT names is on the trust repo's main |
//! | [`status`] | `status`: the verified head, printed |
//!
//! The production release host is [`crate::host::gh`].

pub mod group;
pub mod index;
pub mod mirror;
pub mod seed;
pub mod status;

use std::path::Path;

use crate::error::{Error, Result};
use crate::host::{HostError, ReleaseHost, Slug};

/// A verb's transcript: every line it says, in order. A real run echoes each line as it
/// is said (a group publish takes an hour); a test captures them and reads them back.
#[derive(Debug)]
pub struct Log {
    verb: &'static str,
    echo: bool,
    lines: Vec<String>,
}

impl Log {
    /// A transcript printed as it is written (`<verb>: <line>`).
    #[must_use]
    pub fn echo(verb: &'static str) -> Self {
        Self {
            verb,
            echo: true,
            lines: Vec::new(),
        }
    }

    /// A transcript kept, never printed.
    #[must_use]
    pub fn capture(verb: &'static str) -> Self {
        Self {
            verb,
            echo: false,
            lines: Vec::new(),
        }
    }

    /// One line of progress (stdout).
    pub fn say(&mut self, line: impl Into<String>) {
        let line = line.into();
        if self.echo {
            println!("{}: {line}", self.verb);
        }
        self.lines.push(line);
    }

    /// Something a person should read even when the run succeeds (stderr).
    pub fn note(&mut self, line: impl Into<String>) {
        self.loud(format!("NOTE — {}", line.into()));
    }

    /// Something that will hurt later unless a person acts (stderr).
    pub fn warn(&mut self, line: impl Into<String>) {
        self.loud(format!("WARNING — {}", line.into()));
    }

    fn loud(&mut self, line: String) {
        if self.echo {
            eprintln!("{}: {line}", self.verb);
        }
        self.lines.push(line);
    }

    /// Every line so far.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The transcript as one string, a line each.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether any line contains `needle`.
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        self.lines.iter().any(|l| l.contains(needle))
    }
}

/// A scratch directory under the system temporary directory, removed when dropped
/// however the verb ends.
///
/// It is created new, mode 0700, under a random name, and never adopts an existing
/// directory: on a shared `/tmp` a local user who pre-created it could plant the bytes a
/// compare-and-swap compares against, or swap the head's `index.toml` between its
/// verification and its re-read.
#[derive(Debug)]
pub struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    /// `<tmp>/atpkg-publish-<label>-<pid>_<random>_<n>`, fresh.
    ///
    /// # Errors
    ///
    /// A could-not-run when the directory cannot be created.
    pub fn new(label: &str) -> Result<Self> {
        aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-publish-{label}-"))
            .tempdir()
            .map(Self)
            .map_err(|e| Error::io("create a private scratch directory", &e))
    }

    /// Its path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

/// How many times an authenticated download is tried: the release-asset endpoint answers
/// an intermittent HTTP 500 after other assets of the same call have landed. Only
/// [`HostError::Unreachable`] is retried; [`HostError::NotFound`] is an answer.
pub const DOWNLOAD_ATTEMPTS: usize = 3;

/// [`ReleaseHost::download`], tried [`DOWNLOAD_ATTEMPTS`] times, [`ReleaseHost::retry_pause`]
/// apart, while the host does not answer. Every attempt replaces what an earlier one left
/// in `dir`.
///
/// # Errors
///
/// The first answer that is not unreachable, or the last unreachable one.
pub fn download_retried(
    host: &dyn ReleaseHost,
    slug: &Slug,
    tag: &str,
    assets: &[&str],
    dir: &Path,
) -> std::result::Result<(), HostError> {
    let mut last = HostError::NotFound;
    for attempt in 0..DOWNLOAD_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(host.retry_pause());
        }
        match host.download(slug, tag, assets, dir) {
            Ok(()) => return Ok(()),
            Err(e @ HostError::Unreachable(_)) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// A freshness horizon a client can read and has not passed: exactly
/// `YYYY-MM-DDTHH:MM:SSZ` (the client treats anything else as lapsed) and after `now`.
/// Returns its epoch seconds.
///
/// # Errors
///
/// A refusal naming `what`, the stamp, and `remedy` (how to give a better one).
pub fn horizon(what: &str, stamp: &str, now_unix: i64, remedy: &str) -> Result<i64> {
    let at = aterm_types::rfc3339::parse_utc(stamp).ok_or_else(|| {
        Error::refused(format!(
            "{what} valid_until `{stamp}` is not UTC RFC3339 (YYYY-MM-DDTHH:MM:SSZ) — the \
             client treats a stamp it cannot read as LAPSED, so this is dead on arrival; \
             {remedy}"
        ))
    })?;
    if at <= now_unix {
        return Err(Error::refused(format!(
            "{what} valid_until {stamp} has already LAPSED (now {}) — every client refuses \
             it (Reject::Stale); {remedy}",
            aterm_types::rfc3339::format_rfc3339(u64::try_from(now_unix).unwrap_or(0))
        )));
    }
    Ok(at)
}

/// A host failure while doing `what`, as this crate's error: an answer the caller did not
/// expect is a refusal, no answer is a could-not-run.
#[must_use]
pub fn host_error(what: impl std::fmt::Display, e: &HostError) -> Error {
    match e {
        HostError::Unreachable(_) => Error::cannot_run(format!("{what}: {e}")),
        HostError::NotFound | HostError::Refused(_) => Error::refused(format!("{what}: {e}")),
    }
}

/// The value after `flag` in `args` at `i`, advancing `i` past it.
///
/// # Errors
///
/// A usage error when the flag is the last argument.
pub(crate) fn flag_value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| Error::usage(format!("{flag} needs a value")))
}
