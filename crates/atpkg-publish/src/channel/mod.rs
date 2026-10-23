// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signed index and everything it pins.

pub mod group;
pub mod index;
pub mod mirror;
pub mod seed;
pub mod status;

use std::path::Path;

use crate::error::{Error, Result};
use crate::host::{HostError, Probe, ReleaseHost, Slug};

/// A verb's transcript: every line it says, in order.
#[derive(Debug)]
pub struct Log {
    verb: &'static str,
    echo: bool,
    lines: Vec<String>,
}

impl Log {
    /// A transcript printed as it is written (`<verb>: <line>`).
    #[must_use]
    pub const fn echo(verb: &'static str) -> Self {
        Self {
            verb,
            echo: true,
            lines: Vec::new(),
        }
    }

    /// A transcript kept, never printed.
    #[must_use]
    pub const fn capture(verb: &'static str) -> Self {
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

/// Private scratch directory under the system temp dir, removed on drop.
#[derive(Debug)]
pub struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    /// Creates a fresh `<tmp>/atpkg-publish-<label>-<pid>_<random>_<n>` directory.
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

    /// Returns the scratch directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

/// Maximum attempts for an authenticated download on [`HostError::Unreachable`].
pub const DOWNLOAD_ATTEMPTS: usize = 3;

/// [`ReleaseHost::download`] retried up to [`DOWNLOAD_ATTEMPTS`] times on unreachable errors.
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

/// Maximum public-availability probes for a newly uploaded release asset.
pub const SERVE_PROBES: usize = 13;

/// Probes the download host up to [`SERVE_PROBES`] times until `asset` is [`Probe::Present`].
#[must_use]
pub fn probe_served(host: &dyn ReleaseHost, slug: &Slug, tag: &str, asset: &str) -> Probe {
    let mut answer = Probe::Absent;
    for attempt in 0..SERVE_PROBES {
        if attempt > 0 {
            std::thread::sleep(host.retry_pause());
        }
        answer = host.probe_public(slug, tag, asset);
        if answer == Probe::Present {
            break;
        }
    }
    answer
}

/// Parses a future `YYYY-MM-DDTHH:MM:SSZ` freshness horizon after `now_unix` as epoch seconds.
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

/// Maps a [`HostError`] while doing `what` to this crate's [`Error`].
#[must_use]
pub fn host_error(what: impl std::fmt::Display, e: &HostError) -> Error {
    match e {
        HostError::Unreachable(_) => Error::cannot_run(format!("{what}: {e}")),
        HostError::NotFound | HostError::Refused(_) => Error::refused(format!("{what}: {e}")),
    }
}

/// The value after `flag` in `args` at `i`, advancing `i` past it.
/// # Errors
///
/// A usage error when the flag is the last argument.
pub(crate) fn flag_value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| Error::usage(format!("{flag} needs a value")))
}
