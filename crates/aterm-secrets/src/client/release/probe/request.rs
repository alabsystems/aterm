// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit transaction selection. Neither credentials nor executable paths are arguments.

use super::model;
use crate::{Error, Result};
use std::path::PathBuf;

/// An explicit release capability transaction operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Reserve a new owned probe or recover its exact original request.
    Run,
    /// Read historical evidence without asserting current authority.
    Status,
    /// Continue the same transaction, observing any uncertain effect first.
    Resume,
    /// Observe and remove only objects already owned by this transaction.
    Cleanup,
}

/// The complete public input to a probe. Repositories are restricted further by live
/// owner-aware authorization; naming one here never grants access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Which explicit operation was requested.
    pub mode: Mode,
    /// Exactly 128 bits, encoded as lowercase hexadecimal.
    pub nonce: String,
    /// An absolute private directory named `aterm-release-probe-<nonce>.noindex`.
    pub evidence_dir: PathBuf,
    /// Owner names for `owner/aterm`, present only on the original run.
    pub owners: Vec<String>,
}

impl Request {
    /// Parse the operands following `aterm release probe`. Output is always JSON.
    ///
    /// # Errors
    /// Missing, duplicate, unknown, unsafe or contradictory operands are refused.
    pub fn parse(args: &[String]) -> Result<Self> {
        let mode = match args.first().map(String::as_str) {
            Some("run") => Mode::Run,
            Some("status") => Mode::Status,
            Some("resume") => Mode::Resume,
            Some("cleanup") => Mode::Cleanup,
            _ => {
                return Err(Error::new(
                    "expected release probe run, status, resume or cleanup",
                ));
            }
        };
        let mut nonce = None;
        let mut dir = None;
        let mut owners = Vec::new();
        let mut json = false;
        let mut at = 1;
        while at < args.len() {
            let flag = args[at].as_str();
            if flag == "--json" && !json {
                json = true;
                at += 1;
                continue;
            }
            let value = args
                .get(at + 1)
                .ok_or_else(|| Error::new("release probe option needs a value"))?;
            match flag {
                "--nonce" if nonce.is_none() && model::hex(value, 32) => {
                    nonce = Some(value.clone())
                }
                "--evidence-dir" if dir.is_none() => dir = Some(PathBuf::from(value)),
                "--repo" if mode == Mode::Run => {
                    let owner = value
                        .strip_suffix("/aterm")
                        .filter(|owner| model::owner(owner))
                        .ok_or_else(|| Error::new("release probe requires owner/aterm"))?;
                    if owners.len() == 2
                        || owners
                            .iter()
                            .any(|old: &String| old.eq_ignore_ascii_case(owner))
                    {
                        return Err(Error::new(
                            "duplicate or excessive release probe repositories",
                        ));
                    }
                    owners.push(owner.to_owned());
                }
                _ => return Err(Error::new("unknown or invalid release probe option")),
            }
            at += 2;
        }
        let request = Self {
            mode,
            nonce: nonce.ok_or_else(|| Error::new("release probe needs --nonce"))?,
            evidence_dir: dir.ok_or_else(|| Error::new("release probe needs --evidence-dir"))?,
            owners,
        };
        if !request.evidence_dir.is_absolute()
            || request.evidence_dir.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || request.evidence_dir.file_name().and_then(|s| s.to_str())
                != Some(format!("aterm-release-probe-{}.noindex", request.nonce).as_str())
            || (mode == Mode::Run && request.owners.is_empty())
        {
            return Err(Error::new(
                "release probe needs its exact absolute owned evidence directory and repositories",
            ));
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Vec<String> {
        [
            "run",
            "--nonce",
            "0123456789abcdef0123456789abcdef",
            "--evidence-dir",
            "/private/tmp/.ubd/aterm-release-probe-0123456789abcdef0123456789abcdef.noindex",
            "--repo",
            "fixture-owner/aterm",
            "--json",
        ]
        .map(str::to_owned)
        .into()
    }

    #[test]
    fn only_explicit_owned_transactions_parse() {
        assert!(Request::parse(&args()).is_ok());
        for (at, value) in [
            (2, "--token"),
            (4, "/tmp/unrelated"),
            (6, "fixture-owner/other"),
            (6, "https://github.com/fixture-owner/aterm"),
        ] {
            let mut args = args();
            args[at] = value.into();
            assert!(Request::parse(&args).is_err());
        }
        let mut args = args();
        args.extend(["--binary".into(), "/bin/echo".into()]);
        assert!(Request::parse(&args).is_err());
    }

    #[test]
    fn resume_cannot_change_repository_set() {
        let mut args = args();
        args[0] = "resume".into();
        assert!(Request::parse(&args).is_err());
        args.drain(5..7);
        assert!(Request::parse(&args).is_ok());
    }
}
