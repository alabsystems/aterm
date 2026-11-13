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
//!
//! One that no member seated drops at the next rotation (S1's decision (b)): its sync opens no
//! locker ([`Stored::Revoked`]), as a removed Mac's does, but no act it can read took it out.
//! [`seat`] judges such a Mac's log too and reads it [`Seat::Out`], and `aterm secrets status`
//! says it was dropped, not revoked ([`dropped`], [`out_words`]): only a `Remove` revokes
//! (one-mesh E2).

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
    /// (a Mac on F10's envelope alone), or local state that cannot be read. A Mac its vault let
    /// go (`Membership::Revoked`) whose log still seats or wraps it reads unjudged too: it waits
    /// on a heal or a sync, and only [`Seat::Out`] is read for it.
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
    Ok(
        judged_by(paths)?.map_or(Seat::Unjudged, |(membership, log, recipient)| {
            match (membership, judge(&log, &recipient)) {
                // A Mac its vault let go whose log still seats or wraps it waits on a heal or a
                // sync: only `Out` is read for it, so a person's join takes the path it took
                // before E2.
                (Stored::Revoked, Seat::Seated | Seat::Seatless) => Seat::Unjudged,
                (_, seat) => seat,
            }
        }),
    )
}

/// What a [`Seat`] is judged by: the vault's stored membership, the trusted log this Mac holds,
/// and its own recipient.
type JudgedBy = (Stored, Log, crate::age::Recipient);

/// [`JudgedBy`] for a Mac whose locker opened here, in ([`Stored::In`]) or let go since
/// ([`Stored::Revoked`]); `None` where nothing judges ([`Seat::Unjudged`]): no vault, a locker
/// that never opened here, no identity, or no trusted log.
fn judged_by(paths: &Paths) -> crate::Result<Option<JudgedBy>> {
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let membership = load_state(paths)?.membership;
    let opened_here = match membership {
        Stored::In => true,
        Stored::Revoked => !revoked_unjudged(),
        Stored::None | Stored::Pending | Stored::Plain => false,
    };
    if !opened_here {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let Some(configured) = log_entries::trusted_configured_log(paths, &config)? else {
        return Ok(None);
    };
    Ok(Some((membership, configured.log, identity.recipient())))
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

/// Whether `log` names an act that took `recipient` out: a `Remove` (or quorum `Remove`) of a
/// seat that holds it, a node removal of its newest seat or of the founder entry that lists it,
/// or a seat move that superseded the seat holding it with no newer holder
/// ([`Log::recipient_is_removed`]; `log_entries::recipient_denied_by_roster_or_log` with no
/// roster). These are the acts a sync reads before a rotation's omission
/// (`log_entries::configured_entries`).
fn removed_by_an_act(log: &Log, recipient: &crate::age::Recipient) -> bool {
    log.recipient_is_removed(recipient)
        || log_entries::recipient_denied_by_roster_or_log(None, log, recipient)
}

/// Whether this Mac's vault let it go with no act against it that it can read (one-mesh E2): its
/// sync opened no locker here ([`Stored::Revoked`]), the log it holds reads it [`Seat::Out`], a
/// newest `[everyone]` epoch left its recipient out ([`Log::audience_left_out_recipient`]), and
/// that log names no act that took it out (`removed_by_an_act`). A rotation, whose carry is the
/// current seats, left out a recipient no seat bound: a founder that never took its seat, or a
/// Mac let in through the envelope alone (S1's decision (b)). A desk that read that rotation
/// refuses that key, so it comes back as a new Mac, its vault folder set aside; but a seat a
/// member signed for it before reading the rotation, which this Mac has not read yet, reaches it
/// later, and M5's gap heal then wraps it the newest epoch and it reads in again by itself. A
/// roster deny that an admin's revoke of such a Mac writes is sealed to the audience that left it
/// out, so this Mac cannot read it and reads itself dropped. `false` where nothing judges
/// ([`Seat::Unjudged`]), the Mac is in, or local state cannot be read.
#[must_use]
pub fn dropped(paths: &Paths) -> bool {
    if read_dropped() {
        return matches!(config(paths), Ok(Some(_)))
            && load_state(paths).is_ok_and(|state| state.membership == Stored::Revoked);
    }
    judged_by(paths)
        .ok()
        .flatten()
        .is_some_and(|(membership, log, recipient)| {
            membership == Stored::Revoked
                && judge(&log, &recipient) == Seat::Out
                && log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
                && !removed_by_an_act(&log, &recipient)
        })
}

/// What `aterm secrets status` says of a Mac its vault let go (`Membership::Revoked` at `seq`,
/// the newest version it read while out, 0 for one that read only the log, so no version that
/// let it go): dropped where no act it can read took it out ([`dropped_words`]), revoked
/// otherwise.
#[must_use]
pub fn out_words(paths: &Paths, seq: u64) -> String {
    dropped_words(paths, seq).unwrap_or_else(|| format!("revoked{}", out_since(seq)))
}

/// [`out_words`] of a Mac [`dropped`] reads dropped, from one read of its log, and the way back:
/// set its vault folder, which holds its identity, aside, and it comes back as a new Mac. `None`
/// for any other Mac (one an act took out, one that is in, or local state that cannot be read),
/// whose surface says its own words. The surfaces that tell a Mac its vault let it go ask this
/// or [`dropped`] (one-mesh E2): `aterm secrets status`, `aterm members status`, `aterm status`,
/// the setup's vault item, System State's Secrets row, the fleet sync's peers read, and Network
/// State's secrets and accounts cards.
#[must_use]
pub fn dropped_words(paths: &Paths, seq: u64) -> Option<String> {
    dropped(paths).then(|| {
        format!(
            "dropped: no seat; a rotation left this Mac out with no Remove{}; set the vault \
             folder ({}) aside to come back as a new Mac",
            out_since(seq),
            paths.dir.display()
        )
    })
}

/// How a status line dates a Mac its vault let go at `seq` (`Membership::Revoked`): " (out as of
/// seq N)", N the newest version it read while out, never the version that let it go; nothing
/// for 0, a Mac that read only the log.
#[must_use]
pub fn out_since(seq: u64) -> String {
    if seq == 0 {
        String::new()
    } else {
        format!(" (out as of seq {seq})")
    }
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

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// NEGATIVE CONTROL (one-mesh E2, f204): while set on a thread, [`seat`] reads a Mac its
    /// vault let go ([`Stored::Revoked`]) [`Seat::Unjudged`] and [`dropped`] answers `false`
    /// there, as before E2, so a recipient a rotation dropped with no `Remove` reads "revoked".
    /// A whole process (a lab instance) turns it on with the negative control `revoked-unjudged`
    /// (`aterm_types::negative_control::on`).
    pub(crate) static REVOKED_UNJUDGED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether this build reads a Mac its vault let go unjudged ([`REVOKED_UNJUDGED`]); never in a
/// release build.
fn revoked_unjudged() -> bool {
    #[cfg(any(test, debug_assertions))]
    return REVOKED_UNJUDGED.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("revoked-unjudged");
    #[cfg(not(any(test, debug_assertions)))]
    false
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// While set on a thread ([`test_read_dropped`]), [`dropped`] answers there for every
    /// configured Mac its vault let go ([`Stored::Revoked`]) as for one a rotation left out.
    static READ_DROPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread stands in for the dropped judgment ([`READ_DROPPED`]); never in a
/// release build.
fn read_dropped() -> bool {
    #[cfg(any(test, debug_assertions))]
    return READ_DROPPED.with(std::cell::Cell::get);
    #[cfg(not(any(test, debug_assertions)))]
    false
}

/// Run `read` with [`dropped`], and so [`dropped_words`] and [`out_words`], reading every
/// configured Mac its vault let go ([`Stored::Revoked`]) as dropped on this thread, its log
/// unread: the stand-in another crate's test reads for what its surface says of a dropped Mac,
/// whose fixture holds no rotation that left it out.
/// `a_recipient_a_rotation_drops_with_no_remove_reads_out_and_says_dropped` holds the judgment
/// itself to a real rotation. The setting is put back when `read` returns or unwinds.
#[cfg(any(test, debug_assertions))]
pub fn test_read_dropped<T>(read: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            READ_DROPPED.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(READ_DROPPED.with(|cell| cell.replace(true)));
    read()
}

/// Whether this Mac asks ALab's members for a seat: its vault is the ALab vault at a URL, its
/// locker opened ([`Stored::In`]), its log reads it as [`Seat::Seatless`], and its identity is no
/// lab identity (`client::refuse_lab_member_browse`: a lab identity founds its own lab and asks
/// the live ALab nothing). `aterm members status`, `aterm secrets status` and `aterm members join`
/// read this, so what they say is what the subscriber does ([`asks_for_seat`]), and so does the
/// machine title's reader at each change of the vault's saved state, which is why the seat read
/// is memoized here too ([`seat_memoized`]).
#[must_use]
pub fn seat_asks(paths: &Paths) -> bool {
    asks_with(paths, &seat_memoized)
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

/// What a [`SEATS`] entry was read at: every file the selection of the records' data is judged
/// by, as it stood ([`crate::records_storage::standing`]: the entry and its pin, its intent where
/// neither of those stands, and a layout's header and data file), and `source.toml`
/// ([`fsx::Seen`]). A seat changes only with
/// the records or the configuration (its pin and handoffs); the state's move off `In` is checked
/// on every call before the memo is read. The data file is among them because a record that
/// arrives is put on its end in place (`persist_records`), which in a layout changes nothing
/// at the records entry, a directory.
type Mark = (crate::records_storage::Standing, fsx::Seen);

/// The last judged [`seat`] (never [`Seat::Unjudged`]) for each vault folder, with its [`Mark`]:
/// a subscriber round asks [`asks_for_seat`] every round.
type Memo = std::collections::BTreeMap<PathBuf, (Mark, Seat)>;
static SEATS: std::sync::Mutex<Memo> = std::sync::Mutex::new(Memo::new());

/// [`Mark`] of `paths` now; `None` where the records' selection refuses its files or
/// `source.toml` cannot be seen.
fn mark(paths: &Paths) -> Option<Mark> {
    Some((
        crate::records_storage::standing(&paths.records_file(), None)?,
        fsx::seen_if_present(&paths.source_file()).ok()??,
    ))
}

/// [`seat`], memoized by [`Mark`]: an unreadable mark or an unjudged seat is read afresh.
fn seat_memoized(paths: &Paths) -> Seat {
    let mark = mark(paths);
    let seats = || {
        SEATS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some(mark) = &mark
        && let Some((at, held)) = seats().get(&paths.dir)
        && at == mark
    {
        return *held;
    }
    let read = seat(paths);
    match mark {
        Some(mark) if read != Seat::Unjudged => {
            seats().insert(paths.dir.clone(), (mark, read));
        }
        _ => {
            seats().remove(&paths.dir);
        }
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

/// Merge a guarded proof's seat while its caller holds the state lock and has checked its
/// exact source. A seat never replaces the existing configuration, state, files or keys.
pub(super) fn take_seat_admitted_held(
    paths: &Paths,
    bundle: &crate::members::Bundle,
    admission: &crate::members::join::ProofAdmission<'_>,
) -> crate::Result<Report> {
    let store = seat_records(paths, bundle)?;
    retained_history::validate_write(paths, &store)?;
    let source_crossed = matches!(
        &bundle.handed,
        crate::members::bundle::Handed::Log { source: Some(source), .. } if source.crossed
    );
    let crosses = source_crossed
        || crate::crossing::past_legacy(
            store
                .log()
                .map_or(0, crate::membership_log::Log::held_count),
        );
    let publish = || {
        admission.check(paths)?;
        write_records(paths, &store)?;
        match seat(paths) {
            Seat::Seated => Ok(Report::default()),
            _ => Err(Error::new(
                "the member's records do not seat this Mac's recipient",
            )),
        }
    };
    if crosses {
        admission.check(paths)?;
        after_crossed_mark(&crossing_mark(paths), publish)
    } else {
        publish()
    }
}

#[cfg(test)]
#[path = "seat_ask_tests.rs"]
mod tests;
