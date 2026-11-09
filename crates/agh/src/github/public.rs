// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Anonymous download host probe and fetch.
//!
//! A probe asks only the download host, since its question is what that host serves. A fetch
//! reads the asset's web path first. When the download host fails in transit (a 5xx, a 408 or
//! a 429, a timeout, a reset, a broken body or a failed connection), the fetch reads the same
//! bytes on the REST asset route, still anonymous: the tag's release for the asset's id (once
//! per fetch), then `releases/assets/{id}` as `application/octet-stream`, whose redirect to the
//! asset CDN the client follows without credentials. A 404 on the web route is the asset's
//! absence and asks nothing more. The fetch alternates the two routes, pausing longer each
//! round, each call resuming from the bytes already held, until one delivers or no route has
//! delivered a byte for [`PATIENCE`]: it stops on lack of progress, never on a count. A round
//! in which neither route reached a host ends the fetch at once.
//!
//! Measured 2026-11-08 and 2026-11-09: during the 0.175.0 and 0.185.0 cuts, the web route
//! answered 504 to every try for minutes at a time, while the API asset route served the same
//! asset anonymously.

use std::time::Duration;

use crate::client::{Accept, Call, Github};
use crate::download::ResumableFile;
use crate::error::{Error, Kind, Result, Sub};
use crate::github::{How, Releases, repo_root};
use crate::model::{AssetObject, AssetState};
use crate::path;
use crate::policy::{RouteRequest, RouteTarget};
use crate::retry::{RetryPolicy, transient_cause};
use crate::slug::Slug;
use crate::wire::{Deadlines, OriginRole, WireKind};

/// How long a fetch goes on while no route delivers a byte: the release cutter's patience, as
/// every caller of a public fetch is release tooling (the publication engine's signing step and
/// atpkg-publish's lanes).
pub const PATIENCE: Duration = Duration::from_secs(300);

/// The pause after the first round in which both routes failed; each later pause doubles it,
/// up to [`PAUSE_CAP`].
const FIRST_PAUSE: Duration = Duration::from_secs(2);

/// The longest pause between two rounds.
const PAUSE_CAP: Duration = Duration::from_secs(30);

/// The anonymous download host.
pub struct Public<'a> {
    gh: &'a Github,
}

/// What the anonymous download host answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The asset is there.
    Present,
    /// The asset is not there.
    Absent,
    /// The host's answer said neither.
    Unknown(String),
}

/// Rejects an empty, slash-empty, control-byte, or dot-segment `asset`.
pub(crate) fn refuse_bad_asset(asset: &str) -> Result<()> {
    path::refuse_dot_segment("asset", asset)?;
    if asset.split('/').any(str::is_empty) || asset.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(Error::usage(format!(
            "asset \"{asset}\" is empty or holds an empty or control-byte part: no release can have it"
        )));
    }
    Ok(())
}

/// `/{o}/{r}/releases/download/{tag}/{asset}`, each part one encoded segment.
fn web_path(slug: &Slug, tag: &str, asset: &str) -> Result<String> {
    path::refuse_bad_tag(tag)
        .and_then(|()| refuse_bad_asset(asset))
        .map_err(|e| e.with_repo(slug.to_string()))?;
    Ok(format!(
        "/{}/{}/releases/download/{}/{}",
        slug.owner(),
        slug.repo(),
        path::segment(tag),
        path::segment(asset)
    ))
}

/// An anonymous read about `slug`.
fn anonymous(slug: &Slug) -> RouteRequest {
    RouteRequest::read(RouteTarget::Repo(slug.clone())).with_anonymous(true)
}

impl<'a> Public<'a> {
    /// The host.
    pub(crate) const fn new(gh: &'a Github) -> Public<'a> {
        Public { gh }
    }

    /// A HEAD of the asset's web path, anonymous, any definitive status answered after the
    /// shared transient-failure retry budget.
    #[must_use]
    pub fn probe(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        let path = match web_path(slug, tag, asset) {
            Ok(path) => path,
            Err(error) => return Probe::Unknown(error.why.clone()),
        };
        let call = Call::head(path, anonymous(slug))
            .origin(OriginRole::Web)
            .accept(Accept::Media("*/*".to_string()))
            .answer_any(true);
        match self.gh.call(&call) {
            Ok(answered) => match answered.status {
                200 | 302 | 307 => Probe::Present,
                404 => Probe::Absent,
                status => Probe::Unknown(format!("the download host answered {status}")),
            },
            Err(error) => Probe::Unknown(error.why.clone()),
        }
    }

    /// A GET of `asset` into `dest`, at most `cap` bytes: the web path, then the REST asset
    /// route while the download host fails in transit (see the module).
    ///
    /// # Errors
    ///
    /// Usage, before any request, for a tag `path::refuse_bad_tag` refuses (empty, an empty
    /// `/`-part, or a `.` or `..` part), an asset `refuse_bad_asset` refuses (empty, an empty or
    /// control-byte part, or a `.` or `..` `/`-part in `web_path`), or a destination
    /// `download::refuse_part_name` or `download::refuse_non_file` refuses; Invalid over `cap`
    /// (the part file deleted); NotFound for a 404 on the web route, or for a release or asset
    /// the API route lacks once the download host failed; a route's error that is not a
    /// failure in transit, or the write's; once no route has delivered a byte for
    /// [`PATIENCE`], or a round reached no host on either route, the download host's failure,
    /// its why naming the API route's too.
    pub fn fetch(
        &self,
        slug: &Slug,
        tag: &str,
        asset: &str,
        dest: &std::path::Path,
        cap: u64,
    ) -> Result<u64> {
        let path = web_path(slug, tag, asset)?;
        let mut file = ResumableFile::create(dest).map_err(|e| e.with_repo(slug.to_string()))?;
        self.fetch_into(slug, tag, asset, &path, &mut file, cap)
            .and_then(|written| file.finish().map(|()| written))
            .map_err(|e| e.with_repo(slug.to_string()))
    }

    /// `fetch`'s rounds, into `file`.
    fn fetch_into(
        &self,
        slug: &Slug,
        tag: &str,
        asset: &str,
        path: &str,
        file: &mut ResumableFile,
        cap: u64,
    ) -> Result<u64> {
        let web = Call::get(path, anonymous(slug))
            .origin(OriginRole::Web)
            .accept(Accept::Media("application/octet-stream".to_string()))
            .hop(true)
            .limit(cap)
            .deadlines(Deadlines::for_transfer(cap))
            .retry(Some(RetryPolicy::NONE));
        let mut found: Option<AssetObject> = None;
        // The API route's failure that another round would only repeat (a refusal, a hidden
        // repository, a malformed answer): the route is left out from then on.
        let mut api_out: Option<Error> = None;
        // When the current quiet stretch began, and how many bytes had arrived by then.
        let mut quiet: Option<(Duration, u64)> = None;
        let mut pause = FIRST_PAUSE;
        let mut round = 1u32;
        loop {
            let web_failure = match file.call(self.gh, &web, cap) {
                Ok(written) => return Ok(written),
                Err(error) if in_transit(&error) => error,
                Err(error) => {
                    return Err(over_cap(error, asset, cap).why_if_not_found(|| {
                        format!("the download host has no {asset} for {tag}")
                    }));
                }
            };
            let api_failure = match &api_out {
                Some(error) => error.clone(),
                None => match self
                    .api_leg(slug, tag, asset, file, &mut found, cap)
                    .map_err(|error| over_cap(error, asset, cap))
                {
                    Ok(written) => return Ok(written),
                    Err(error) if error.kind == Kind::NotFound => {
                        let why = format!(
                            "the download host failed ({}), and on the API asset route {}",
                            web_failure.why, error.why
                        );
                        return Err(error.recast(Kind::NotFound, why));
                    }
                    Err(error) if error.kind == Kind::Invalid => return Err(error),
                    Err(error) if in_transit(&error) => error,
                    Err(error) => {
                        api_out = Some(error.clone());
                        error
                    }
                },
            };
            if reached_no_host(&web_failure) && reached_no_host(&api_failure) {
                return Err(both(
                    web_failure,
                    &api_failure,
                    "neither route reached a host",
                ));
            }
            let now = self.gh.fx().mono()?;
            let arrived = file.arrived();
            let since = match quiet {
                Some((since, mark)) if mark == arrived => since,
                _ => {
                    quiet = Some((now, arrived));
                    now
                }
            };
            let quiet_for = now.saturating_sub(since);
            if quiet_for >= PATIENCE {
                let why = format!("no route delivered a byte for {}s", PATIENCE.as_secs());
                return Err(both(web_failure, &api_failure, &why));
            }
            let wait = pause.min(PATIENCE.saturating_sub(quiet_for));
            self.gh.fx().note_round(&format!(
                "round {round} failed: the download host ({}), the API asset route ({}); next \
                 round in {}s, {}s of {}s with no byte",
                failure_of(&web_failure),
                failure_of(&api_failure),
                wait.as_secs(),
                quiet_for.as_secs(),
                PATIENCE.as_secs()
            ))?;
            self.gh.fx().sleep(wait)?;
            pause = pause.saturating_mul(2).min(PAUSE_CAP);
            round = round.saturating_add(1);
        }
    }

    /// The REST asset route: the tag's release for `asset`'s id (read once per fetch, into
    /// `found`), then its bytes into `file`, anonymous throughout.
    fn api_leg(
        &self,
        slug: &Slug,
        tag: &str,
        asset: &str,
        file: &mut ResumableFile,
        found: &mut Option<AssetObject>,
        cap: u64,
    ) -> Result<u64> {
        let id =
            match found {
                Some(object) => object.id,
                None => {
                    let release = Releases::new(self.gh, slug)
                        .how(How::read().anonymous(true).retry_policy(RetryPolicy::NONE))
                        .published_by_tag(tag)?;
                    let Some(object) = release.assets.into_iter().find(|object| {
                        object.name == asset && object.state() == AssetState::Uploaded
                    }) else {
                        return Err(Error::new(
                            Kind::NotFound,
                            format!("release {tag} has no uploaded {asset}"),
                        ));
                    };
                    if object.size > cap {
                        return Err(Error::new(
                            Kind::Invalid,
                            format!("{asset} is over the {cap}-byte cap"),
                        ));
                    }
                    found.insert(object).id
                }
            };
        let call = Call::get(
            format!("{}/releases/assets/{id}", repo_root(slug)),
            anonymous(slug),
        )
        .accept(Accept::OctetStream)
        .hop(true)
        .limit(cap)
        .deadlines(Deadlines::for_transfer(cap))
        .retry(Some(RetryPolicy::NONE));
        file.call(self.gh, &call, cap)
    }
}

/// Whether `error` is a route's failure in transit, which the other route may not share: a
/// transient cause (a 5xx, a 408, a 429, a timeout, a reset, a broken body) or a connection
/// that failed or never opened.
fn in_transit(error: &Error) -> bool {
    transient_cause(error).is_some()
        || reached_no_host(error)
        || error.sub == Sub::Wire(WireKind::Tls)
}

/// Whether `error` says no host was reached: the name did not resolve, or no connection
/// opened (directly or through the proxy).
fn reached_no_host(error: &Error) -> bool {
    matches!(
        error.sub,
        Sub::Wire(WireKind::Dns | WireKind::Connect | WireKind::Proxy)
    )
}

/// A route's failure in a few words: its transient cause, else its why.
fn failure_of(error: &Error) -> String {
    transient_cause(error).map_or_else(|| error.why.clone(), |cause| cause.to_string())
}

/// `error`, Invalid when the body outran `cap`.
fn over_cap(error: Error, asset: &str, cap: u64) -> Error {
    if error.sub == Sub::Wire(WireKind::TooLarge) {
        Error::new(Kind::Invalid, format!("{asset} is over the {cap}-byte cap"))
    } else {
        error
    }
}

/// The download host's failure, its why naming the API route's and why the fetch stopped.
fn both(web: Error, api: &Error, stopped: &str) -> Error {
    let kind = web.kind;
    let why = format!(
        "the download host: {}; the API asset route: {} ({stopped})",
        web.why, api.why
    );
    web.recast(kind, why)
}
