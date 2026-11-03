// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic account claims on the private physical-LAN profiles. The ordinary
//! app replicates the rows; this probe opens no provider and performs no repair.

use super::{Fail, Value, fail, hex, json, open};
use aterm_agent::harness::claims::{Book, Claim, HORIZON_S, Kind, LeaseKey, Row, View};
use aterm_secrets::client::Paths;
use std::path::Path;

const EMAIL: &str = "commons-lan-claims@example.invalid";

fn now() -> Result<i64, Fail> {
    aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms / 1000).ok())
        .ok_or_else(|| Fail::die("system clock cannot place the synthetic claim"))
}

fn key() -> Result<LeaseKey, Fail> {
    LeaseKey::new("claude", EMAIL).map_err(fail)
}

fn rows(paths: &Paths) -> Result<(String, Book), Fail> {
    let read = aterm_db::accounts::claims::read(paths).map_err(fail)?;
    let me = hex::encode_lower(&read.me);
    let rows = read
        .rows
        .iter()
        .map(|(machine, body)| Row::from_value(&hex::encode_lower(machine), body))
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    Ok((me, Book::of(rows)))
}

fn snapshot(paths: &Paths) -> Result<Value, Fail> {
    let (me, book) = rows(paths)?;
    let at = now()?;
    let holder = book.senior(
        &key()?,
        &View {
            me: me.clone(),
            presence: None,
            // Synthetic snapshot: this view holds for no peer, as it applies no presence.
            held: Vec::new(),
            skewed: std::collections::BTreeMap::new(),
        },
        at,
    );
    Ok(json!({
        "kind":"synthetic-account-claims", "me":me, "observed_at":at,
        "provider":"claude", "account":EMAIL, "presence_applied":false,
        "rows":book.rows().map(|row|json!({
            "machine":row.machine,"body":row.to_value()
        })).collect::<Vec<_>>(),
        "holder":holder.map(|holder|json!({
            "machine":holder.machine,"label":holder.label,"kind":holder.kind.word(),
            "since":holder.since,"until":holder.until
        })),
    }))
}

/// The claims of the profile at `root`; one of the live default ALab vault opens only where
/// `live` (`open`).
pub(super) fn read(root: &Path, live: bool) -> Result<Value, Fail> {
    let (world, _) = open(root, live)?;
    snapshot(&Paths::for_home(&world.home))
}

/// Write exactly the caller's revision, so repeats and stale updates exercise
/// the production compare-and-append path. The account is always synthetic. A profile of the
/// live default ALab vault opens only where `live` (`open`).
pub(super) fn write(
    root: &Path,
    counter: &str,
    since: Option<&str>,
    until: &str,
    live: bool,
) -> Result<Value, Fail> {
    let counter = counter
        .parse::<u64>()
        .ok()
        .filter(|counter| *counter > 0)
        .ok_or_else(|| Fail::usage("claim counter must be positive"))?;
    let until = until
        .parse::<i64>()
        .map_err(|_| Fail::usage("claim expiry must be a Unix timestamp"))?;
    let at = now()?;
    if until <= at || until.saturating_sub(at) > HORIZON_S {
        return Err(Fail::usage("claim expiry must be within the next day"));
    }
    let since = since
        .map(|since| {
            since
                .parse::<i64>()
                .ok()
                .filter(|since| (0..=at).contains(since))
                .ok_or_else(|| Fail::usage("claim start must be a past Unix timestamp"))
        })
        .transpose()?;
    let (world, role) = open(root, live)?;
    let paths = Paths::for_home(&world.home);
    let (me, _) = rows(&paths)?;
    let row = Row {
        machine: me,
        label: if role == "source" { "m1" } else { "m2" }.into(),
        counter,
        until,
        claims: since
            .map(|since| {
                key().map(|key| Claim {
                    key,
                    kind: Kind::Lease,
                    since,
                })
            })
            .transpose()?
            .into_iter()
            .collect(),
    };
    aterm_db::accounts::claims::write(&paths, &row.to_value()).map_err(fail)?;
    snapshot(&paths)
}
