// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Anonymous download host probe and fetch.

use crate::client::{Accept, Call, Github};
use crate::download;
use crate::error::{Error, Kind, Result, Sub};
use crate::path;
use crate::policy::{RouteRequest, RouteTarget};
use crate::retry::RetryPolicy;
use crate::slug::Slug;
use crate::wire::{Deadlines, OriginRole, WireKind};

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
        path::segment(slug.owner()),
        path::segment(slug.repo()),
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

    /// A HEAD of the asset's web path, anonymous, any status answered, no retry.
    #[must_use]
    pub fn probe(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        let path = match web_path(slug, tag, asset) {
            Ok(path) => path,
            Err(error) => return Probe::Unknown(error.why.clone()),
        };
        let call = Call::head(path, anonymous(slug))
            .origin(OriginRole::Web)
            .accept(Accept::Media("*/*".to_string()))
            .answer_any(true)
            .retry(Some(RetryPolicy::NONE));
        match self.gh.call(&call) {
            Ok(answered) => match answered.status {
                200 | 302 | 307 => Probe::Present,
                404 => Probe::Absent,
                status => Probe::Unknown(format!("the download host answered {status}")),
            },
            Err(error) => Probe::Unknown(error.why.clone()),
        }
    }

    /// A GET of the asset's web path into `dest`, at most `cap` bytes.
    ///
    /// # Errors
    ///
    /// Usage, before any request, for a tag `path::refuse_bad_tag` refuses (empty, an empty
    /// `/`-part, or a `.` or `..` part) or an asset `refuse_bad_asset` refuses (empty, an empty
    /// or control-byte part, or a `.` or `..` `/`-part in `web_path`); Invalid over `cap` (the
    /// part file deleted); NotFound for a 404; the call's or the write's error.
    pub fn fetch(
        &self,
        slug: &Slug,
        tag: &str,
        asset: &str,
        dest: &std::path::Path,
        cap: u64,
    ) -> Result<u64> {
        let call = Call::get(web_path(slug, tag, asset)?, anonymous(slug))
            .origin(OriginRole::Web)
            .accept(Accept::Media("application/octet-stream".to_string()))
            .hop(true)
            .limit(cap)
            .deadlines(Deadlines::for_transfer(cap));
        let streamed = self.gh.call_stream(&call).map_err(|error| {
            error.why_if_not_found(|| format!("the download host has no {asset} for {tag}"))
        })?;
        let mut body = streamed.body;
        let written = download::stream_to_file(&mut body, dest, &mut |_| {}).map_err(|error| {
            if error.sub == Sub::Wire(WireKind::TooLarge) {
                Error::new(Kind::Invalid, format!("{asset} is over the {cap}-byte cap"))
            } else {
                error
            }
        });
        written.map_err(|e| e.with_repo(slug.to_string()))
    }
}
