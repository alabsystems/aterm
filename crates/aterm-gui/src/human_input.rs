// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tracks when a person last gave a session input (`human_ms=` and `human_seq=`).

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use crate::input::InputEvent;

/// When a person last gave this session input ([`crate::metrics::now_us`] plus one, so `0` is "never" in this process) and how many gestures that has been ([`Self::seq`]).
#[derive(Debug)]
pub(crate) struct HumanInputStamp {
    at: AtomicU64,
    count: AtomicU64,
    /// THE NUMBER OF THE LAST GESTURE THAT REACHED THE PROGRAM,
    /// `0` for none: a key that encodes to
    /// bytes, text, a paste, a wheel the program is sent (a report or
    /// alternate scroll), a mouse press under tracking, a drag the tracking
    /// mode reports ([`reaches_program`]).
    reached: AtomicU64,
    /// THE NUMBER OF THE LAST GESTURE THAT OPENED A BURST,
    /// `0` for none: published after the count and
    /// before [`Self::reached`], so a reading that holds a reaching
    /// gesture's number holds whether it opened a burst.
    burst: AtomicU64,
    /// The same moment on the clock that counts through sleep
    /// (`aterm_uds::process::continuous_ms`) plus one, `0` for never or no
    /// such clock: what the survey's `human_ms=` is taken on
    /// ([`Self::age_ms`]).
    at_continuous: AtomicU64,
    /// The number of the last gesture that was [`Gesture::PlainEnter`]; `0`
    /// for none.
    last_plain_enter: AtomicU64,
    /// THE OUTGOING PROCESS'S AGE OF THE LAST GESTURE, in milliseconds plus
    /// one (so `0` stays "none carried"), as of [`Self::carried_at`]
    /// ([`Self::carry`]): read as the last gesture ([`Self::ms_since`]) only
    /// while no gesture of this process is stamped (`at` is `0`), every one
    /// of which is later, and, whatever was stamped here, as the bound on a
    /// carried typing age with no awake row ([`Self::typed_awake_ms`]).
    carried_ms: AtomicU64,
    /// When the carried age was taken here ([`crate::metrics::now_us`]).
    carried_at: AtomicU64,
    /// WHEN THE PERSON LAST TYPED ([`crate::metrics::now_us`] plus one, `0` for never in this
    /// process): a gesture that types ([`Gesture::types`]: a key press or repeat, text, a key
    /// sequence, a paste or an input method's composition), never a click, a drag, a wheel or a
    /// scroll of the view (`human_typed_ms=`).
    typed_at: AtomicU64,
    /// The outgoing process's typing age, or conservatively its gesture age where a legacy
    /// writer recorded no typing provenance, in milliseconds plus one (`0`: none carried), as
    /// of [`Self::typed_carried_at`] ([`Self::carry_typed`]): read only while no typing of this
    /// process is stamped, every one of which is later.
    typed_carried_ms: AtomicU64,
    /// When the carried typing age was taken here ([`crate::metrics::now_us`]).
    typed_carried_at: AtomicU64,
    /// THE MAC'S SLEEP SINCE BOOT WHEN THE PERSON LAST TYPED (microseconds plus one, `0` for
    /// never or no clock that says it, [`slept_us`]): [`crate::metrics::now_us`] stops while the
    /// Mac sleeps, and the typing's age adds the sleep since (2026-10-30 review 9), as the
    /// harness's hour after the person's key is an hour of the clock on the wall.
    typed_slept: AtomicU64,
    /// The Mac's sleep since boot when the carried typing age was taken here, as
    /// [`Self::typed_slept`] is.
    typed_carried_slept: AtomicU64,
    /// THE OUTGOING PROCESS'S TYPING AGE IN AWAKE TIME, in milliseconds plus one (`0`: none
    /// carried), as of [`Self::typed_awake_carried_at`] ([`Self::carry_typed_awake`]): the
    /// carried [`Self::typed_carried_ms`] counts the sleep before the carry, which the awake
    /// clock a task record crosses on does not ([`Self::typed_awake_ms`]).
    typed_awake_carried_ms: AtomicU64,
    /// When the carried awake typing age was taken here ([`crate::metrics::now_us`]).
    typed_awake_carried_at: AtomicU64,
    /// WHEN THE HOST BEGAN SERVING THE SESSION, on this process's clock
    /// ([`crate::metrics::now_us`], microseconds): its start here, or the
    /// Commit a seamless successor adopted it at — earlier than this
    /// process's own start (negative) where the outgoing process said how
    /// long it served the shell with no person's gesture ([`Self::serve_from`]).
    served_from_us: AtomicI64,
}

impl Default for HumanInputStamp {
    fn default() -> Self {
        Self {
            at: AtomicU64::new(0),
            count: AtomicU64::new(0),
            reached: AtomicU64::new(0),
            burst: AtomicU64::new(0),
            at_continuous: AtomicU64::new(0),
            last_plain_enter: AtomicU64::new(0),
            carried_ms: AtomicU64::new(0),
            carried_at: AtomicU64::new(0),
            typed_at: AtomicU64::new(0),
            typed_carried_ms: AtomicU64::new(0),
            typed_carried_at: AtomicU64::new(0),
            typed_slept: AtomicU64::new(0),
            typed_carried_slept: AtomicU64::new(0),
            typed_awake_carried_ms: AtomicU64::new(0),
            typed_awake_carried_at: AtomicU64::new(0),
            served_from_us: AtomicI64::new(
                i64::try_from(crate::metrics::now_us()).unwrap_or(i64::MAX),
            ),
        }
    }
}

/// What kind of gesture [`HumanInputStamp::note_reaching`] counts, for the plain-Enter marker and the typing stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gesture {
    /// A press of the named Enter key with no modifier held: not a repeat, not keypad Enter, not committed text, a paste or a `[key_sequences]` chord that carries a CR.
    PlainEnter,
    /// Any other key press or repeat, committed text, a key sequence, a paste or an input method's composition: the person typing.
    Typed,
    /// Every other gesture: a click, a drag, a wheel or a scroll of the view.
    Other,
}

impl Gesture {
    /// The class of `ev`, a person's gesture ([`is_person_gesture`]).
    pub(crate) fn of(ev: &InputEvent) -> Self {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        match ev {
            InputEvent::Key {
                key: Key::Named(NamedKey::Enter),
                mods,
                event_type: KeyEventType::Press,
                ..
            } if mods
                .difference(Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK)
                .is_empty() =>
            {
                Self::PlainEnter
            }
            InputEvent::Key { event_type, .. } => {
                if matches!(event_type, KeyEventType::Release) {
                    Self::Other
                } else {
                    Self::Typed
                }
            }
            InputEvent::Text(_) | InputEvent::KeySequence(_) | InputEvent::Paste(..) => Self::Typed,
            _ => Self::Other,
        }
    }

    /// Whether it is the person typing: [`Self::PlainEnter`] or [`Self::Typed`].
    pub(crate) const fn types(self) -> bool {
        matches!(self, Self::PlainEnter | Self::Typed)
    }
}

/// What [`HumanInputStamp::note_reaching`] counted: whether the gesture opens a burst, and its number (the [`HumanInputStamp::seq`] it made).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Noted {
    /// It opens a burst ([`HumanInputStamp::note_reaching`]).
    pub(crate) burst: bool,
    /// Its number.
    pub(crate) seq: u64,
}

/// The person's count and its last program-bound gesture, read together ([`HumanInputStamp::counts`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PersonCounts {
    /// How many gestures are counted ([`HumanInputStamp::seq`]), never below `reached`.
    pub(crate) seq: u64,
    /// The number of the last gesture that reached the program; `0` for none.
    pub(crate) reached: u64,
    /// The number of the last gesture that opened a burst; `0` for none.
    pub(crate) burst: u64,
}

/// The person count with what its last gesture was ([`HumanInputStamp::probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersonProbe {
    /// [`HumanInputStamp::seq`].
    pub(crate) seq: u64,
    /// Gesture `seq` was a [`Gesture::PlainEnter`] (never for `seq` 0).
    pub(crate) plain_enter: bool,
}

impl HumanInputStamp {
    /// [`Self::note_reaching`] for a gesture that reached the program: whether it opens a burst.
    #[cfg(test)]
    pub(crate) fn note(&self, now_us: u64, gesture: Gesture) -> bool {
        self.note_reaching(now_us, gesture, true).burst
    }

    /// A person's gesture of kind `gesture` reached the session at `now_us`, named the last that reached the program ([`Self::counts`]) too when it `reaches` it ([`reaches_program`]): its number, and whether it opens a burst (the first ever, or the first after [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one).
    pub(crate) fn note_reaching(&self, now_us: u64, gesture: Gesture, reaches: bool) -> Noted {
        self.note_reaching_split(now_us, gesture, reaches, || ())
    }

    /// [`Self::note_reaching`], running `between` once the gesture is counted and before its class is published: the one point another thread can read the two apart, where a test puts a reconciling entry (the production call runs nothing there).
    pub(crate) fn note_reaching_split(
        &self,
        now_us: u64,
        gesture: Gesture,
        reaches: bool,
        between: impl FnOnce(),
    ) -> Noted {
        let before = self.at.swap(now_us.saturating_add(1), Ordering::Relaxed);
        if let Some(ms) = aterm_uds::process::continuous_ms() {
            self.at_continuous
                .store(ms.saturating_add(1), Ordering::Relaxed);
        }
        let n = self.count.fetch_add(1, Ordering::Release).wrapping_add(1);
        let burst = before.checked_sub(1).is_none_or(|at| {
            now_us.saturating_sub(at) / 1000 >= crate::session_timeline::HUMAN_BURST_GAP_MS
        });
        between();
        if burst {
            self.burst.fetch_max(n, Ordering::Release);
        }
        if reaches {
            self.reached.fetch_max(n, Ordering::Release);
        }
        if gesture.types() {
            self.stamp_typed(now_us, slept_us());
        }
        if gesture == Gesture::PlainEnter {
            self.last_plain_enter.store(n, Ordering::Release);
        }
        Noted { burst, seq: n }
    }

    /// THE PERSON'S COUNTS AS ONE READING (rulings T2, T39): how many gestures are counted and the number of the last that reached the program, the count never below that number.
    pub(crate) fn counts(&self) -> PersonCounts {
        let reached = self.reached.load(Ordering::Acquire);
        let burst = self.burst.load(Ordering::Acquire);
        let seq = self.count.load(Ordering::Acquire);
        PersonCounts {
            seq: seq.max(reached).max(burst),
            reached,
            burst,
        }
    }

    /// The raw stamp; the tests pair it with [`Self::seq`].
    #[cfg(test)]
    pub(crate) fn last(&self) -> u64 {
        self.at.load(Ordering::Relaxed)
    }

    /// How many person gestures have reached the session in this process (`human_seq=`): equal to an earlier reading exactly when none has since — what a write fenced `if-human=<n>` is checked against.
    pub(crate) fn seq(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// [`Self::seq`] and whether its last gesture was a plain Enter: the count first, then the marker, both acquires.
    pub(crate) fn probe(&self) -> PersonProbe {
        let seq = self.count.load(Ordering::Acquire);
        let enter = self.last_plain_enter.load(Ordering::Acquire);
        PersonProbe {
            seq,
            plain_enter: seq > 0 && enter == seq,
        }
    }

    /// Milliseconds from the last person's gesture to `now_us`; `None` when no person has given the session input — in this process or, carried ([`Self::carry`]), in the one a seamless update adopted it from.
    pub(crate) fn ms_since(&self, now_us: u64) -> Option<u64> {
        if let Some(at) = self.at.load(Ordering::Relaxed).checked_sub(1) {
            return Some(now_us.saturating_sub(at) / 1000);
        }
        self.carried_ms_since(now_us)
    }

    /// Milliseconds from the last gesture the outgoing process carried ([`Self::carry`]) to
    /// `now_us`, in awake time; `None` with none carried.
    fn carried_ms_since(&self, now_us: u64) -> Option<u64> {
        let age = self.carried_ms.load(Ordering::Acquire).checked_sub(1)?;
        let at = self.carried_at.load(Ordering::Relaxed);
        Some(age.saturating_add(now_us.saturating_sub(at) / 1000))
    }

    /// Milliseconds from the typing stamp ([`Self::typed_at`]) to `now_us`, the Mac's sleep since
    /// counted ([`Self::typed_slept`]), or a legacy predecessor's gesture age carried
    /// conservatively as a possible key; `None` when no typing stamp is recorded here or carried
    /// ([`Self::carry_typed`]).
    pub(crate) fn typed_ms_since(&self, now_us: u64) -> Option<u64> {
        self.typed_ms_at(now_us, slept_us())
    }

    /// [`Self::typed_ms_since`] with `slept` the Mac's sleep since boot now ([`slept_us`]): the
    /// awake time since the typing, plus the sleep since where both readings of it are known; a
    /// carried age is the outgoing process's own reading, the sleep before the carry in it.
    pub(crate) fn typed_ms_at(&self, now_us: u64, slept: Option<u64>) -> Option<u64> {
        let asleep = |then: &AtomicU64| match (then.load(Ordering::Relaxed).checked_sub(1), slept) {
            (Some(then), Some(now)) => now.saturating_sub(then) / 1000,
            _ => 0,
        };
        // Acquire: the sleep stamped with the typing ([`Self::stamp_typed`]) is the one read.
        if let Some(at) = self.typed_at.load(Ordering::Acquire).checked_sub(1) {
            return Some(
                (now_us.saturating_sub(at) / 1000).saturating_add(asleep(&self.typed_slept)),
            );
        }
        let age = self
            .typed_carried_ms
            .load(Ordering::Acquire)
            .checked_sub(1)?;
        let at = self.typed_carried_at.load(Ordering::Relaxed);
        Some(
            age.saturating_add(now_us.saturating_sub(at) / 1000)
                .saturating_add(asleep(&self.typed_carried_slept)),
        )
    }

    /// Milliseconds from the person's last typing to `now_us` in awake time, the clock
    /// [`crate::metrics::now_us`] and an [`std::time::Instant`] keep, which stop while the Mac
    /// sleeps: since the typing stamped here, else the carried awake age
    /// ([`Self::carry_typed_awake`]) plus the time since, else, where only an age that may count
    /// sleep was carried (an outgoing build before the awake row, or a legacy gesture kept as a
    /// possible key), the awake age of the last gesture the outgoing process carried
    /// ([`Self::carried_ms_since`]) where it is sooner, which no typing before the update can be
    /// later than; a gesture made here before any typing here is no typing, so it never shortens
    /// that age. `None` with no typing stamp.
    pub(crate) fn typed_awake_ms(&self, now_us: u64) -> Option<u64> {
        if let Some(at) = self.typed_at.load(Ordering::Acquire).checked_sub(1) {
            return Some(now_us.saturating_sub(at) / 1000);
        }
        if let Some(age) = self
            .typed_awake_carried_ms
            .load(Ordering::Acquire)
            .checked_sub(1)
        {
            let at = self.typed_awake_carried_at.load(Ordering::Relaxed);
            return Some(age.saturating_add(now_us.saturating_sub(at) / 1000));
        }
        let carried = self.typed_ms_at(now_us, None)?;
        Some(
            self.carried_ms_since(now_us)
                .map_or(carried, |gesture| gesture.min(carried)),
        )
    }

    /// THE OUTGOING PROCESS'S TYPING AGE IN AWAKE TIME, CARRIED: `age_ms` as of `now_us` on the
    /// adopting clock ([`Self::typed_awake_ms`]).
    #[cfg(any(unix, test))]
    pub(crate) fn carry_typed_awake(&self, age_ms: u64, now_us: u64) {
        self.typed_awake_carried_at.store(now_us, Ordering::Relaxed);
        self.typed_awake_carried_ms
            .store(age_ms.saturating_add(1), Ordering::Release);
    }

    /// The person typed at `now_us`, `slept` the Mac's sleep since boot then ([`slept_us`]).
    pub(crate) fn stamp_typed(&self, now_us: u64, slept: Option<u64>) {
        self.typed_slept.store(
            slept.map_or(0, |us| us.saturating_add(1)),
            Ordering::Relaxed,
        );
        self.typed_at
            .store(now_us.saturating_add(1), Ordering::Release);
    }

    /// The typing wire value: the milliseconds, or `-` for no stamp (`status human_typed_ms=`).
    pub(crate) fn typed_wire(&self, now_us: u64) -> String {
        self.typed_ms_since(now_us)
            .map_or_else(|| "-".to_string(), |ms| ms.to_string())
    }

    /// THE OUTGOING PROCESS'S TYPING STAMP, CARRIED: its typing age or a legacy gesture's age
    /// kept as a possible key, with `age_ms` carried as of `now_us` on the adopting clock.
    #[cfg(unix)]
    pub(crate) fn carry_typed(&self, age_ms: u64, now_us: u64) {
        self.carry_typed_at(age_ms, now_us, slept_us());
    }

    /// [`Self::carry_typed`], `slept` the Mac's sleep since boot at `now_us`.
    #[cfg(any(unix, test))]
    pub(crate) fn carry_typed_at(&self, age_ms: u64, now_us: u64, slept: Option<u64>) {
        self.typed_carried_slept.store(
            slept.map_or(0, |us| us.saturating_add(1)),
            Ordering::Relaxed,
        );
        self.typed_carried_at.store(now_us, Ordering::Relaxed);
        self.typed_carried_ms
            .store(age_ms.saturating_add(1), Ordering::Release);
    }

    /// THE OUTGOING PROCESS'S STAMP, CARRIED: a person last gave the session input `age_ms` before
    /// `now_us`, in the instance a seamless update adopted it from.
    #[cfg(any(unix, test))]
    pub(crate) fn carry(&self, age_ms: u64, now_us: u64) {
        self.carried_at.store(now_us, Ordering::Relaxed);
        self.carried_ms
            .store(age_ms.saturating_add(1), Ordering::Release);
    }

    /// THE HOST HAS SERVED THE SESSION WITH NO PERSON'S GESTURE: `age_ms` before `now_us` it began,
    /// here or, carried, in the instance a seamless update adopted it from.
    #[cfg(any(unix, test))]
    pub(crate) fn serve_from(&self, age_ms: u64, now_us: u64) {
        let now = i64::try_from(now_us).unwrap_or(i64::MAX);
        let age = i64::try_from(age_ms.saturating_mul(1000)).unwrap_or(i64::MAX);
        self.served_from_us
            .store(now.saturating_sub(age), Ordering::Relaxed);
    }

    /// How long, in milliseconds to `now_us`, the host has served a session no person ever gave input ([`Self::serve_from`]); `None` once one has ([`Self::ms_since`] is `Some`).
    pub(crate) fn unkeyed_ms(&self, now_us: u64) -> Option<u64> {
        if self.ms_since(now_us).is_some() {
            return None;
        }
        let now = i64::try_from(now_us).unwrap_or(i64::MAX);
        let from = self.served_from_us.load(Ordering::Relaxed);
        Some(u64::try_from(now.saturating_sub(from)).unwrap_or(0) / 1000)
    }

    /// [`Self::ms_since`] counted THROUGH SLEEP, as the survey reports it: a laptop that slept since the person's last gesture reads the time that passed.
    pub(crate) fn age_ms(&self, now_us: u64) -> Option<u64> {
        let since = self.ms_since(now_us)?;
        let at = self.at_continuous.load(Ordering::Relaxed).checked_sub(1);
        Some(match (at, aterm_uds::process::continuous_ms()) {
            (Some(at), Some(now)) => now.saturating_sub(at),
            _ => since,
        })
    }

    /// The wire value: the milliseconds, or `-` for never (`status human_ms=`).
    pub(crate) fn wire(&self, now_us: u64) -> String {
        self.ms_since(now_us)
            .map_or_else(|| "-".to_string(), |ms| ms.to_string())
    }
}

/// The Mac's sleep since boot, in microseconds (`aterm_uds::process::asleep_since_boot`): what
/// [`crate::metrics::now_us`], which stops while the Mac sleeps, leaves out.
fn slept_us() -> Option<u64> {
    aterm_uds::process::asleep_since_boot()
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

#[cfg(unix)]
impl crate::App {
    /// THE OUTGOING PROCESS'S STAMPS, WRITTEN RIGHT BEFORE ITS COMMIT'S ONE WRITE: each carried
    /// session's age of its last person's gesture, in the attempt `nonce`'s person-stamps sidecar
    /// ([`crate::seamless::person_stamps_path`]), after the drain gate's last dispatch of input — a
    /// key the overlap passed through to a shell is in it.
    pub(crate) fn write_person_stamps_at_commit(&self, nonce: &str) -> Option<std::path::PathBuf> {
        let path =
            crate::seamless::person_stamps_path(&crate::seamless::outgoing_manifest_path(nonce)?);
        let live: Vec<u64> = self
            .pending_update_handoff
            .as_ref()?
            .live
            .iter()
            .map(|&(id, _, _)| id)
            .collect();
        let written = self.write_person_stamps_of(&live, path);
        if written.is_some() {
            // The keep seam's second copy (inert in a shipped build), of what the stamps added:
            // after the grant, inside the Commit window, before the Commit that lets the
            // successor read the stamps.
            crate::seamless::keep_attempt(nonce);
        }
        written
    }

    fn write_person_stamps_of(
        &self,
        ids: &[u64],
        path: std::path::PathBuf,
    ) -> Option<std::path::PathBuf> {
        use crate::seamless::PersonStamp;
        let now_us = crate::metrics::now_us();
        let now = std::time::Instant::now();
        let job_seen = &self.worker_job_seen;
        let stamps: Vec<(u64, PersonStamp)> = ids
            .iter()
            .filter_map(|&id| Some((id, &self.pool.get(id)?.ctx.human_input)))
            .flat_map(|(id, stamp)| {
                let seen = job_seen.get(&id).map(|&seen| {
                    PersonStamp::JobSeen(
                        u64::try_from(now.saturating_duration_since(seen).as_millis())
                            .unwrap_or(u64::MAX),
                    )
                });
                let carried = match stamp.ms_since(now_us) {
                    Some(age) => Some(PersonStamp::Keyed(age)),
                    None => stamp.unkeyed_ms(now_us).map(PersonStamp::Unkeyed),
                };
                let typed = stamp
                    .typed_ms_since(now_us)
                    .map_or(PersonStamp::Untyped, PersonStamp::Typed);
                let awake = stamp.typed_awake_ms(now_us).map(PersonStamp::TypedAwake);
                carried
                    .into_iter()
                    .chain(std::iter::once(typed))
                    .chain(awake)
                    .chain(seen)
                    .map(move |carried| (id, carried))
            })
            .collect();
        if stamps.is_empty() {
            return None;
        }
        if crate::seamless::write_person_stamps(&path, &stamps) {
            Some(path)
        } else {
            aterm_log::warn!(
                "update apply: the person's stamps could not be written for the successor; it \
                 reads the {} handed session(s) as never keyed since its Commit",
                stamps.len()
            );
            None
        }
    }

    /// THE SUCCESSOR TAKES THE OUTGOING PROCESS'S STAMPS AT ITS COMMIT: every adopted shell starts
    /// serving here, at the Commit — the outgoing process served it until now, and only its own row
    /// says how — then each one the sidecar names by its outgoing id carries its keyed age
    /// ([`HumanInputStamp::carry`]) or how long it was served unkeyed
    /// ([`HumanInputStamp::serve_from`]). Each modern writer says whether typing is recorded;
    /// without that provenance, a legacy keyed age conservatively counts as a possible key. The
    /// reaper's last sight of someone else's program in a concluded worker's tab crosses too
    /// (`App::worker_job_seen`), so the tab's reading time after that program is not cut short.
    pub(crate) fn take_carried_person_stamps(&mut self) {
        let now_us = crate::metrics::now_us();
        for session in self.pool.iter() {
            if session.handoff_local_id.is_some() {
                session.ctx.human_input.serve_from(0, now_us);
            }
        }
        let Some(path) = self.handoff_person_stamps.take() else {
            return;
        };
        let Some(dir) = crate::control_auth::socket_dir() else {
            return;
        };
        let stamps = crate::seamless::take_person_stamps(&path, &dir);
        let typing_known: std::collections::BTreeSet<_> = stamps
            .iter()
            .filter(|(_, stamp)| {
                matches!(
                    stamp,
                    crate::seamless::PersonStamp::Typed(_)
                        | crate::seamless::PersonStamp::TypedAwake(_)
                        | crate::seamless::PersonStamp::Untyped
                )
            })
            .map(|(named, _)| *named)
            .collect();
        let mut carried = 0_usize;
        let mut job_seen = Vec::new();
        for (named, stamp) in stamps {
            if let Some(session) = self
                .pool
                .iter()
                .find(|session| session.handoff_local_id == Some(named))
            {
                match stamp {
                    crate::seamless::PersonStamp::Keyed(age_ms) => {
                        session.ctx.human_input.carry(age_ms, now_us);
                        if !typing_known.contains(&named) {
                            session.ctx.human_input.carry_typed(age_ms, now_us);
                        }
                        carried += 1;
                    }
                    crate::seamless::PersonStamp::Unkeyed(age_ms) => {
                        session.ctx.human_input.serve_from(age_ms, now_us);
                        carried += 1;
                    }
                    crate::seamless::PersonStamp::Typed(age_ms) => {
                        session.ctx.human_input.carry_typed(age_ms, now_us);
                    }
                    crate::seamless::PersonStamp::TypedAwake(age_ms) => {
                        session.ctx.human_input.carry_typed_awake(age_ms, now_us);
                    }
                    crate::seamless::PersonStamp::Untyped => {}
                    crate::seamless::PersonStamp::JobSeen(age_ms) => job_seen.push((named, age_ms)),
                }
            }
        }
        let now = std::time::Instant::now();
        for (named, age_ms) in job_seen {
            let adopted = self
                .pool
                .sessions
                .iter()
                .find(|(_, pooled)| pooled.session.handoff_local_id == Some(named))
                .map(|(&id, _)| id);
            if let Some(adopted) = adopted {
                let seen = now
                    .checked_sub(std::time::Duration::from_millis(age_ms))
                    .unwrap_or(now);
                self.worker_job_seen.insert(adopted, seen);
            }
        }
        if carried > 0 {
            aterm_log::info!(
                "update apply: carried the person's stamps of {carried} adopted session(s)"
            );
        }
    }
}

/// Whether a person's gesture `ev` ([`is_person_gesture`]) reaches the program, as the session's lock-free mode mirror says before the seam routes it: a key press or repeat that encodes to bytes (under the mirror's keyboard mode), text that encodes to bytes (the seam's own [`crate::keymap::encode_committed_text`]: under kitty's report-all-keys mode a control character encodes to nothing), a key sequence, a paste that writes bytes (framed by its own answer or, for one that reads the live mode, the mirror's DEC 2004, then [`aterm_core::terminal::Terminal::paste_sends_bytes`], the formatter's own rule: bracketed always, unbracketed when a character survives the sanitizer); a wheel the seam sends the program bytes for (its route, [`crate::input::wheel_route_mirrored`], then the seam's own rule on that route: a report under a tracking mode that reports wheels, [`aterm_core::terminal::ModeMirror::reports_wheel`], so not X10; or alternate scroll's arrows, [`crate::input::alt_scroll_arrow`], so only a vertical wheel, whose arrow encodes to bytes, on a platform that scrolls a detent some lines, [`crate::input::alt_scroll_sends`] on `platform`, the wheel rule the seam scrolls by: [`crate::input::WheelPlatform`]), a mouse press under mouse tracking, and a drag the tracking mode reports (button-event or any-event tracking, [`aterm_core::terminal::ModeMirror::reports_motion`], the encoder's own rule: X10 and normal tracking report no motion).
pub(crate) fn reaches_program(
    ev: &InputEvent,
    mirror: &aterm_core::terminal::ModeMirror,
    platform: crate::input::WheelPlatform,
) -> bool {
    match ev {
        InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text,
            event_type,
        } => {
            !matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
                && !aterm_types::keyboard::encode_key_with_text(
                    key,
                    *mods,
                    mirror.keyboard_mode(),
                    *event_type,
                    *base_layout,
                    *layout_text,
                )
                .is_empty()
        }
        InputEvent::Text(t) => {
            !crate::keymap::encode_committed_text(t, mirror.keyboard_mode()).is_empty()
        }
        InputEvent::KeySequence(b) => !b.is_empty(),
        InputEvent::Paste(text, framing) => aterm_core::terminal::Terminal::paste_sends_bytes(
            text,
            framing.bracketed(mirror.bracketed_paste()),
        ),
        InputEvent::Wheel {
            dir,
            lines,
            detent,
            mods,
            ..
        } => match crate::input::wheel_route_mirrored(mirror, *mods) {
            crate::input::WheelRoute::Report => mirror.reports_wheel(),
            crate::input::WheelRoute::AltScroll => {
                crate::input::alt_scroll_arrow(*dir, mirror.keyboard_mode())
                    .is_some_and(|arrow| !arrow.is_empty())
                    && crate::input::alt_scroll_sends(platform, *lines, *detent)
            }
            crate::input::WheelRoute::Viewport => false,
        },
        InputEvent::MouseButton { pressed, .. } => *pressed && mirror.mouse_tracking_enabled(),
        InputEvent::MouseMove { buttons, .. } => *buttons != 3 && mirror.reports_motion(*buttons),
        InputEvent::ScrollView(_)
        | InputEvent::Resize { .. }
        | InputEvent::ResizeWindowPx { .. }
        | InputEvent::Focus(_) => false,
    }
}

/// Whether `ev` is a person's deliberate gesture toward a session: everything but a key or button
/// release, bare pointer motion, a focus change and a geometry change.
pub(crate) const fn is_person_gesture(ev: &InputEvent) -> bool {
    match ev {
        InputEvent::Key { event_type, .. } => {
            !matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
        }
        InputEvent::Text(_)
        | InputEvent::KeySequence(_)
        | InputEvent::Paste(..)
        | InputEvent::Wheel { .. }
        | InputEvent::ScrollView(_) => true,
        InputEvent::MouseButton { pressed, .. } => *pressed,
        InputEvent::MouseMove { buttons, .. } => *buttons != 3,
        InputEvent::Resize { .. } | InputEvent::ResizeWindowPx { .. } | InputEvent::Focus(_) => {
            false
        }
    }
}

impl crate::session_log::KeyClass {
    /// The class of `ev`, a person's gesture or a control verb's event (fix E8): a plain press of
    /// Enter, Escape or a digit `1`-`9` (Caps or Num Lock aside), one character of text, or the
    /// bytes [`Self::of_bytes`] places; anything else is [`Self::Other`]. It lives on the input
    /// side, so the session log takes a key's class and never names an event (census
    /// `slog-inert`).
    pub(crate) fn of_event(ev: &InputEvent) -> Self {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        match ev {
            InputEvent::Key {
                key,
                mods,
                event_type: KeyEventType::Press,
                ..
            } if mods
                .difference(Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK)
                .is_empty() =>
            {
                match key {
                    Key::Named(NamedKey::Enter) => Self::Enter,
                    Key::Named(NamedKey::Escape) => Self::Esc,
                    Key::Character(c) => Self::of_char(*c),
                    _ => Self::Other,
                }
            }
            InputEvent::Text(t) => {
                let mut chars = t.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Self::of_char(c),
                    _ => Self::Other,
                }
            }
            InputEvent::KeySequence(b) => Self::of_bytes(b),
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
#[path = "human_input_tests.rs"]
mod tests;
