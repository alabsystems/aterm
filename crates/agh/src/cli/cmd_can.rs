// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh can` (design §10.2): whether the routed identity holds a permission.

use aterm_json::{Value, json};

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt};
use crate::error::{Error, Forbid, Kind, Result, Sub};
use crate::model::Permissions;
use crate::policy::Access;

/// The `permissions` object, every key a bool.
fn permissions_value(p: &Permissions) -> Value {
    json!({
        "admin": p.admin,
        "maintain": p.maintain,
        "pull": p.pull,
        "push": p.push,
        "triage": p.triage,
    })
}

/// `agh can REPO pull|push|admin`: exit 0 with the Permission object when granted, Forbidden
/// (exit 5, nothing on stdout) when not. `pull` is read as a Read; `push` and `admin` are
/// judged as the writer (WriterRead).
///
/// # Errors
///
/// Usage for another OP; Forbidden `permission` when not granted; the read's error.
pub(crate) fn can(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let op = inv.rest().first().map(String::as_str).unwrap_or_default();
    let access = match op {
        "pull" => Access::Read,
        "push" | "admin" => Access::WriterRead,
        _ => return Err(inv.usage("can wants pull, push or admin")),
    };
    let gh = ctx.github()?;
    let (slug, named) = inv.repo(gh.router())?;
    let (_, permissions) = gh
        .repos()
        .how(inv.how(access, named))
        .permissions(&slug, access)?;
    let receipt = Receipt::for_repo(inv, &gh, &slug);
    let meter = gh.meter();
    let label = meter.label();
    let granted = match op {
        "pull" => permissions.pull,
        "push" => permissions.push,
        _ => permissions.admin,
    };
    if !granted {
        return Err(Error::new(
            Kind::Forbidden,
            format!("as {label}, {op} is not granted on {slug}"),
        )
        .with_sub(Sub::Forbid(Forbid::Permission { accepted: None }))
        .with_next(format!("a person grants {op} on {slug}"))
        .with_repo(slug.to_string()));
    }
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Permission,
            value: json!({
                "as": label,
                "granted": true,
                "op": op,
                "permissions": permissions_value(&permissions),
                "repo": slug.to_string(),
            }),
        },
        receipt,
    })
}
