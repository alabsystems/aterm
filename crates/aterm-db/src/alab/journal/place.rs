// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `place` rows (design §0, §4.17): this Mac's automatic use of an account no preference of its
//! own names, keyed `<account pk>.<uuid32>`, so "which Macs hold A" and "what runs on m2" see
//! what a Mac runs beside what it prefers. A place row says what a Mac does and never what it may
//! do: no reader refuses, drops or signs out anything because a row is there or is not.
//!
//! The harness decides what a Mac holds (its rosters, the lab sync's own keep and add, step 11)
//! and reports it as [`Holding`]s through the hook the `aterm` executable installs
//! ([`install_holdings`]); the flusher folds them into its book ([`super::book::Book::hold`])
//! and writes the rows inside the journal's fixed shares, in the same records as the `use` rows,
//! restating a live one in each new week so it is carried while it holds and lapses with its
//! weeks once the Mac is gone. [`read`] is the reader's half: every live row in the window, by
//! the Mac that wrote it.

use super::event::Pk;
use aterm_json::Value;
use std::sync::OnceLock;

/// Why this Mac runs an account no preference of its own names (the `place.why` enum).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Why {
    /// No live Mac prefers it and it may be signed in: this Mac fills its pool with it (its
    /// default login, an account asked for by name, a row an earlier source gave it).
    Fill,
    /// Another live Mac prefers it.
    Borrow,
    /// No live Mac prefers it and the lab says it may not be signed in (paused, retired,
    /// barred): the row stays, as every row does, with nothing to run.
    Stranded,
    /// Not run or signed in here yet: no session start and no sign-in this Mac's journal saw.
    Probation,
}

impl Why {
    /// The word a row carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Borrow => "borrow",
            Self::Stranded => "stranded",
            Self::Probation => "probation",
        }
    }

    /// The reason a word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [Self::Fill, Self::Borrow, Self::Stranded, Self::Probation]
            .into_iter()
            .find(|why| why.word() == word)
    }
}

/// One account this Mac holds a row of that no preference here names, as the harness decides
/// it: the base reason ([`Why::Fill`], [`Why::Borrow`] or [`Why::Stranded`]); the flusher says
/// [`Why::Probation`] instead while its own book shows the account never ran or signed in here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Holding {
    pub account: Pk,
    pub why: Why,
}

/// What reports this Mac's holdings: `None` where they are not known now (no lab names this Mac,
/// a roster did not read), which leaves every place row as the book holds it, so a passing
/// failure never ends a row.
pub type Holdings = dyn Fn() -> Option<Vec<Holding>> + Send + Sync;

static HOLDINGS: OnceLock<Box<Holdings>> = OnceLock::new();

/// Install this process's holdings report (the `aterm` executable's, from the lab and its
/// rosters); the first install wins. With none installed the flusher writes no place row.
pub fn install_holdings(report: Box<Holdings>) -> bool {
    HOLDINGS.set(report).is_ok()
}

/// The installed report's holdings now; `None` with none installed.
pub(crate) fn holdings() -> Option<Vec<Holding>> {
    (HOLDINGS.get()?)()
}

/// One live `place` row of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The Commons machine uuid that wrote it (32 hex), which `machine-facts` binds to a machine.
    pub uuid: String,
    pub account: String,
    pub why: Why,
    pub since: u64,
    pub until: u64,
}

/// What the journal window says of where accounts run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    /// Every live place row (its `until` still ahead), by account then writer.
    pub rows: Vec<Placed>,
}

/// The live place rows among `places` (the window's owned read) at `now`: a row whose `why` is no
/// word of the enum, whose key names no account, or whose `until` has passed is no row.
///
/// No Mac's set of rows is read as all it runs: a Mac states holdings only while its harness
/// reads the lab (`[alab] roster` on, a release build, rosters that read), and its `sum` row's
/// `journal: on` says none of that, so a Mac's missing row never means it holds nothing.
pub(crate) fn live(places: &crate::typed::owned::OwnedRead, now: u64) -> Places {
    let mut rows: Vec<Placed> = places
        .rows
        .iter()
        .filter_map(|(key, row)| {
            let (account, uuid) = key.rsplit_once('.')?;
            Pk::new(account).filter(|pk| pk.as_str().starts_with("ac-"))?;
            let value = &row.value;
            let until = value.get("until").and_then(Value::as_u64)?;
            (until > now).then_some(())?;
            Some(Placed {
                uuid: uuid.to_owned(),
                account: account.to_owned(),
                why: Why::parse(value.get("why").and_then(Value::as_str)?)?,
                since: value.get("since").and_then(Value::as_u64)?,
                until,
            })
        })
        .collect();
    rows.sort_by(|a, b| (&a.account, &a.uuid).cmp(&(&b.account, &b.uuid)));
    Places { rows }
}

/// The journal window's live place rows as this Mac holds them at `now` (Unix seconds), read as a
/// member; `None` where this Mac holds no Commons it is a member of.
///
/// # Errors
/// A window table cannot be observed or read.
pub fn read(paths: &aterm_secrets::client::Paths, now: u64) -> crate::Result<Option<Places>> {
    use crate::{catalog::Directory, table::Session, typed::owned};
    use aterm_secrets::commons::bootstrap::Bootstrap;
    let selected = crate::generation::selected(paths)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let Ok((bootstrap, journal)) = Bootstrap::required_with_journal_until(&selected, deadline)
    else {
        return Ok(None);
    };
    let Ok(session) = Session::from_admitted_journal(&bootstrap, journal) else {
        return Ok(None);
    };
    let directory = Directory::new(&selected, bootstrap.trust());
    let window = super::flush::window(&session, &directory, now)?;
    let resolve = owned::commons::historical(&session);
    let places = owned::read_owned(&super::PLACE, &window.ops, &resolve);
    Ok(Some(live(&places, now)))
}
