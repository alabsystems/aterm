// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How full this Mac's membership log is, as `aterm secrets status` and its JSON say it: the
//! reading of [`crate::membership_log::Log::fill`] over the records this Mac holds, with each
//! seat named by its label. Read-only, and from record headers alone: no body is opened, and
//! nothing here holds an entry's name or value.

use super::{Paths, load_records};
use crate::membership_log::budgets::Projected;
use crate::membership_log::{Class, Fill, MemberId};
use aterm_json::{Value, json};
use std::collections::BTreeMap;

/// The fill of the log this Mac holds, and the label of each seat that signed in its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFill {
    /// The reading.
    pub fill: Fill,
    /// The label of each seat of [`crate::membership_log::budgets::Signed::seats`], as the log
    /// shows it (`m22`); a seat the log names no label for is `a Mac`.
    labels: BTreeMap<MemberId, String>,
}

/// The fill of the membership log `paths` holds, read at the clock's time; `None` where this
/// Mac holds no log (no vault, or a vault with no log yet).
///
/// # Errors
/// The records cannot be read.
pub fn log_fill(paths: &Paths) -> crate::Result<Option<LogFill>> {
    let store = load_records(paths)?;
    let Some(log) = store.log() else {
        return Ok(None);
    };
    let fill = log.fill(aterm_time::unix_millis());
    let membership = log.membership();
    let labels = fill
        .signed
        .iter()
        .flat_map(|signed| &signed.seats)
        .map(|seat| {
            (
                seat.seat,
                super::remove::label_of(log, &membership, &seat.seat),
            )
        })
        .collect();
    Ok(Some(LogFill { fill, labels }))
}

impl LogFill {
    fn label(&self, seat: &MemberId) -> String {
        self.labels
            .get(seat)
            .cloned()
            .unwrap_or_else(|| "a Mac".to_owned())
    }

    /// `aterm secrets status`' line ([`Fill::line`]).
    #[must_use]
    pub fn line(&self) -> String {
        self.fill.line(|seat| self.label(seat))
    }

    /// The same reading as `aterm secrets status --json` carries it under `log`: bytes of the
    /// bound, each kind's records and bytes, each class's stop, room and whether it is open,
    /// what every seat signed in 24 hours, and the milliseconds to the bound at that rate.
    /// What is not measured is the string `unread`; a rate of nothing is `never`.
    #[must_use]
    pub fn json(&self) -> Value {
        let fill = &self.fill;
        let mut by_kind = aterm_json::Map::new();
        for (kind, records, bytes) in fill.by_kind {
            by_kind.insert(
                kind.word().to_owned(),
                json!({"records": records, "bytes": bytes}),
            );
        }
        let mut classes = aterm_json::Map::new();
        for class in Class::ALL {
            classes.insert(
                class.word().to_owned(),
                json!({
                    "stop": fill.stops[class.index()],
                    "room": fill.room(class),
                    "open": fill.open(class),
                }),
            );
        }
        let signed = fill.signed.as_ref().map_or_else(
            || json!("unread"),
            |signed| {
                let seats: Vec<Value> = signed
                    .seats
                    .iter()
                    .map(|seat| json!({"seat": self.label(&seat.seat), "bytes": seat.bytes}))
                    .collect();
                json!({"bytes": signed.bytes, "seats": seats})
            },
        );
        let full_in_ms = match fill.projected() {
            Projected::Unread => json!("unread"),
            Projected::Never => json!("never"),
            Projected::In(ms) => json!(ms),
        };
        json!({
            "bytes": fill.bytes,
            "bound": fill.bound,
            "by_kind": Value::Object(by_kind),
            "classes": Value::Object(classes),
            "signed_24h": signed,
            "full_in_ms": full_in_ms,
        })
    }
}
