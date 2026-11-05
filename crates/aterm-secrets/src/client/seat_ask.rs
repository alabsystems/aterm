// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A live seatless recipient takes its seat by itself (one-mesh S1,
//! `docs/DESIGN-alab-one-mesh-2026-11-05.md` §8 Step 0 order 4, §11 wave A).
//!
//! A seatless recipient is a Mac whose own age recipient a newest `[everyone]` epoch of the log it
//! holds wraps while no seat of that log binds it: an approver let it in through the envelope
//! (F10) and no `Admit` followed, or the genesis lists it as a founder that never proved itself.
//! Its vault opens, so before S1 its client read itself as a member (`member as …`,
//! `membership: in`) and nothing asked for a seat. Since the carry became the current seats (M1),
//! the next rotation of `[everyone]` or Commons drops such a recipient, and it loses the vault.
//!
//! So a seatless recipient's vault subscriber asks ALab's members for a seat with the keys it
//! holds: the same request (age recipient, writer key, device key) and its fleet node key, through
//! the ask a waiting Mac makes (`ask_members_if_due`, `members::join::prove_first`), on the same
//! spacing (one ask per round at most, then pauses up to 15 minutes), and under the same home-LAN
//! rule at the desk (`client::readmit`). Nothing new is minted, nothing rotates, and a lab
//! identity never asks. Once seated, the ask stops at the next round.

use super::*;
use crate::membership_log::{EpochAudience, Log};

/// Where this Mac's own recipient stands in the membership log it holds ([`seat`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
    /// A current seat of the log binds this Mac's recipient.
    Seated,
    /// A newest `[everyone]` epoch of the log wraps this Mac's recipient and no seat binds it:
    /// it asks for one, with the same keys.
    Seatless,
    /// The log removed it, a newest `[everyone]` epoch left it out, a removed seat holds its
    /// recipient, or no newest epoch wraps it: no seat comes by asking, and none is asked for.
    Out,
    /// Nothing to judge by: no vault, a vault whose locker never opened here, no trusted log
    /// (a Mac on F10's envelope alone), or local state that cannot be read.
    Unjudged,
}

impl Seat {
    /// The words `aterm members status` and `aterm secrets status` say of a seatless Mac in
    /// place of a member's.
    pub const SEATLESS: &'static str = "seatless: asking for a seat";
}

/// Where this Mac's recipient stands in its log ([`Seat`]), read from local files alone: the
/// vault's state, its identity and its records, never a dial.
#[must_use]
pub fn seat(paths: &Paths) -> Seat {
    seat_read(paths).unwrap_or(Seat::Unjudged)
}

fn seat_read(paths: &Paths) -> crate::Result<Seat> {
    let Some(config) = config(paths)? else {
        return Ok(Seat::Unjudged);
    };
    if load_state(paths)?.membership != Stored::In {
        return Ok(Seat::Unjudged);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(Seat::Unjudged);
    };
    let Some(configured) = log_entries::trusted_configured_log(paths, &config)? else {
        return Ok(Seat::Unjudged);
    };
    Ok(judge(&configured.log, &identity.recipient()))
}

/// What `log` says of the Mac whose recipient is `recipient` ([`Seat`]).
pub(super) fn judge(log: &Log, recipient: &crate::age::Recipient) -> Seat {
    if log
        .current_recipients(&EpochAudience::Everyone)
        .contains(recipient)
    {
        return Seat::Seated;
    }
    let held_by_a_seat = log
        .membership()
        .members()
        .values()
        .any(|seat| seat.binding.recipient() == *recipient);
    if held_by_a_seat
        || log.recipient_is_removed(recipient)
        || log.audience_left_out_recipient(&EpochAudience::Everyone, recipient)
        || !log.wraps_maximal_audience_to(&EpochAudience::Everyone, recipient)
    {
        return Seat::Out;
    }
    Seat::Seatless
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// NEGATIVE CONTROL (one-mesh S1): while set on a thread, [`asks_for_seat`] answers `false`
    /// there, as 0.154.0 and before did for every Mac whose locker opened, so a seatless
    /// recipient asks for nothing and the next rotation drops it. A whole process (a lab
    /// instance) turns it on with the negative control `no-seat-ask`
    /// (`aterm_types::negative_control::on`).
    pub(crate) static NO_SEAT_ASK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this build's self-ask is off ([`NO_SEAT_ASK`]); never in a release build.
fn self_ask_off() -> bool {
    #[cfg(any(test, debug_assertions))]
    return NO_SEAT_ASK.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("no-seat-ask");
    #[cfg(not(any(test, debug_assertions)))]
    false
}

/// Whether this Mac asks ALab's members for a seat: its vault is the ALab vault at a URL, its
/// locker opened ([`Stored::In`]), its log reads it as [`Seat::Seatless`], and its identity is no
/// lab identity (`client::refuse_lab_member_browse`: a lab identity founds its own lab and asks
/// the live ALab nothing). `aterm members status`, `aterm secrets status` and `aterm members join`
/// read this, so what they say is what the subscriber does ([`asks_for_seat`]).
#[must_use]
pub fn seat_asks(paths: &Paths) -> bool {
    asks_with(paths, &seat)
}

/// [`seat_asks`], `seat_of` reading the seat once the cheap conditions hold.
fn asks_with(paths: &Paths, seat_of: &dyn Fn(&Paths) -> Seat) -> bool {
    let alab = config(paths)
        .ok()
        .flatten()
        .is_some_and(|config| matches!(config.source, Source::Url(_)) && alab_vault(&config));
    alab && load_state(paths)
        .ok()
        .is_some_and(|state| state.membership == Stored::In)
        && seat_of(paths) == Seat::Seatless
        && refuse_lab_member_browse(paths).is_ok()
}

/// What a [`SEATS`] entry was read at: the records file's and `source.toml`'s size and change
/// time. A seat changes only with the records or the configuration (its pin and handoffs); the
/// state's move off `In` is checked on every call before the memo is read.
type Mark = [Option<(u64, std::time::SystemTime)>; 2];

/// The last judged [`seat`] (never [`Seat::Unjudged`]) for each vault folder, with its [`Mark`]:
/// a subscriber round asks [`asks_for_seat`] every round.
type Memo = std::collections::BTreeMap<PathBuf, (Mark, Seat)>;
static SEATS: std::sync::Mutex<Memo> = std::sync::Mutex::new(Memo::new());

fn file_mark(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// [`seat`], memoized by [`Mark`]: an unreadable mark or an unjudged seat is read afresh.
fn seat_memoized(paths: &Paths) -> Seat {
    let mark = [
        file_mark(&paths.records_file()),
        file_mark(&paths.source_file()),
    ];
    let known = mark.iter().all(Option::is_some);
    let seats = || {
        SEATS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if known
        && let Some((at, held)) = seats().get(&paths.dir).copied()
        && at == mark
    {
        return held;
    }
    let read = seat(paths);
    if known && read != Seat::Unjudged {
        seats().insert(paths.dir.clone(), (mark, read));
    } else {
        seats().remove(&paths.dir);
    }
    read
}

/// Whether this Mac's vault subscriber asks ALab's members for a seat on this round
/// ([`seat_asks`], the seat read memoized), unless the negative control is on.
pub(super) fn asks_for_seat(paths: &Paths) -> bool {
    !self_ask_off() && asks_with(paths, &seat_memoized)
}

/// The records of a log bundle a member handed this seatless Mac, checked: the bundle allows
/// this Mac's own recipient, and its records, merged into the log this Mac holds, seat it. The
/// merged store, unwritten.
fn seat_records(
    paths: &Paths,
    bundle: &crate::members::Bundle,
) -> crate::Result<crate::membership_log::Store> {
    let crate::members::bundle::Handed::Log { records, .. } = &bundle.handed else {
        return Err(Error::new("a member handed files, not the log, for a seat"));
    };
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no vault identity"))?;
    if bundle.allowed.binding().recipient() != identity.recipient() {
        return Err(Error::new("the member's bundle allows another Mac"));
    }
    let mut store = load_records(paths)?;
    store.ingest(records)?;
    let seated = store
        .log()
        .is_some_and(|log| judge(log, &identity.recipient()) == Seat::Seated);
    if !seated {
        return Err(Error::new(
            "the member's records do not seat this Mac's recipient",
        ));
    }
    Ok(store)
}

/// Whether this seatless Mac takes the log bundle a member handed it ([`take_seat`]'s checks),
/// writing nothing: what a phrase run acknowledges a bundle by (`members::join`).
///
/// # Errors
/// As [`take_seat`].
pub fn accepts_seat(paths: &Paths, bundle: &crate::members::Bundle) -> crate::Result<()> {
    seat_records(paths, bundle).map(drop)
}

/// Take the seat a member handed this seatless Mac ([`Seat::Seatless`]), by its proof or a
/// person's Allow (`members::join`): it already holds the vault, so it writes no file and keeps
/// its configuration, its state and its keys, and merges the member's records (its `Admit` and
/// the `Wrap`s beside it) into the log it holds, each verified as every record is
/// ([`save_records`]), once they are checked to seat this Mac's own recipient. It is for a
/// seatless Mac only: the caller sends any other Mac to [`write_from_log_bundle`].
///
/// # Errors
/// The bundle carries no log or allows another recipient, its records do not verify or merge,
/// or the log they make does not seat this Mac (nothing is written then).
pub fn take_seat(paths: &Paths, bundle: &crate::members::Bundle) -> crate::Result<Report> {
    let crate::members::bundle::Handed::Log { records, .. } = &bundle.handed else {
        return Err(Error::new("a member handed files, not the log, for a seat"));
    };
    seat_records(paths, bundle)?;
    save_records(paths, records)?;
    match seat(paths) {
        Seat::Seated => Ok(Report::default()),
        _ => Err(Error::new(
            "the member's records do not seat this Mac's recipient",
        )),
    }
}

#[cfg(test)]
#[path = "seat_ask_tests.rs"]
mod tests;
