// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Relay rows (design §4.17), the fix for relays that all overlapped: accounts fall into 16
//! buckets by the first nibble of their key's SHA-256, and a Mac relays its two buckets
//! (`typed::owned::relay_duty`) plus any bucket no other Mac relays in its window's tables, so
//! every bucket stays covered as Macs come and go. A bucket's row lists, per account, the newest
//! `use`, `signin` and `signout` this Mac knows from any Mac, each `{a, m, t, k, p}`: the account,
//! the Commons machine uuid that saw it (named the way every journal record names a Mac, through
//! its `machine-facts` row, design §6), the time, the kind, and the week of the table the fact was
//! first read from. A relayed time is believed only within that week ± 1, so a wrong clock cannot
//! carry a fact forward for ever. Re-written each week, a relay keeps an account's last use after
//! the table that held it is deleted.

use super::{
    book::{VERSION, WEEK},
    event::{Event, Kind, Pk},
};
use crate::{
    Result,
    typed::owned::{caps, fit, relay_bucket, relay_duty},
};
use aterm_codec::hex;
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// What a relay entry says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rk {
    Use,
    SignIn,
    SignOut,
}

impl Rk {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Use => "use",
            Self::SignIn => "signin",
            Self::SignOut => "signout",
        }
    }

    pub(crate) fn parse(word: &str) -> Option<Self> {
        [Self::Use, Self::SignIn, Self::SignOut]
            .into_iter()
            .find(|k| k.word() == word)
    }
}

/// One relayed fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Entry {
    /// The Mac that saw it.
    pub m: [u8; 16],
    pub t: u64,
    /// The week of the table it was first read from.
    pub p: u64,
}

/// Whether a fact at `t` first read from week `p`'s table is believed: within that week ± 1.
pub(crate) fn believed(t: u64, p: u64) -> bool {
    t.saturating_add(WEEK) >= p.saturating_mul(WEEK) && t < p.saturating_add(2).saturating_mul(WEEK)
}

/// One relay entry ranked for truncation: by kind, then newest first by (time, machine).
type Ranked = (Rk, std::cmp::Reverse<(u64, [u8; 16])>, Pk, Entry);

/// What the window's tables say about each account's last use and sign-ins, and which buckets
/// other Macs relay.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Heard {
    pub newest: BTreeMap<(Pk, Rk), Entry>,
    /// Buckets a Mac other than this one relays in the window's tables.
    pub covered: BTreeSet<u8>,
}

impl Heard {
    /// Offer one fact: kept when believed and newer, by (time, machine), than the one held.
    pub(crate) fn offer(&mut self, account: Pk, kind: Rk, entry: Entry) {
        if !believed(entry.t, entry.p) || entry.t == 0 {
            return;
        }
        let key = (account, kind);
        if self
            .newest
            .get(&key)
            .is_none_or(|held| (held.t, held.m) < (entry.t, entry.m))
        {
            self.newest.insert(key, entry);
        }
    }

    /// Read a `use` row of `machine` from week `period`'s table, for the account `account`.
    pub(crate) fn read_use(&mut self, account: Pk, machine: [u8; 16], period: u64, row: &Value) {
        let at = |key: &str| row.get(key).and_then(Value::as_u64).unwrap_or(0);
        let entry = |t| Entry {
            m: machine,
            t,
            p: period,
        };
        self.offer(
            account,
            Rk::Use,
            entry(at("last_start").max(at("last_end"))),
        );
        if let Some(signin) = row.get("signin") {
            let t = signin.get("t").and_then(Value::as_u64).unwrap_or(0);
            match signin.get("state").and_then(Value::as_str) {
                Some("in") => self.offer(account, Rk::SignIn, entry(t)),
                Some("out" | "ended") => self.offer(account, Rk::SignOut, entry(t)),
                _ => {}
            }
        }
    }

    /// Read one event `machine` logged, from week `period`'s table.
    pub(crate) fn read_event(&mut self, machine: [u8; 16], period: u64, event: &Event) {
        let kind = match event.kind {
            Kind::SessionStart | Kind::SessionEnd => Rk::Use,
            Kind::SignedIn | Kind::Renewed => Rk::SignIn,
            Kind::SignedOut | Kind::LoginEnded | Kind::Displaced => Rk::SignOut,
            _ => return,
        };
        let Some(account) = event.account else {
            return;
        };
        self.offer(
            account,
            kind,
            Entry {
                m: machine,
                t: event.t,
                p: period,
            },
        );
    }

    /// Read a relay row `writer` wrote into week `period`'s table (for a row this Mac's book
    /// stated, the week it was written). An entry claiming a source week after `period` is
    /// dropped: a fact cannot have been first read from a table later than the one relaying it,
    /// and `believed` alone would let a row whose `p` and `t` both lie in the future outrank every
    /// real use, then ride every honest Mac's weekly restatement for good.
    ///
    /// A row of a Mac other than `me` covers its bucket when that bucket is one of the writer's
    /// own two, or the writer's uuid sorts below `me`'s: of two Macs filling the same gap, the
    /// lower keeps relaying and the higher stands down, so the gap stays filled every week
    /// rather than both dropping it at once and both taking it back the week after.
    pub(crate) fn read_relay(&mut self, writer: [u8; 16], me: &[u8; 16], row: &Value, period: u64) {
        let Some(bucket) = row
            .get("b")
            .and_then(Value::as_u64)
            .and_then(|b| u8::try_from(b).ok())
            .filter(|b| *b < caps::RELAY_BUCKETS)
        else {
            return;
        };
        if writer != *me && (relay_duty(&writer).contains(&bucket) || writer < *me) {
            self.covered.insert(bucket);
        }
        for e in row.get("e").and_then(Value::as_array).into_iter().flatten() {
            let parsed = (|| {
                let account = Pk::new(e.get("a")?.as_str()?)?;
                let kind = Rk::parse(e.get("k")?.as_str()?)?;
                let m = crate::database::decode_hex::<16>(e.get("m")?.as_str()?).ok()?;
                let entry = Entry {
                    m,
                    t: e.get("t")?.as_u64()?,
                    p: e.get("p")?.as_u64()?,
                };
                (relay_bucket(account.as_str()) == bucket && entry.p <= period)
                    .then_some((account, kind, entry))
            })();
            if let Some((account, kind, entry)) = parsed {
                self.offer(account, kind, entry);
            }
        }
    }

    /// The buckets this Mac relays: its own two, and every bucket no other Mac relays.
    pub(crate) fn duty(&self, me: &[u8; 16]) -> BTreeSet<u8> {
        let mut duty: BTreeSet<u8> = relay_duty(me).into_iter().collect();
        duty.extend((0..caps::RELAY_BUCKETS).filter(|b| !self.covered.contains(b)));
        duty
    }

    /// Bucket `bucket`'s row at counter `n` in week `period`, within `cap` bytes, or none when
    /// nothing is known of its accounts. Over the cap the oldest `signout` entries go first,
    /// then the oldest `signin` entries; an account's newest `use` goes last of all.
    ///
    /// # Errors
    /// Even an empty row passes `cap`.
    pub(crate) fn row(&self, bucket: u8, n: u64, period: u64, cap: usize) -> Result<Option<Value>> {
        let mut entries: Vec<Ranked> = self
            .newest
            .iter()
            .filter(|((account, _), _)| relay_bucket(account.as_str()) == bucket)
            .map(|((account, kind), e)| (*kind, std::cmp::Reverse((e.t, e.m)), *account, *e))
            .collect();
        if entries.is_empty() {
            return Ok(None);
        }
        entries.sort_unstable();
        let values: Vec<Value> = entries
            .iter()
            .map(|(kind, _, account, e)| {
                json!({"a": account.as_str(), "k": kind.word(), "m": hex::encode_lower(&e.m),
                    "p": e.p, "t": e.t})
            })
            .collect();
        let wrap = |kept: &[Value]| json!({"r": VERSION, "n": n, "period": period, "b": bucket, "e": kept});
        let kept = fit(&values, cap, wrap)?;
        Ok(Some(wrap(&values[..kept])))
    }
}
