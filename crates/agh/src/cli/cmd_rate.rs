// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh rate` (design §13.2): GitHub's `/rate_limit` (free) for each identity, beside what the
//! rate book recorded.

use aterm_json::{Value, json};

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::error::{IdentityRef, Result};
use crate::policy::Presence;
use crate::rate::{self, RateSnapshot};

/// One row: `{"as","source","resource","limit","remaining","reset","used"}`.
fn row(label: &str, source: &str, snapshot: &RateSnapshot) -> Value {
    json!({
        "as": label,
        "source": source,
        "resource": snapshot.resource.as_str(),
        "limit": snapshot.limit,
        "remaining": snapshot.remaining,
        "reset": snapshot.reset,
        "used": snapshot.used,
    })
}

/// `agh rate [--as PROFILE | --anonymous]`: with `--as`, that profile; with `--anonymous`,
/// anonymous; else every present profile, by name, then anonymous.
///
/// # Errors
///
/// The first identity's error.
pub(crate) fn rate(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let gh = ctx.github()?;
    let who: Vec<Option<String>> = if let Some(profile) = &inv.global.as_profile {
        vec![Some(profile.clone())]
    } else if inv.global.anonymous {
        vec![None]
    } else {
        let mut names: Vec<String> = gh
            .router()
            .profiles()
            .into_iter()
            .map(|p| p.name.clone())
            .filter(|name| gh.presence(name) == Presence::Present)
            .collect();
        names.sort();
        names.into_iter().map(Some).chain([None]).collect()
    };
    let mut rows = Vec::new();
    for profile in who {
        let snapshots = gh.users().rate(profile.as_deref())?;
        // The identity the request was sent as: the client loaded the credential once, and
        // the meter names it with its fingerprint.
        let identity = gh.meter().as_.unwrap_or(IdentityRef::Anonymous);
        let label = identity.label().to_string();
        rows.extend(snapshots.iter().map(|s| row(&label, "api", s)));
        let book = gh.rate_book().read(&rate::book_key(&identity));
        rows.extend(book.iter().map(|s| row(&label, "book", s)));
    }
    let receipt = Receipt::new(&inv.verb_name()).metered(&gh.meter());
    Ok(Outcome {
        answer: Answer::Items {
            noun: Noun::Rate,
            items: rows,
            completeness: Completeness::Complete,
        },
        receipt,
    })
}
