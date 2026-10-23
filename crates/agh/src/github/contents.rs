// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A repository's files (design §10.1): one file's bytes, and one directory's entries.

use aterm_json::Value;

use crate::client::{Accept, Call, Github};
use crate::error::{Error, Kind, Result, Sub};
use crate::github::raw::Media;
use crate::github::{How, repo_root};
use crate::model::{ContentEntry, from_json};
use crate::page::{Consistency, End, Listing};
use crate::path;
use crate::policy::RouteTarget;
use crate::slug::Slug;
use crate::wire::WireKind;

/// The most entries GitHub lists in one directory.
const DIRECTORY_CAP: usize = 1000;

/// Why a directory listing at the cap is not whole.
pub(crate) const DIRECTORY_CAP_WHY: &str =
    "the directory holds 1000 entries or more, and GitHub lists at most 1000";

/// The least wire limit of a file read: the first 64 KiB of a failed answer's body are what
/// the pipeline classifies it from.
const CLASSIFY_FLOOR: u64 = 64 * 1024;

/// The contents of one repository.
pub struct Contents<'a> {
    gh: &'a Github,
    slug: Slug,
    how: How,
}

impl<'a> Contents<'a> {
    /// `How::read()` over `slug`.
    pub(crate) fn new(gh: &'a Github, slug: &Slug) -> Contents<'a> {
        Contents {
            gh,
            slug: slug.clone(),
            how: How::read(),
        }
    }

    /// The same contents under `how`.
    #[must_use]
    pub fn how(self, how: How) -> Contents<'a> {
        Contents { how, ..self }
    }

    /// `GET /repos/{o}/{r}/contents/{path}` (no trailing `/` for the root), `?ref=` when given.
    fn call(&self, reference: Option<&str>, path: &str) -> Result<Call> {
        path::refuse_dot_segment("path", path).map_err(|e| e.with_repo(self.slug.to_string()))?;
        let encoded = path::file_path(path);
        let at = if encoded.is_empty() {
            format!("{}/contents", repo_root(&self.slug))
        } else {
            format!("{}/contents/{encoded}", repo_root(&self.slug))
        };
        let call = self.how.get(at, RouteTarget::Repo(self.slug.clone()));
        Ok(match reference {
            Some(reference) => call.query("ref", reference),
            None => call,
        })
    }

    /// An Invalid error about this repository.
    fn invalid(&self, why: String) -> Error {
        Error::new(Kind::Invalid, why).with_repo(self.slug.to_string())
    }

    /// One file's bytes, at most `max` (Accept raw).
    /// # Errors
    /// Usage for a path with a `.` or `..` part, before any request.
    pub fn raw(&self, reference: Option<&str>, path: &str, max: u64) -> Result<Vec<u8>> {
        let call = self
            .call(reference, path)?
            .accept(Accept::Raw)
            .limit(max.max(CLASSIFY_FLOOR));
        let too_large = || self.invalid(format!("{path} is larger than {max} bytes (--max-bytes)"));
        let answered = self.gh.call(&call).map_err(|error| {
            if error.sub == Sub::Wire(WireKind::TooLarge) {
                too_large()
            } else {
                error
            }
        })?;
        if Media::of(answered.header("content-type")) == Media::Json {
            let value = answered.json().ok();
            return Err(match value.as_ref().and_then(entry_type) {
                Some(kind) => {
                    self.invalid(format!("{path} is a {kind}, which agh cat does not read"))
                }
                None => self.invalid(format!("{path} is a directory: use agh ls")),
            });
        }
        if u64::try_from(answered.body.len()).unwrap_or(u64::MAX) > max {
            return Err(too_large());
        }
        Ok(answered.body)
    }

    /// One directory's entries, sorted by name.
    /// # Errors
    /// Usage for a path with a `.` or `..` part, before any request.
    pub fn list(
        &self,
        reference: Option<&str>,
        path: &str,
        partial: bool,
    ) -> Result<Listing<ContentEntry>> {
        let answered = self.gh.call(&self.call(reference, path)?)?;
        let value = answered
            .json()
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        let Some(array) = value.as_array() else {
            return Err(match entry_type(&value) {
                None => self.invalid(format!("{path} is a file: use agh cat")),
                Some(kind @ ("file" | "symlink")) => {
                    self.invalid(format!("{path} is a {kind}: use agh cat"))
                }
                Some(kind) => {
                    self.invalid(format!("{path} is a {kind}, which agh ls does not list"))
                }
            });
        };
        let mut items = array
            .iter()
            .map(from_json::<ContentEntry>)
            .collect::<Result<Vec<_>>>()
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let end = if items.len() >= DIRECTORY_CAP {
            if !partial {
                return Err(Error::new(Kind::Incomplete, DIRECTORY_CAP_WHY)
                    .with_sub(Sub::DirectoryCap)
                    .with_repo(self.slug.to_string()));
            }
            End::Capped { pages: 1 }
        } else {
            End::Complete
        };
        Ok(Listing {
            items,
            pages: 1,
            requests: answered.requests,
            end,
            consistency: Consistency::Snapshot,
            as_: answered.as_,
        })
    }
}

/// A contents object's `type`, when it is one GitHub documents for a single entry (`file`,
/// `symlink` or `submodule`); `None` for an array, a directory or anything else, which the
/// callers read as before.
fn entry_type(value: &Value) -> Option<&'static str> {
    match value.get("type").and_then(Value::as_str)? {
        "file" => Some("file"),
        "symlink" => Some("symlink"),
        "submodule" => Some("submodule"),
        _ => None,
    }
}
