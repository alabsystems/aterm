// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Repository names, `-R` arguments and CONTENT references (design §9.1).

use crate::error::{Error, Result};
use crate::path;

/// A repository, `owner/name`. Compared, ordered and hashed by `key()`, case-insensitively;
/// shown as given.
#[derive(Clone, Debug)]
pub struct Slug {
    owner: String,
    repo: String,
}

/// Whether `owner` is a GitHub owner name: 1-39 of `A-Z a-z 0-9 -`, not starting with `-`
/// (L3's one rule, `config::is_owner_name`).
#[inline]
fn owner_ok(owner: &str) -> bool {
    crate::config::is_owner_name(owner)
}

/// Whether `repo` is a GitHub repository name: 1-100 of `A-Z a-z 0-9 . _ -`, not `.` or `..`.
#[inline]
fn repo_ok(repo: &str) -> bool {
    (1..=100).contains(&repo.len())
        && repo != "."
        && repo != ".."
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Slug {
    /// A slug from its two halves.
    ///
    /// # Errors
    ///
    /// Usage: `not an owner name: "{owner}" (1-39 of A-Z a-z 0-9 -, not starting with -)`, or
    /// `not a repository name: "{repo}" (1-100 of A-Z a-z 0-9 . _ -, not . or ..)`.
    pub fn new(owner: &str, repo: &str) -> Result<Slug> {
        if !owner_ok(owner) {
            return Err(Error::usage(format!(
                "not an owner name: \"{owner}\" (1-39 of A-Z a-z 0-9 -, not starting with -)"
            )));
        }
        if !repo_ok(repo) {
            return Err(Error::usage(format!(
                "not a repository name: \"{repo}\" (1-100 of A-Z a-z 0-9 . _ -, not . or ..)"
            )));
        }
        Ok(Slug {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    /// `owner/name`, with exactly one `/`.
    ///
    /// # Errors
    ///
    /// Usage: `not a repository: "{text}" (want OWNER/NAME)`, or `new`'s errors.
    pub fn parse(text: &str) -> Result<Slug> {
        let mut halves = text.split('/');
        match (halves.next(), halves.next(), halves.next()) {
            (Some(owner), Some(repo), None) => Slug::new(owner, repo),
            _ => Err(Error::usage(format!(
                "not a repository: \"{text}\" (want OWNER/NAME)"
            ))),
        }
    }

    /// The owner, as given.
    #[inline]
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The repository's name, as given.
    #[inline]
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The owner, lowercased.
    #[inline]
    #[must_use]
    pub fn owner_key(&self) -> String {
        self.owner.to_ascii_lowercase()
    }

    /// `owner/repo`, lowercased.
    #[inline]
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", self.owner, self.repo).to_ascii_lowercase()
    }
}

impl Slug {
    /// `key()`'s bytes, without building it: `owner`, `/`, `repo`, each lowercased.
    #[inline]
    fn key_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.owner
            .bytes()
            .chain(*b"/")
            .chain(self.repo.bytes())
            .map(|b| b.to_ascii_lowercase())
    }
}

impl PartialEq for Slug {
    #[inline]
    fn eq(&self, other: &Slug) -> bool {
        // `Slug::new` holds both halves to ASCII, so each half's case-insensitive equality is
        // `key()`'s equality.
        self.owner.eq_ignore_ascii_case(&other.owner) && self.repo.eq_ignore_ascii_case(&other.repo)
    }
}

impl Eq for Slug {}

impl std::hash::Hash for Slug {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for b in self.key_bytes() {
            state.write_u8(b);
        }
    }
}

impl PartialOrd for Slug {
    #[inline]
    fn partial_cmp(&self, other: &Slug) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Slug {
    #[inline]
    fn cmp(&self, other: &Slug) -> std::cmp::Ordering {
        self.key_bytes().cmp(other.key_bytes())
    }
}

impl std::fmt::Display for Slug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// A `-R` value: a literal slug, or `@name` from `[targets]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoArg {
    /// `OWNER/NAME`.
    Literal(Slug),
    /// `@name`: the name, without its `@`.
    Target(String),
}

/// Whether `name` is a target name: `[a-z][a-z0-9-]{0,31}` (L10's one rule,
/// `config::is_row_name`).
#[inline]
pub(crate) fn target_name_ok(name: &str) -> bool {
    crate::config::is_row_name(name)
}

impl RepoArg {
    /// `@name` (`[a-z][a-z0-9-]{0,31}`) is a target. `.` is refused.
    ///
    /// # Errors
    ///
    /// Usage: `not a target name: "{text}"`;
    /// `no cwd inference in M1: name the repository (OWNER/NAME or @TARGET)`; or
    /// `Slug::parse`'s errors.
    pub fn parse(text: &str) -> Result<RepoArg> {
        if text == "." {
            return Err(Error::usage(
                "no cwd inference in M1: name the repository (OWNER/NAME or @TARGET)",
            ));
        }
        if let Some(name) = text.strip_prefix('@') {
            if target_name_ok(name) {
                return Ok(RepoArg::Target(name.to_string()));
            }
            return Err(Error::usage(format!("not a target name: \"{text}\"")));
        }
        Slug::parse(text).map(RepoArg::Literal)
    }
}

/// `REPO[@REF][:PATH]`, or `[@REF][:PATH]` when `-R` gave the repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentRef {
    /// The repository, when the text named one.
    pub repo: Option<RepoArg>,
    /// `@REF`.
    pub reference: Option<String>,
    /// `:PATH`, any leading `/` stripped; `Some("")` is the root.
    pub path: Option<String>,
}

impl ContentRef {
    /// With `with_repo`, a leading `@target` runs to the next `@` or `:`, and an `owner/name`
    /// runs to the first `@` or `:`. Then `@REF` runs to the first `:`, and `:PATH` is the rest
    /// with any leading `/` stripped. An empty PATH (`:`) is `Some("")`, the root. No `:` is
    /// `None`.
    ///
    /// # Errors
    ///
    /// Usage: `an empty @REF: name a branch, tag or sha`; `"{text}" holds a . or .. part: agh
    /// sends no dot-segment`, for a REF or a PATH with one (`path::has_dot_segment`), because
    /// a server that resolved it would move the request off the repository it was routed
    /// for; and `RepoArg::parse`'s errors.
    pub fn parse(text: &str, with_repo: bool) -> Result<ContentRef> {
        let mut rest = text;
        let mut repo = None;
        if with_repo {
            let skip = usize::from(rest.starts_with('@'));
            let end = rest
                .get(skip..)
                .and_then(|tail| tail.find(['@', ':']))
                .map_or(rest.len(), |at| at + skip);
            let (head, tail) = rest.split_at(end);
            repo = Some(RepoArg::parse(head)?);
            rest = tail;
        }
        let mut reference = None;
        if let Some(after) = rest.strip_prefix('@') {
            let end = after.find(':').unwrap_or(after.len());
            let (name, tail) = after.split_at(end);
            if name.is_empty() {
                return Err(Error::usage("an empty @REF: name a branch, tag or sha"));
            }
            reference = Some(name.to_string());
            rest = tail;
        }
        let path = match rest.strip_prefix(':') {
            Some(path) => Some(path.trim_start_matches('/').to_string()),
            None if rest.is_empty() => None,
            None => {
                return Err(Error::usage(format!(
                    "not a content reference: \"{text}\" (want [@REF][:PATH])"
                )));
            }
        };
        let dotted = |part: &Option<String>| part.as_deref().is_some_and(path::has_dot_segment);
        if dotted(&reference) || dotted(&path) {
            return Err(path::dot_segment_error("content reference", text));
        }
        Ok(ContentRef {
            repo,
            reference,
            path,
        })
    }
}
