// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A repository's history (design §10.1): commits, comparisons, branches, tags and trees.

use aterm_json::Value;

use crate::client::Github;
use crate::error::{Error, Kind, Result};
use crate::github::{How, limit_after_sort, repo_root};
use crate::model::{
    BranchObject, CommitObject, CompareObject, FromJson, TagObject, TreeObject, from_json,
};
use crate::page::{LinkScope, ListOpts, Listing};
use crate::path;
use crate::policy::RouteTarget;
use crate::slug::Slug;

/// The most items GitHub sends on one page.
const MAX_PER_PAGE: usize = 100;

/// The git objects of one repository.
pub struct Git<'a> {
    gh: &'a Github,
    slug: Slug,
    how: How,
}

/// A comparison, or the answer that the refs share no history.
#[derive(Clone, Debug, PartialEq)]
pub enum CompareResult {
    /// The comparison.
    Compared(CompareObject),
    /// The refs share no history.
    NoMergeBase {
        /// The base ref.
        base: String,
        /// The head ref.
        head: String,
    },
}

/// Whether `error` is a probe-confirmed absence whose GitHub message starts with `prefix`.
fn not_found_saying(error: &Error, prefix: &str) -> bool {
    error.kind == Kind::NotFound
        && error
            .github
            .as_ref()
            .and_then(|github| github.message.as_deref())
            .is_some_and(|message| message.starts_with(prefix))
}

impl<'a> Git<'a> {
    /// `How::read()` over `slug`.
    pub(crate) fn new(gh: &'a Github, slug: &Slug) -> Git<'a> {
        Git {
            gh,
            slug: slug.clone(),
            how: How::read(),
        }
    }

    /// The same objects under `how`.
    #[must_use]
    pub fn how(self, how: How) -> Git<'a> {
        Git { how, ..self }
    }

    /// `/repos/{o}/{r}{rest}`.
    fn at(&self, rest: &str) -> String {
        format!("{}{rest}", repo_root(&self.slug))
    }

    /// One call's JSON answer.
    fn read(&self, call: &crate::client::Call) -> Result<Value> {
        self.gh
            .call(call)?
            .json()
            .map_err(|e| e.with_repo(self.slug.to_string()))
    }

    /// A walked listing, mapped to `T` and sorted by `name`, then `opts.limit`.
    fn names<T: FromJson>(
        &self,
        rest: &str,
        opts: &ListOpts,
        name: fn(&T) -> &str,
    ) -> Result<Listing<T>> {
        let first = self
            .how
            .get(self.at(rest), RouteTarget::Repo(self.slug.clone()));
        let walked = crate::page::walk(
            self.gh,
            first,
            &opts.with_limit(None),
            &LinkScope::Repo(self.slug.clone()),
        )?;
        let mut listing = walked
            .map(|value| from_json::<T>(&value))
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        listing.items.sort_by(|a, b| name(a).cmp(name(b)));
        limit_after_sort(&mut listing, opts.limit);
        Ok(listing)
    }

    /// The commit `reference` names: `GET commits?sha={reference}&per_page=1`.
    /// # Errors
    /// NotFound when there is none.
    pub fn resolve(&self, reference: &str) -> Result<CommitObject> {
        let call = self
            .how
            .get(self.at("/commits"), RouteTarget::Repo(self.slug.clone()))
            .query("sha", reference)
            .query("per_page", "1");
        let no_commit = || format!("no commit for {reference}");
        let value = self
            .read(&call)
            .map_err(|error| error.why_if_not_found(no_commit))?;
        match value.as_array().and_then(|items| items.first()) {
            Some(first) => from_json(first).map_err(|e| e.with_repo(self.slug.to_string())),
            None => Err(Error::new(Kind::NotFound, no_commit()).with_repo(self.slug.to_string())),
        }
    }

    /// Commits, in GitHub's order, at most `limit`.
    /// # Errors
    /// The walk's error.
    pub fn log(
        &self,
        reference: Option<&str>,
        path: Option<&str>,
        since: Option<&str>,
        author: Option<&str>,
        limit: usize,
    ) -> Result<Listing<CommitObject>> {
        let limit = limit.max(1);
        let mut first = self
            .how
            .get(self.at("/commits"), RouteTarget::Repo(self.slug.clone()));
        for (key, value) in [
            ("sha", reference),
            ("path", path),
            ("since", since),
            ("author", author),
        ] {
            if let Some(value) = value {
                first = first.query(key, value);
            }
        }
        let per_page = limit.min(MAX_PER_PAGE);
        let pages = u32::try_from(limit.div_ceil(per_page)).unwrap_or(u32::MAX);
        let opts = ListOpts::STANDARD
            .with_per_page(u8::try_from(per_page).unwrap_or(u8::MAX))
            .with_limit(Some(limit))
            .with_max_pages(pages.max(ListOpts::STANDARD.max_pages));
        crate::page::walk(self.gh, first, &opts, &LinkScope::Repo(self.slug.clone()))?
            .map(|value| from_json::<CommitObject>(&value))
            .map_err(|e| e.with_repo(self.slug.to_string()))
    }

    /// `base...head`.
    /// # Errors
    /// Usage for a base or head with a `.` or `..` part, before any request; the call's error.
    pub fn compare(&self, base: &str, head: &str) -> Result<CompareResult> {
        path::refuse_dot_segment("base", base)
            .and_then(|()| path::refuse_dot_segment("head", head))
            .map_err(|e| e.with_repo(self.slug.to_string()))?;
        let rest = format!("/compare/{}...{}", path::segment(base), path::segment(head));
        let call = self
            .how
            .get(self.at(&rest), RouteTarget::Repo(self.slug.clone()));
        match self.read(&call) {
            Ok(value) => from_json(&value)
                .map(CompareResult::Compared)
                .map_err(|e| e.with_repo(self.slug.to_string())),
            Err(error) if not_found_saying(&error, "No common ancestor") => {
                Ok(CompareResult::NoMergeBase {
                    base: base.to_string(),
                    head: head.to_string(),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Branches, sorted by name, `opts.limit` after the sort.
    /// # Errors
    /// The walk's error.
    pub fn branches(&self, opts: &ListOpts) -> Result<Listing<BranchObject>> {
        self.names("/branches", opts, |branch: &BranchObject| &branch.name)
    }

    /// Tags, sorted by name, `opts.limit` after the sort.
    /// # Errors
    /// The walk's error.
    pub fn tags(&self, opts: &ListOpts) -> Result<Listing<TagObject>> {
        self.names("/tags", opts, |tag: &TagObject| &tag.name)
    }

    /// The tree of `reference`: `GET git/trees/{ref}`, `?recursive=1` when `recursive`.
    /// # Errors
    /// Usage for a ref with a `.` or `..` part, before any request; the calls' errors.
    pub fn tree(&self, reference: Option<&str>, recursive: bool) -> Result<TreeObject> {
        if let Some(reference) = reference {
            path::refuse_dot_segment("ref", reference)
                .map_err(|e| e.with_repo(self.slug.to_string()))?;
        }
        let reference = match reference {
            Some(reference) => reference.to_string(),
            None => {
                let repo = self.gh.repos().how(self.how.clone()).get(&self.slug)?;
                repo.default_branch.ok_or_else(|| {
                    Error::protocol(format!("GitHub names no default branch of {}", self.slug))
                        .with_repo(self.slug.to_string())
                })?
            }
        };
        let rest = format!("/git/trees/{}", path::ref_path(&reference));
        let mut call = self
            .how
            .get(self.at(&rest), RouteTarget::Repo(self.slug.clone()));
        if recursive {
            call = call.query("recursive", "1");
        }
        let value = self.read(&call)?;
        let mut tree: TreeObject =
            from_json(&value).map_err(|e| e.with_repo(self.slug.to_string()))?;
        if let Some(raws) = value.get("tree").and_then(Value::as_array) {
            for (entry, raw) in tree.tree.iter_mut().zip(raws) {
                entry.raw = raw.clone();
            }
        }
        Ok(tree)
    }
}
