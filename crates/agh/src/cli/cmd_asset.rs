// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh asset delete` (design §9.4): one release asset removed, its name typed again.

use super::args::Invocation;
use super::cmd_release_write::{Which, Write, as_outcome, confirm};
use super::dry_run;
use super::write_out::done;
use super::{Ctx, Outcome};
use crate::error::Result;
use crate::write::WriteOp;

/// `agh asset delete REPO TAG NAME --confirm NAME` (plan §10, body 7): the release read as
/// the writer, then `Releases::delete_asset`. The report is op `asset.delete`, key the name,
/// and the asset's id and `browser_download_url` as the guard read saw them.
///
/// # Errors
///
/// Usage for the positional rules; Refused for a `--confirm` absent or not the NAME, before
/// anything is read; `Ctx::github`'s refusals (agh.toml, vault adoption), before the
/// repository is routed, with no report; every error after the repository is routed prints
/// its report.
pub(crate) fn asset_delete(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let rest = inv.rest();
    let tag = rest
        .first()
        .map(String::as_str)
        .ok_or_else(|| inv.usage("TAG is required"))?;
    let name = rest
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| inv.usage("NAME is required"))?;
    crate::path::refuse_bad_tag(tag)?;
    confirm(inv, name)?;
    let w = Write::begin(ctx, inv)?;
    let report = w.report(WriteOp::AssetDelete, name);
    w.finish(report, |report| {
        let permit = w.permit()?;
        let release = w.release(&Which::Tag(tag))?;
        if let Some(asset) = release.assets.iter().find(|a| a.name == name) {
            report.id = Some(asset.id);
            report.url = Some(asset.browser_download_url.clone());
        }
        if w.dry {
            return dry_run::asset_delete(&w, &release, name, report);
        }
        let deleted = w.releases().delete_asset(&permit, &release, name)?;
        Ok(done(as_outcome(report.clone(), deleted)))
    })
}
