// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Typed GitHub resource readers over the client.

pub mod contents;
pub mod git;
pub mod public;
pub mod raw;
pub mod refs;
pub mod releases;
pub mod repos;
pub mod users;

pub use contents::Contents;
pub use git::{CompareResult, Git};
pub use public::{Probe, Public};
pub use raw::{Raw, RawAnswer, RawGet, RawOutcome, RawSend};
pub use refs::Refs;
pub use releases::{Releases, TagObjects};
pub use repos::{RepoListKind, Repos};
pub use users::Users;

use crate::client::{Call, Github};
use crate::policy::{Access, Named, RouteRequest, RouteTarget};
use crate::retry::RetryPolicy;
use crate::slug::Slug;

/// The checks every write method makes first (plan §9 body 1): unix only, and a permit for
/// `slug` (ASCII case-insensitive, as `Slug`'s `==` compares), else Usage with nothing sent,
/// naming `what` (`refs`, `releases`) as `slug`'s.
pub(crate) fn permit_covers(
    permit: &crate::policy::Permit,
    slug: &Slug,
    what: &str,
) -> crate::error::Result<()> {
    use crate::policy::PermitTarget;
    if cfg!(not(unix)) {
        return Err(crate::write::unix_only());
    }
    let target = match permit.target() {
        PermitTarget::Repo(held) if held == slug => return Ok(()),
        PermitTarget::Repo(held) => held.to_string(),
        PermitTarget::Identity => "/user".to_string(),
    };
    Err(crate::error::Error::usage(format!(
        "the permit is for {target}, and these {what} are {slug}'s"
    ))
    .with_repo(slug.to_string()))
}

/// `limit` applied to a listing already in its final order ([`crate::page::cut`]).
pub(crate) fn limit_after_sort<T>(listing: &mut crate::page::Listing<T>, limit: Option<usize>) {
    listing.end = crate::page::cut(listing.end, &mut listing.items, limit);
}

/// `/repos/{o}/{r}`, each half one encoded segment.
pub(crate) fn repo_root(slug: &Slug) -> String {
    format!(
        "/repos/{}/{}",
        crate::path::segment(slug.owner()),
        crate::path::segment(slug.repo())
    )
}

/// How a resource call is routed. Built from `How::read()`, `How::writer()` or `How::write()`
/// (`#[non_exhaustive]`); `Default` is a hand `impl`, because `rate_book` defaults to `true`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct How {
    /// What the calls may see and do.
    pub access: Access,
    /// `--anonymous`.
    pub anonymous: bool,
    /// `--as`.
    pub as_profile: Option<String>,
    /// How the destination was named.
    pub named: Named,
    /// `--no-probe`: a 404 is not probed.
    pub no_probe: bool,
    /// `None`: the client's policy.
    pub retry: Option<RetryPolicy>,
    /// Consult the rate book before sending (default true). The reads that guard a write set it
    /// false (design §7.4, §11.3).
    pub rate_book: bool,
    /// The most time this call's retry sleeps may take (default `None`: no cap).
    pub sleep_budget: Option<std::time::Duration>,
    /// Post the settled event to the fabric (design §21.5; default false).
    pub notify: bool,
}

impl Default for How {
    /// Read, routed, `Named::Default`, the probe on, the client's retry policy, the rate book
    /// consulted, no sleep budget, no notify.
    fn default() -> How {
        How {
            access: Access::Read,
            anonymous: false,
            as_profile: None,
            named: Named::Default,
            no_probe: false,
            retry: None,
            rate_book: true,
            sleep_budget: None,
            notify: false,
        }
    }
}

impl How {
    /// A read.
    #[must_use]
    pub fn read() -> How {
        How::default()
    }

    /// WriterRead: drafts and compare-and-swap reads, as exactly the writer.
    #[must_use]
    pub fn writer() -> How {
        How {
            access: Access::WriterRead,
            ..How::default()
        }
    }

    /// A write: `Access::Write`, the other fields defaulted.
    #[must_use]
    pub fn write() -> How {
        How {
            access: Access::Write,
            ..How::default()
        }
    }

    /// The same routing with the rate book consulted or not.
    #[must_use]
    pub fn rate_book(mut self, on: bool) -> How {
        self.rate_book = on;
        self
    }

    /// The same routing with a cap on retry sleeps.
    #[must_use]
    pub fn sleep_budget(mut self, budget: std::time::Duration) -> How {
        self.sleep_budget = Some(budget);
        self
    }

    /// The same routing, posting its settled event (design §21.5).
    #[must_use]
    pub fn notify(mut self, on: bool) -> How {
        self.notify = on;
        self
    }

    /// The same routing with `anonymous`.
    #[must_use]
    pub const fn anonymous(mut self, anonymous: bool) -> How {
        self.anonymous = anonymous;
        self
    }

    /// The same routing with `as_profile`.
    #[must_use]
    pub fn as_profile(mut self, profile: Option<String>) -> How {
        self.as_profile = profile;
        self
    }

    /// The same routing with `named`.
    #[must_use]
    pub fn named(mut self, named: Named) -> How {
        self.named = named;
        self
    }

    /// The same routing with the probe off or on.
    #[must_use]
    pub const fn no_probe(mut self, no_probe: bool) -> How {
        self.no_probe = no_probe;
        self
    }

    /// The same routing with its own retry policy.
    #[must_use]
    pub const fn retry_policy(mut self, policy: RetryPolicy) -> How {
        self.retry = Some(policy);
        self
    }

    /// This routing for `target`.
    #[must_use]
    pub fn route(&self, target: RouteTarget) -> RouteRequest {
        let mut route = RouteRequest::read(target)
            .with_anonymous(self.anonymous)
            .with_as(self.as_profile.clone())
            .with_named(self.named.clone());
        route.access = self.access;
        route
    }

    /// `Call::get(path, self.route(target))` with this routing's probe, retry policy, rate
    /// book and sleep budget.
    pub(crate) fn get(&self, path: impl Into<String>, target: RouteTarget) -> Call {
        Call::get(path, self.route(target))
            .probe_404(!self.no_probe)
            .retry(self.retry)
            .rate_book(self.rate_book)
            .sleep_budget(self.sleep_budget)
    }

    /// The same routing with `access`.
    pub(crate) fn with_access(&self, access: Access) -> How {
        How {
            access,
            ..self.clone()
        }
    }

    /// This routing as a Read, for a sub-read a resource makes whatever its own access.
    pub(crate) fn as_read(&self) -> How {
        let mut read = self.with_access(Access::Read);
        if self.access != Access::Read {
            read.as_profile = None;
        }
        read
    }
}

impl Github {
    /// Repositories.
    #[must_use]
    pub fn repos(&self) -> Repos<'_> {
        Repos::new(self)
    }

    /// The releases of `slug`.
    #[must_use]
    pub fn releases(&self, slug: &Slug) -> Releases<'_> {
        Releases::new(self, slug)
    }

    /// The contents of `slug`.
    #[must_use]
    pub fn contents(&self, slug: &Slug) -> Contents<'_> {
        Contents::new(self, slug)
    }

    /// The git objects of `slug`.
    #[must_use]
    pub fn git(&self, slug: &Slug) -> Git<'_> {
        Git::new(self, slug)
    }

    /// Users and the rate limit.
    #[must_use]
    pub const fn users(&self) -> Users<'_> {
        Users::new(self)
    }

    /// The anonymous download host.
    #[must_use]
    pub const fn public(&self) -> Public<'_> {
        Public::new(self)
    }

    /// `agh api`.
    #[must_use]
    pub const fn raw(&self) -> Raw<'_> {
        Raw::new(self)
    }
}

/// `value` as compact JSON with sorted keys: a write's body, which agh encodes from its own
/// `json!` values.
///
/// # Errors
///
/// could_not_run `Sub::Local`, why `a write body did not encode: {e}`.
pub(crate) fn encode_body(value: &aterm_json::Value) -> crate::error::Result<Vec<u8>> {
    aterm_json::to_vec(value)
        .map_err(|e| crate::error::Error::local(format!("a write body did not encode: {e}")))
}
