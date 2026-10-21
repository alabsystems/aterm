// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When a person last gave a session input: `status human_ms=<ms|->` and `text --json`'s
//! `"human_ms"`, with the count in `human_seq=` and `"human_seq"`.
//!
//! The supervisor presses keys into dialogs a person may be navigating, and neither the screen
//! (an arrow into a dialog echoes nothing) nor the OS-wide HID idle says whether anyone keyed
//! this session. So the App input seam ([`crate::App::input_to_session`]) stamps each
//! [`crate::input::Source::Human`] gesture addressed to a session, before any filter that
//! classifies a key as navigation.
//!
//! What counts is [`is_person_gesture`] (a view scroll too: a supervisor's keys would snap the
//! view and clear the selection), plus an IME composition (`App::on_ime_preedit`). A
//! [`crate::input::Source::Controller`] input (every control verb, the owner's included, and
//! the shift+tabs the Claude Code lights press after a click) is never stamped. The first
//! gesture, and the first after [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one,
//! opens a burst ([`HumanInputStamp::note`]): one `human` timeline row and one
//! `EVENT <local> human` push (`crate::app_input::note_person`).
//!
//! **The burst** ([`HumanInputStamp::note_reaching`]): the first gesture ever, and the
//! first after [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one,
//! opens a burst — the one `human` timeline row and `EVENT <local> human`
//! push per burst (`crate::app_input::note_person`), so a person at work is
//! one row however long they stay. A controller's input
//! ([`crate::input::Source::Controller`] — every control verb, the session
//! owner's included, and the shift+tabs the Claude Code lights press on their
//! own after a person's click) is never stamped: that is the whole point.
//!
//! **What it never does**: gate a PERSON's byte. The seam records `src` for
//! audit and never branches its egress on it (`seam_egress` takes no source);
//! this stamp is such a record, read by the `status` and `text --json` verbs
//! and by a CONTROLLER's own fence: `key|send
//! if-human=<n>` (`crate::control_input::InputFence`) asks, under the terminal
//! lock its write is made under, that ITS OWN write be refused (`OK skipped
//! reason=person`) when the count moved since the read that returned `n`
//! ([`HumanInputStamp::seq`]). The only byte the stamp ever holds back is the
//! controller's that asked for it.
//!
//! **The count** ([`HumanInputStamp::seq`], `status human_seq=`, `text
//! --json`'s `"human_seq"`): how many person gestures have reached the
//! session. A write fenced on it (`key|send if-human=<n>`, checked under the
//! terminal lock with the other fences, `crate::control_input::InputFence`)
//! is refused `OK skipped reason=person` when a person has keyed since the
//! read that returned `n` — the round trip between a supervisor's read and
//! its key that the age alone cannot close (R3b of the question answer's
//! critique, 2026-09-25; built 2026-09-27).
//!
//! **What that fence is not** (the residual, said plainly): atomic with the
//! person's own write. A person's gesture is counted here, on the main thread,
//! BEFORE its bytes are written, but that write does not take the terminal
//! lock (the key encoder reads the lock-free `ModeMirror`; a paste or an
//! ordered write goes through the FIFO writer). So a person's gesture counted
//! after the fence's check and written before the controller's write — inside
//! that one lock hold, microseconds wide — still lands ahead of it. What the
//! fence does close is the round trip it replaces: a controller that read the
//! count on one request and wrote on the next.
//!
//! **The plain-Enter marker** ([`HumanInputStamp::probe`], 2026-10-16): the
//! number of the last gesture that was a plain Enter key press
//! ([`Gesture::PlainEnter`]). Equal to the count, the LAST gesture counted was
//! that Enter — what the launch witness (`crate::launch_witness`) asks at the
//! shell's command-start mark: the key that accepted the command line, with
//! nothing typed after it.
//!
//! Lock-free: one monotonic clock read ([`crate::metrics::now_us`]) and one
//! of the clock that counts through sleep (the survey's age), two relaxed
//! atomic stores and one release add per person gesture — the stamp
//! before the count, so a read of the count (an acquire) and then the age
//! never pairs a counted gesture with an age from before it — one load per
//! read, for a plain Enter one release store more, and for a gesture that
//! reached the program one release `fetch_max` of its number, after the count
//! ([`HumanInputStamp::counts`]).
//!
//! **Across a seamless update** ([`HumanInputStamp::carry`], LC-4 of
//! ATERM-KI-1016-1): the clock is this process's, so the stamp itself cannot
//! cross. The outgoing process writes each session's age at its Commit's
//! write — after the last input it dispatches, so a key typed in the overlap
//! is in it ([`crate::App::write_person_stamps_at_commit`]) — and the
//! successor carries it at its Commit ([`crate::App::take_carried_person_stamps`]).
//! Read there, the age is short by the time between the two (milliseconds):
//! the person's last gesture is dated later than it was, the safe way to be
//! wrong. Before, every adopted session read "never" until a person keyed it
//! again, and a check that asks whether nobody keyed since an instant before
//! the update (a relaunch carried through a hand, `keyed-while-held`) could
//! never pass.
//!
//! **Never, and for how long** ([`HumanInputStamp::unkeyed_ms`], `text
//! --json`'s `"unkeyed_ms"`; the review of LC-4, KR-2): "never" alone says
//! only that nobody keyed the session since the host began serving it, which
//! may be after the instant a check asks about. So the stamp also keeps when
//! this process began serving the session — at its start here, or at a
//! seamless successor's Commit, where every adopted shell starts again — and
//! a read of a never-keyed session says how long that has been. Carried
//! across a seamless update as the keyed age is (a `-<age>` row): the
//! outgoing process served the shell that long with no person's gesture.
//! An adopted shell no row names starts serving at the Commit, after
//! anything a check before the update asks about: it reads as it did,
//! "never", for that check.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use crate::input::InputEvent;

/// When a person last gave this session input — [`crate::metrics::now_us`]
/// plus one (so `0` stays "never"), or `0` when no person ever has since the
/// session started in this process — and how many gestures that has been
/// ([`Self::seq`]). Lives on [`crate::SessionCtx`].
#[derive(Debug)]
pub(crate) struct HumanInputStamp {
    at: AtomicU64,
    count: AtomicU64,
    /// THE NUMBER OF THE LAST GESTURE THAT REACHED THE PROGRAM (session log
    /// phase 5, rulings T1, T2, T39), `0` for none: a key that encodes to
    /// bytes, text, a paste, a wheel the program is sent (a report or
    /// alternate scroll), a mouse press under tracking, a drag the tracking
    /// mode reports ([`reaches_program`]). Only these can dismiss a box or
    /// answer delivered mail; a view scroll or a selection is counted in
    /// [`Self::seq`] alone. A gesture's NUMBER, not a second count: a reader
    /// that sees it sees which gesture it is, so no order of two loads can
    /// pair a gesture's class with a count from before it ([`Self::counts`]).
    reached: AtomicU64,
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
    /// ([`Self::carry`]): read only while no gesture of this process is
    /// stamped (`at` is `0`), every one of which is later.
    carried_ms: AtomicU64,
    /// When the carried age was taken here ([`crate::metrics::now_us`]).
    carried_at: AtomicU64,
    /// WHEN THE HOST BEGAN SERVING THE SESSION, on this process's clock
    /// ([`crate::metrics::now_us`], microseconds): its start here, or the
    /// Commit a seamless successor adopted it at — earlier than this
    /// process's own start (negative) where the outgoing process said how
    /// long it served the shell with no person's gesture ([`Self::serve_from`]).
    /// One atomic, so a read never pairs one instant with another's age.
    served_from_us: AtomicI64,
}

impl Default for HumanInputStamp {
    /// A session the host begins serving now: nobody keyed it here yet.
    fn default() -> Self {
        Self {
            at: AtomicU64::new(0),
            count: AtomicU64::new(0),
            reached: AtomicU64::new(0),
            at_continuous: AtomicU64::new(0),
            last_plain_enter: AtomicU64::new(0),
            carried_ms: AtomicU64::new(0),
            carried_at: AtomicU64::new(0),
            served_from_us: AtomicI64::new(
                i64::try_from(crate::metrics::now_us()).unwrap_or(i64::MAX),
            ),
        }
    }
}

/// What kind of gesture [`HumanInputStamp::note_reaching`] counts, for the
/// plain-Enter marker. Passed down from the seam that saw the event
/// (`crate::app_input::note_person`), never guessed from bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gesture {
    /// A press of the named Enter key with no modifier held: not a repeat,
    /// not keypad Enter, not committed text, a paste or a `[key_sequences]`
    /// chord that carries a CR. A lock key's state (Caps Lock, Num Lock),
    /// which a windowed key press carries in its modifiers, is no modifier
    /// held: it does not change which key accepted the line (as
    /// `crate::app_input`'s plain Enter reads it).
    PlainEnter,
    /// Every other gesture.
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
            _ => Self::Other,
        }
    }
}

/// What [`HumanInputStamp::note_reaching`] counted: whether the gesture
/// opens a burst, and its number (the [`HumanInputStamp::seq`] it made).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Noted {
    /// It opens a burst ([`HumanInputStamp::note_reaching`]).
    pub(crate) burst: bool,
    /// Its number.
    pub(crate) seq: u64,
}

/// The person's count and its last program-bound gesture, read together
/// ([`HumanInputStamp::counts`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PersonCounts {
    /// How many gestures are counted ([`HumanInputStamp::seq`]), never below
    /// `reached`.
    pub(crate) seq: u64,
    /// The number of the last gesture that reached the program; `0` for none.
    pub(crate) reached: u64,
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
    /// [`Self::note_reaching`] for a gesture that reached the program: whether
    /// it opens a burst.
    #[cfg(test)]
    pub(crate) fn note(&self, now_us: u64, gesture: Gesture) -> bool {
        self.note_reaching(now_us, gesture, true).burst
    }

    /// A person's gesture of kind `gesture` reached the session at `now_us`,
    /// named the last that reached the program ([`Self::counts`]) too when it
    /// `reaches` it ([`reaches_program`]): its number, and whether it opens a
    /// burst (the first ever, or the first after
    /// [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one).
    pub(crate) fn note_reaching(&self, now_us: u64, gesture: Gesture, reaches: bool) -> Noted {
        self.note_reaching_split(now_us, gesture, reaches, || ())
    }

    /// [`Self::note_reaching`], running `between` once the gesture is counted
    /// and before its class is published: the one point another thread can
    /// read the two apart, where a test puts a reconciling entry (the
    /// production call runs nothing there).
    pub(crate) fn note_reaching_split(
        &self,
        now_us: u64,
        gesture: Gesture,
        reaches: bool,
        between: impl FnOnce(),
    ) -> Noted {
        // THE STAMP BEFORE THE COUNT, the count's add a Release and
        // [`Self::seq`]'s load an Acquire: a reader that sees this gesture
        // counted sees its stamp too. Counted first (as it was until
        // 2026-09-29), a read landing between the two stores paired the new
        // count with the OLD age — "quiet" — and a key fenced on that count
        // passed the fence the gesture should have moved.
        let before = self.at.swap(now_us.saturating_add(1), Ordering::Relaxed);
        if let Some(ms) = aterm_uds::process::continuous_ms() {
            self.at_continuous
                .store(ms.saturating_add(1), Ordering::Relaxed);
        }
        let n = self.count.fetch_add(1, Ordering::Release).wrapping_add(1);
        between();
        // THE CLASS AFTER THE COUNT, and as the gesture's number: a reader
        // that sees it knows gesture `n` is counted, whatever it read of the
        // count ([`Self::counts`]). Until 2026-10-18 this was a second count,
        // added before the first: a reconciling entry between the two adds
        // read the gesture's class with the count before it, marked `human`
        // as through `n - 1`, and the key's own delivery marked it again.
        // `fetch_max`, so two notes racing never move it back.
        if reaches {
            self.reached.fetch_max(n, Ordering::Release);
        }
        // The marker takes the number THIS add returned, after it: equal to
        // the count only when gesture `n` itself was a plain Enter, however
        // two notes interleave. A probe between the add and this store sees
        // the older marker, and says "not a plain Enter".
        if gesture == Gesture::PlainEnter {
            self.last_plain_enter.store(n, Ordering::Release);
        }
        let burst = before.checked_sub(1).is_none_or(|at| {
            now_us.saturating_sub(at) / 1000 >= crate::session_timeline::HUMAN_BURST_GAP_MS
        });
        Noted { burst, seq: n }
    }

    /// THE PERSON'S COUNTS AS ONE READING (rulings T2, T39): how many
    /// gestures are counted and the number of the last that reached the
    /// program, the count never below that number. The number names its
    /// gesture, so a reading that holds it covers it: a log that delivers
    /// through `seq` has delivered the gesture `reached` names, whichever of
    /// the two loads a note landed between.
    pub(crate) fn counts(&self) -> PersonCounts {
        let reached = self.reached.load(Ordering::Acquire);
        let seq = self.count.load(Ordering::Acquire);
        PersonCounts {
            seq: seq.max(reached),
            reached,
        }
    }

    /// The raw stamp; the tests pair it with [`Self::seq`].
    #[cfg(test)]
    pub(crate) fn last(&self) -> u64 {
        self.at.load(Ordering::Relaxed)
    }

    /// How many person gestures have reached the session in this process
    /// (`human_seq=`): equal to an earlier reading exactly when none has
    /// since — what a write fenced `if-human=<n>` is checked against. Read
    /// it BEFORE the age ([`Self::ms_since`], [`Self::wire`]) whenever the two
    /// go out together: the Acquire here makes every stamp up to the count
    /// read visible to that later load, so the pair never says "quiet" about
    /// a gesture the count already holds ([`Self::note_reaching`]).
    pub(crate) fn seq(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// [`Self::seq`] and whether its last gesture was a plain Enter: the
    /// count first, then the marker, both acquires. Equal, gesture `seq` was
    /// a [`Gesture::PlainEnter`] (the marker only ever holds the number of a
    /// plain Enter); a probe racing a note reads them unequal, never a false
    /// "plain Enter".
    pub(crate) fn probe(&self) -> PersonProbe {
        let seq = self.count.load(Ordering::Acquire);
        let enter = self.last_plain_enter.load(Ordering::Acquire);
        PersonProbe {
            seq,
            plain_enter: seq > 0 && enter == seq,
        }
    }

    /// Milliseconds from the last person's gesture to `now_us`; `None` when
    /// no person has given the session input — in this process or, carried
    /// ([`Self::carry`]), in the one a seamless update adopted it from. A
    /// stamp later than `now_us` (two clock reads racing) reads as `0`.
    pub(crate) fn ms_since(&self, now_us: u64) -> Option<u64> {
        if let Some(at) = self.at.load(Ordering::Relaxed).checked_sub(1) {
            return Some(now_us.saturating_sub(at) / 1000);
        }
        let age = self.carried_ms.load(Ordering::Acquire).checked_sub(1)?;
        let at = self.carried_at.load(Ordering::Relaxed);
        Some(age.saturating_add(now_us.saturating_sub(at) / 1000))
    }

    /// THE OUTGOING PROCESS'S STAMP, CARRIED (module header, "Across a
    /// seamless update"): a person last gave the session input `age_ms`
    /// before `now_us`, in the instance a seamless update adopted it from.
    /// Read by [`Self::ms_since`] until a person keys the session here. The
    /// count ([`Self::seq`]) and the plain-Enter marker are this process's
    /// and stay as they are: a fence on a count read from the outgoing
    /// process is refused here, as a person's key since.
    #[cfg(any(unix, test))]
    pub(crate) fn carry(&self, age_ms: u64, now_us: u64) {
        self.carried_at.store(now_us, Ordering::Relaxed);
        self.carried_ms
            .store(age_ms.saturating_add(1), Ordering::Release);
    }

    /// THE HOST HAS SERVED THE SESSION WITH NO PERSON'S GESTURE (module
    /// header, "Never, and for how long"): `age_ms` before `now_us` it began,
    /// here or, carried, in the instance a seamless update adopted it from.
    /// A successor's Commit restarts every adopted shell at `0`, then each
    /// row the outgoing process wrote sets its own age.
    #[cfg(any(unix, test))]
    pub(crate) fn serve_from(&self, age_ms: u64, now_us: u64) {
        let now = i64::try_from(now_us).unwrap_or(i64::MAX);
        let age = i64::try_from(age_ms.saturating_mul(1000)).unwrap_or(i64::MAX);
        self.served_from_us
            .store(now.saturating_sub(age), Ordering::Relaxed);
    }

    /// How long, in milliseconds to `now_us`, the host has served a session
    /// no person ever gave input ([`Self::serve_from`]); `None` once one has
    /// ([`Self::ms_since`] is `Some`). A start later than `now_us` (two
    /// clock reads racing) reads `0`.
    pub(crate) fn unkeyed_ms(&self, now_us: u64) -> Option<u64> {
        if self.ms_since(now_us).is_some() {
            return None;
        }
        let now = i64::try_from(now_us).unwrap_or(i64::MAX);
        let from = self.served_from_us.load(Ordering::Relaxed);
        Some(u64::try_from(now.saturating_sub(from)).unwrap_or(0) / 1000)
    }

    /// [`Self::ms_since`] counted THROUGH SLEEP, as the survey reports it: a
    /// laptop that slept since the person's last gesture reads the time that
    /// passed. The awake count where the platform has no such clock.
    pub(crate) fn age_ms(&self, now_us: u64) -> Option<u64> {
        let since = self.ms_since(now_us)?;
        let at = self.at_continuous.load(Ordering::Relaxed).checked_sub(1);
        Some(match (at, aterm_uds::process::continuous_ms()) {
            (Some(at), Some(now)) => now.saturating_sub(at),
            _ => since,
        })
    }

    /// The wire value: the milliseconds, or `-` for never
    /// (`status human_ms=`).
    pub(crate) fn wire(&self, now_us: u64) -> String {
        self.ms_since(now_us)
            .map_or_else(|| "-".to_string(), |ms| ms.to_string())
    }
}

#[cfg(unix)]
impl crate::App {
    /// THE OUTGOING PROCESS'S STAMPS, WRITTEN RIGHT BEFORE ITS COMMIT'S ONE
    /// WRITE (module header, "Across a seamless update"): each carried
    /// session's age of its last person's gesture, in the attempt `nonce`'s
    /// person-stamps sidecar ([`crate::seamless::person_stamps_path`]),
    /// after the drain gate's last dispatch of input — a key the overlap
    /// passed through to a shell is in it. The path written, for a Commit
    /// that returns (it failed) to remove; `None` when no carried session
    /// was ever keyed, or the write failed (said), and the successor reads
    /// every session as never keyed.
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
        self.write_person_stamps_of(&live, path)
    }

    /// [`Self::write_person_stamps_at_commit`] for the pool sessions `ids`,
    /// at `path`: each one's keyed age, or how long it was served with no
    /// person's gesture ([`HumanInputStamp::unkeyed_ms`]).
    fn write_person_stamps_of(
        &self,
        ids: &[u64],
        path: std::path::PathBuf,
    ) -> Option<std::path::PathBuf> {
        use crate::seamless::PersonStamp;
        let now_us = crate::metrics::now_us();
        let stamps: Vec<(u64, PersonStamp)> = ids
            .iter()
            .filter_map(|&id| {
                let stamp = &self.pool.get(id)?.ctx.human_input;
                let carried = match stamp.ms_since(now_us) {
                    Some(age) => PersonStamp::Keyed(age),
                    None => PersonStamp::Unkeyed(stamp.unkeyed_ms(now_us)?),
                };
                Some((id, carried))
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

    /// THE SUCCESSOR TAKES THE OUTGOING PROCESS'S STAMPS AT ITS COMMIT
    /// (module header, "Across a seamless update"): every adopted shell
    /// starts serving here, at the Commit — the outgoing process served it
    /// until now, and only its own row says how — then each one the sidecar
    /// names by its outgoing id carries its keyed age
    /// ([`HumanInputStamp::carry`]) or how long it was served unkeyed
    /// ([`HumanInputStamp::serve_from`]). Called first at the Commit, before
    /// this process's supervisor host resumes and carries a relaunch through
    /// a hand that asks whether nobody keyed since. One-shot; a sidecar that
    /// is missing or unreadable carries nothing.
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
        let mut carried = 0_usize;
        for (named, stamp) in stamps {
            if let Some(session) = self
                .pool
                .iter()
                .find(|session| session.handoff_local_id == Some(named))
            {
                match stamp {
                    crate::seamless::PersonStamp::Keyed(age_ms) => {
                        session.ctx.human_input.carry(age_ms, now_us);
                    }
                    crate::seamless::PersonStamp::Unkeyed(age_ms) => {
                        session.ctx.human_input.serve_from(age_ms, now_us);
                    }
                }
                carried += 1;
            }
        }
        if carried > 0 {
            aterm_log::info!(
                "update apply: carried the person's stamps of {carried} adopted session(s)"
            );
        }
    }
}

/// Whether a person's gesture `ev` ([`is_person_gesture`]) reaches the
/// program, as the session's lock-free mode mirror says before the seam
/// routes it: a key press or repeat that encodes to bytes (under the
/// mirror's keyboard mode), text that encodes to bytes (the seam's own
/// [`crate::keymap::encode_committed_text`]: under kitty's report-all-keys
/// mode a control character encodes to nothing), a key sequence, a paste
/// that writes bytes (framed by its own answer or, for one that reads the
/// live mode, the mirror's DEC 2004, then
/// [`aterm_core::terminal::Terminal::paste_sends_bytes`], the formatter's
/// own rule: bracketed always, unbracketed when a character survives the
/// sanitizer); a wheel the seam sends the program bytes for (its route,
/// [`crate::input::wheel_route_mirrored`], then the seam's own rule on that
/// route: a report under a tracking mode that reports wheels,
/// [`aterm_core::terminal::ModeMirror::reports_wheel`], so not X10; or
/// alternate scroll's arrows, [`crate::input::alt_scroll_arrow`], so only a
/// vertical wheel, whose arrow encodes to bytes, on a platform that scrolls
/// a detent some lines, [`crate::input::alt_scroll_sends`] on `platform`,
/// the wheel rule the seam scrolls by: [`crate::input::WheelPlatform`]), a
/// mouse press
/// under mouse tracking, and a drag the tracking mode reports (button-event
/// or any-event tracking,
/// [`aterm_core::terminal::ModeMirror::reports_motion`], the encoder's own
/// rule: X10 and normal tracking report no motion). A wheel or a view
/// scroll that moves the viewport, a wheel the seam swallows (an X10
/// report, a horizontal wheel on alternate scroll), a click or a drag that
/// selects, a drag the mode does not report, an unbracketed paste of
/// controls only and a composition's preedit (no bytes) do not: they can
/// neither dismiss a box (T1) nor answer delivered mail (T39). The mirror
/// trails a batch running now by that batch, as the key encoder's does.
///
/// THE LAW (phase 6, 6A-4; `session_log_attribution_tests.rs`'s conformance
/// table): for every person gesture of every [`InputEvent`] class, on every
/// mode the mirror holds and every platform's wheel rule, this is true
/// exactly where the seam writes the program at least one byte, on a
/// terminal whose state is the mirror's. The one gesture outside it is a
/// reported drag that stays in the cell of the report before it (its press,
/// or the last drag the encoder sent): the encoder's per-cell memo, held
/// under the lock and not in the mirror, sends nothing for it, and this says
/// it reached. That report before it reached the program from the same
/// held button, so the person was already at the program; the table drives
/// the pair through one terminal and pins this treatment.
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

/// Whether `ev` is a person's deliberate gesture toward a session (module
/// header, "What counts"): everything but a key release, bare pointer motion,
/// a focus change and a geometry change.
pub(crate) fn is_person_gesture(ev: &InputEvent) -> bool {
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
        // A drag (a button held: `buttons` 3 is none) selects; a hover reads.
        InputEvent::MouseMove { buttons, .. } => *buttons != 3,
        InputEvent::Resize { .. } | InputEvent::ResizeWindowPx { .. } | InputEvent::Focus(_) => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn key(event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(NamedKey::ArrowDown),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type,
        }
    }

    /// Never until a gesture, then the time since it, whole milliseconds;
    /// a clock read that raced behind the stamp reads 0, never wraps.
    #[test]
    fn the_stamp_reads_never_then_the_time_since() {
        let s = HumanInputStamp::default();
        assert_eq!(s.ms_since(5_000_000), None);
        assert_eq!(s.wire(5_000_000), "-");
        s.note(5_000_000, Gesture::Other);
        assert_eq!(s.ms_since(5_000_000), Some(0));
        assert_eq!(s.ms_since(17_345_678), Some(12_345));
        assert_eq!(s.wire(17_345_678), "12345");
        assert_eq!(s.ms_since(15_000_000), Some(10_000));
        assert_eq!(s.ms_since(4_000_000), Some(0), "a racing read never wraps");
        // A stamp at the clock's origin is still a stamp, not "never".
        s.note(0, Gesture::Other);
        assert_eq!(s.ms_since(2_000), Some(2));
    }

    /// A CARRIED STAMP READS ITS AGE ON, AND A GESTURE HERE TAKES OVER (LC-4
    /// of ATERM-KI-1016-1): carried 90 s old, the session reads 90 s at the
    /// carry and 100 s ten seconds on — never "never" — with its count still
    /// this process's; a gesture here then reads from itself.
    #[test]
    fn a_carried_stamp_reads_its_age_on_until_a_gesture_here() {
        let s = HumanInputStamp::default();
        s.carry(90_000, 5_000_000);
        assert_eq!(s.ms_since(5_000_000), Some(90_000));
        assert_eq!(s.wire(15_000_000), "100000");
        assert_eq!(
            s.ms_since(4_000_000),
            Some(90_000),
            "a racing read never wraps"
        );
        assert_eq!(s.seq(), 0, "the count is this process's");
        s.note(20_000_000, Gesture::Other);
        assert_eq!(s.ms_since(21_000_000), Some(1_000), "the gesture here");
        // Commit replay precedes taking the predecessor's sidecar: neither
        // a keyed nor an unkeyed old age may replace the local gesture.
        s.carry(600_000, 21_000_000);
        s.serve_from(600_000, 21_000_000);
        assert_eq!(s.seq(), 1, "the local gesture stays counted");
        assert_eq!(s.ms_since(21_000_000), Some(1_000), "the local stamp wins");
        assert_eq!(s.unkeyed_ms(21_000_000), None, "keyed here");
        // NEGATIVE CONTROL: nothing carried reads never.
        assert_eq!(HumanInputStamp::default().ms_since(5_000_000), None);
    }

    /// A COUNT NEVER OUTRUNS ITS STAMP: a reader that loads the count and
    /// then the stamp — the order `status` and `text --json` read them in —
    /// while a person's gestures land never sees gesture `n` counted beside
    /// the stamp of an earlier one. A torn pair is a `human_seq` that holds
    /// the gesture beside a `human_ms` that says the person is quiet: the
    /// supervisor keys on it, fenced `if-human=` on that very count, and the
    /// fence passes. Gesture `k` lands at `k` ms, so gesture `n`'s stamp is
    /// `n * 1000 + 1`, and a later one only larger. (Counted before it was
    /// stamped, as until 2026-09-29, the pair tore on a plain interleaving,
    /// whatever the memory model: this reader, spinning beside the writer,
    /// is how it shows.)
    #[test]
    fn a_read_that_sees_a_gesture_counted_sees_its_stamp() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        const GESTURES: u64 = 200_000;
        let stamp = Arc::new(HumanInputStamp::default());
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let stamp = Arc::clone(&stamp);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                for k in 1..=GESTURES {
                    stamp.note(k * 1000, Gesture::Other);
                }
                done.store(true, Ordering::Release);
            })
        };
        let (mut reads, mut torn) = (0_u64, 0_u64);
        while !done.load(Ordering::Acquire) {
            let n = stamp.seq();
            let at = stamp.last();
            if n > 0 && at < n * 1000 + 1 {
                torn += 1;
            }
            reads += 1;
        }
        writer.join().expect("the writer");
        assert_eq!(
            torn, 0,
            "{torn} of {reads} reads paired a counted gesture with an older stamp"
        );
        assert_eq!(stamp.seq(), GESTURES);
        assert_eq!(stamp.last(), GESTURES * 1000 + 1);
    }

    /// The first gesture and the first after
    /// [`crate::session_timeline::HUMAN_BURST_GAP_MS`] of quiet open a burst; gestures inside one
    /// do not, however long it lasts. An unstamped session reads `-`.
    #[test]
    fn a_persons_burst_opens_after_the_gap_without_input() {
        let gap_us = crate::session_timeline::HUMAN_BURST_GAP_MS * 1000;
        let s = HumanInputStamp::default();
        assert_eq!(s.wire(5_000_000), "-");
        assert!(
            s.note(5_000_000, Gesture::Other),
            "the first gesture opens a burst"
        );
        assert!(!s.note(5_400_000, Gesture::Other), "inside the burst");
        assert!(
            !s.note(5_400_000 + gap_us - 1_000, Gesture::Other),
            "a gesture just inside the gap after the last is inside it"
        );
        // A key every three seconds for ten minutes is still one burst.
        let mut t = 5_400_000 + gap_us - 1_000;
        for _ in 0..200 {
            t += 3_000_000;
            assert!(!s.note(t, Gesture::Other), "at {t}");
        }
        assert!(
            s.note(t + gap_us, Gesture::Other),
            "a full gap without input opens the next"
        );
        assert_eq!(s.ms_since(t + gap_us + 101_000), Some(101));
        // The clock's first microsecond is still a stamp, not "never".
        let early = HumanInputStamp::default();
        assert!(early.note(0, Gesture::Other));
        assert!(!early.note(3_000, Gesture::Other));
    }

    /// The App input seam ([`crate::App::input`]) stamps a person's arrow, never a controller's
    /// arrow, a key release or a focus change.
    #[test]
    fn the_seam_stamps_a_persons_arrow_and_never_a_controllers() {
        use crate::input::Source;
        use crate::{App, WindowId};
        let mut app = App::headless_for_test();
        let stamp = |app: &App| {
            app.pool
                .get(0)
                .expect("session 0")
                .ctx
                .human_input
                .ms_since(crate::metrics::now_us())
        };
        assert_eq!(stamp(&app), None, "a fresh session: never");
        let _ = app.input(WindowId(0), key(KeyEventType::Press), Source::Controller);
        assert_eq!(stamp(&app), None, "a control verb's key is no person's");
        let _ = app.input(WindowId(0), key(KeyEventType::Release), Source::Human);
        let _ = app.input(WindowId(0), InputEvent::Focus(true), Source::Human);
        assert_eq!(
            stamp(&app),
            None,
            "a release and a focus change are no gesture"
        );
        let _ = app.input(WindowId(0), key(KeyEventType::Press), Source::Human);
        assert!(
            stamp(&app).is_some_and(|ms| ms < 60_000),
            "the person's arrow stamped the session: {:?}",
            stamp(&app)
        );
    }

    /// THE PERSON'S STAMPS CROSS A SEAMLESS UPDATE (LC-4 of ATERM-KI-1016-1):
    /// the outgoing process writes, for the sessions it hands over, the age
    /// of each one's last person's gesture — a stamp it carried itself from
    /// an earlier update included — and for a session nobody keyed, how long
    /// it served it so (`-<age>`, the review's KR-2); the successor's Commit
    /// carries each onto the shell adopted under the id it names and
    /// consumes the sidecar. The keyed session then reads at least that old,
    /// never "never"; the unkeyed one reads never, served at least as long.
    /// NEGATIVE CONTROLS: a successor whose sidecar is missing reads both
    /// never keyed, served only since its Commit — after anything a check
    /// from before the update asks about.
    #[cfg(unix)]
    #[test]
    fn the_persons_stamps_cross_a_seamless_update_at_its_commit() {
        use crate::App;
        // The control dir is the process's (`XDG_RUNTIME_DIR`), read at the
        // write and again at the take, which refuses a sidecar outside it:
        // a seamless test's scratch moved it between the two, under
        // ENV_LOCK, and the stamps read as never carried (2026-10-18,
        // `(None, Some(0))` for both, beside the seamless module).
        let _env = crate::seamless::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = crate::control_auth::socket_dir().expect("a private control dir");
        let path = dir.join(format!(
            "seamless-{}-{}.person",
            std::process::id(),
            aterm_uds::rand::hex_token::<8>().expect("a token")
        ));
        let mut outgoing = App::headless_for_test();
        let keyed = *outgoing.pool.sessions.keys().max().expect("session 0");
        let _ = outgoing.insert_logical_window(crate::stub_session(1), 24, 80);
        let untouched = *outgoing
            .pool
            .sessions
            .keys()
            .max()
            .expect("a second session");
        assert_ne!(keyed, untouched);
        let now = crate::metrics::now_us();
        // Keyed 90 s ago in an instance before this one: carried here already.
        outgoing
            .pool
            .get(keyed)
            .expect("live")
            .ctx
            .human_input
            .carry(90_000, now);
        // Served 600 s with nobody's key, through the instances before.
        outgoing
            .pool
            .get(untouched)
            .expect("live")
            .ctx
            .human_input
            .serve_from(600_000, now);
        let written = outgoing
            .write_person_stamps_of(&[keyed, untouched], path.clone())
            .expect("written");
        assert_eq!(written, path);
        let body = std::fs::read_to_string(&path).expect("the sidecar");
        let rows: Vec<&str> = body.lines().collect();
        assert!(
            rows.len() == 2
                && rows[0].starts_with(&format!("{keyed} 9"))
                && rows[1].starts_with(&format!("{untouched} -6")),
            "the keyed age and the unkeyed one: {body:?}"
        );

        let read = |carried: Option<std::path::PathBuf>| {
            let mut successor = App::headless_for_test();
            let _ = successor.insert_logical_window(crate::stub_session(1), 24, 80);
            let ids: Vec<u64> = successor.pool.sessions.keys().copied().collect();
            for (&id, named) in ids.iter().zip([keyed, untouched]) {
                let session = successor.pool.sessions.get_mut(&id).expect("live");
                session.session.handoff_local_id = Some(named);
                // Adopted long before the Commit, as a prepare is.
                session
                    .session
                    .ctx
                    .human_input
                    .serve_from(3_600_000, crate::metrics::now_us());
            }
            successor.handoff_person_stamps = carried;
            successor.take_carried_person_stamps();
            let now = crate::metrics::now_us();
            ids.iter()
                .map(|&id| {
                    let stamp = &successor.pool.get(id).expect("live").ctx.human_input;
                    (stamp.ms_since(now), stamp.unkeyed_ms(now))
                })
                .collect::<Vec<_>>()
        };
        let carried = read(Some(path.clone()));
        assert!(
            matches!(
                carried[..],
                [(Some(keyed), None), (None, Some(served))] if keyed >= 90_000 && served >= 600_000
            ),
            "(keyed, untouched): {carried:?}"
        );
        assert!(!path.exists(), "consumed");
        let none = read(Some(path.clone()));
        assert!(
            matches!(
                none[..],
                [(None, Some(a)), (None, Some(b))] if a < 60_000 && b < 60_000
            ),
            "NEGATIVE CONTROL: no sidecar, never keyed and served since the Commit: {none:?}"
        );
    }

    /// NEVER, AND FOR HOW LONG (the review of LC-4, KR-2): a session reads
    /// how long the host has served it with nobody's gesture, from its start
    /// here or the age [`HumanInputStamp::serve_from`] gives it, and nothing
    /// once a person keyed it — here, or carried; a start ahead of the
    /// clock reads `0`, never wraps.
    #[test]
    fn a_never_keyed_session_reads_how_long_it_was_served() {
        let s = HumanInputStamp::default();
        let now = crate::metrics::now_us();
        assert!(
            s.unkeyed_ms(now).is_some_and(|ms| ms < 60_000),
            "since its start"
        );
        s.serve_from(600_000, 5_000_000);
        assert_eq!(s.unkeyed_ms(5_000_000), Some(600_000));
        assert_eq!(s.unkeyed_ms(15_000_000), Some(610_000));
        s.serve_from(0, 5_000_000);
        assert_eq!(
            s.unkeyed_ms(4_000_000),
            Some(0),
            "a racing read never wraps"
        );
        let carried = HumanInputStamp::default();
        carried.carry(90_000, 5_000_000);
        assert_eq!(carried.unkeyed_ms(5_000_000), None, "keyed, carried");
        s.note(6_000_000, Gesture::Other);
        assert_eq!(s.unkeyed_ms(7_000_000), None, "keyed here");
    }

    fn enter(mods: Modifiers, event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods,
            base_layout: None,
            layout_text: None,
            event_type,
        }
    }

    /// THE PLAIN-ENTER MARKER, through the real App input seam
    /// ([`crate::App::input`], the headless App's session 0): a person's
    /// Enter press marks the count's last gesture as a plain Enter. Each
    /// gesture that may carry a CR another way — Shift+Enter, a held Enter's
    /// repeat, keypad Enter, committed text, a paste, a `[key_sequences]`
    /// chord — and a letter is counted after a plain Enter and leaves the
    /// probe saying "not a plain Enter"; a controller's Enter neither counts
    /// nor marks. A lock key's state (Caps Lock, Num Lock: a windowed press
    /// carries it in its modifiers) leaves an Enter plain, and Shift+Enter
    /// not.
    #[test]
    fn only_a_persons_plain_enter_press_marks_the_count() {
        use crate::input::{PasteFraming, Source};
        use crate::{App, WindowId};
        let mut app = App::headless_for_test();
        let probe = |app: &App| app.pool.get(0).expect("session 0").ctx.human_input.probe();
        let at = |seq, plain_enter| PersonProbe { seq, plain_enter };
        let plain = enter(Modifiers::empty(), KeyEventType::Press);
        let letter = InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        assert_eq!(probe(&app), at(0, false), "a fresh session");
        let _ = app.input(WindowId(0), plain.clone(), Source::Controller);
        assert_eq!(probe(&app), at(0, false), "a control verb's Enter");
        let _ = app.input(WindowId(0), plain.clone(), Source::Human);
        assert_eq!(probe(&app), at(1, true), "the person's Enter");
        let mut seq = 1;
        for (what, lock) in [
            ("Caps Lock", Modifiers::CAPS_LOCK),
            ("Num Lock", Modifiers::NUM_LOCK),
        ] {
            let _ = app.input(WindowId(0), letter.clone(), Source::Human);
            let _ = app.input(WindowId(0), enter(lock, KeyEventType::Press), Source::Human);
            seq += 2;
            assert_eq!(probe(&app), at(seq, true), "Enter with {what} on");
        }
        let others = [
            ("Shift+Enter", enter(Modifiers::SHIFT, KeyEventType::Press)),
            (
                "Shift+Enter with Caps Lock on",
                enter(Modifiers::SHIFT | Modifiers::CAPS_LOCK, KeyEventType::Press),
            ),
            ("a repeat", enter(Modifiers::empty(), KeyEventType::Repeat)),
            (
                "keypad Enter",
                InputEvent::Key {
                    key: Key::Named(NamedKey::NumpadEnter),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
            ),
            ("text", InputEvent::Text("\r".into())),
            (
                "a paste",
                InputEvent::Paste("claude\r".into(), PasteFraming::AtDrain),
            ),
            ("a chord", InputEvent::KeySequence(b"\r".to_vec())),
            ("a letter", letter),
        ];
        for (what, ev) in others {
            let _ = app.input(WindowId(0), plain.clone(), Source::Human);
            assert_eq!(probe(&app), at(seq + 1, true), "the Enter before {what}");
            let _ = app.input(WindowId(0), ev, Source::Human);
            seq += 2;
            assert_eq!(probe(&app), at(seq, false), "{what}");
        }
    }

    /// An arrow, a digit, Enter and a paste are a person's gesture — the
    /// keys that navigate a dialog and echo nothing are exactly the ones the
    /// momentum signal missed — and a release, a hover, a focus change and a
    /// resize are not.
    #[test]
    fn navigation_counts_and_releases_hovers_focus_and_geometry_do_not() {
        assert!(is_person_gesture(&key(KeyEventType::Press)));
        assert!(is_person_gesture(&key(KeyEventType::Repeat)));
        assert!(!is_person_gesture(&key(KeyEventType::Release)));
        assert!(is_person_gesture(&InputEvent::Text("1".into())));
        assert!(is_person_gesture(&InputEvent::KeySequence(
            b"\x1b[B".to_vec()
        )));
        assert!(is_person_gesture(&InputEvent::Paste(
            "x".into(),
            crate::input::PasteFraming::AtDrain
        )));
        assert!(!is_person_gesture(&InputEvent::Focus(true)));
        assert!(!is_person_gesture(&InputEvent::Resize {
            rows: 24,
            cols: 80,
            echo_to_window: false
        }));
        assert!(!is_person_gesture(&InputEvent::ResizeWindowPx {
            width: 800,
            height: 600
        }));
    }

    /// THE COUNTS ARE ONE READING (rulings T2, T39): at the one point a note
    /// can be read apart (counted, its class not yet published) the reading
    /// holds the gesture's count and not its class; after the note, both.
    /// A reading never names a program-bound gesture its count does not
    /// hold, whichever load a note lands between: `counts` covers the number
    /// `reached` names (a class loaded after a count that missed it, here
    /// stored by hand). NEGATIVE CONTROL (by scratch revert): the class
    /// published before the count (the order until 2026-10-18) is read
    /// inside the note, and without the cover the reading names a class its
    /// count does not hold.
    #[test]
    fn a_reading_that_sees_a_gesture_reach_the_program_counts_it() {
        let s = HumanInputStamp::default();
        s.note_reaching(1_000, Gesture::Other, false);
        let mut inside = PersonCounts::default();
        let noted = s.note_reaching_split(2_000, Gesture::Other, true, || inside = s.counts());
        assert_eq!(noted.seq, 2);
        assert_eq!(inside, PersonCounts { seq: 2, reached: 0 }, "counted first");
        assert_eq!(s.counts(), PersonCounts { seq: 2, reached: 2 });
        // A class loaded past the count it was read with still is covered.
        s.reached.fetch_max(3, Ordering::Release);
        assert_eq!(s.counts(), PersonCounts { seq: 3, reached: 3 });
    }

    /// A DRAG REACHES THE PROGRAM ONLY WHERE ITS MODE REPORTS MOTION (T1,
    /// T39): under X10 (9) and normal (1000) tracking a held-button motion
    /// sends nothing, so it neither dismisses a box nor answers mail; under
    /// button-event (1002) and any-event (1003) tracking it is reported. The
    /// verdict is the encoder's, mode for mode, and a press reaches under
    /// every tracking mode. NEGATIVE CONTROL (by scratch revert): "tracking
    /// on" alone counts the X10 and normal drags.
    #[test]
    fn a_drag_reaches_the_program_only_where_its_mode_reports_motion() {
        use crate::input::PixelOffset;
        use aterm_core::selection::SelectionSide;
        let drag = InputEvent::MouseMove {
            buttons: 0,
            row: 1,
            col: 4,
            mods: 0,
            side: SelectionSide::Left,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        let press = InputEvent::MouseButton {
            button: aterm_types::mouse::MouseButton::Left,
            pressed: true,
            row: 1,
            col: 4,
            mods: 0,
            click_count: 1,
            side: SelectionSide::Left,
            block: false,
            suppress_copy_on_select: false,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        for (set, motion) in [
            (&b""[..], false),
            (b"\x1b[?9h", false),
            (b"\x1b[?1000h", false),
            (b"\x1b[?1002h", true),
            (b"\x1b[?1003h", true),
        ] {
            let mut term = aterm_core::terminal::Terminal::new(24, 80);
            term.process(set);
            let mirror = term.mode_mirror();
            let mode = String::from_utf8_lossy(set);
            assert_eq!(
                reaches_program(&drag, mirror, crate::input::WheelPlatform::Host),
                motion,
                "a drag under {mode:?}"
            );
            assert_eq!(
                term.encode_mouse_motion(0, 4, 1, 0).is_some(),
                motion,
                "the encoder under {mode:?}"
            );
            assert_eq!(
                reaches_program(&press, mirror, crate::input::WheelPlatform::Host),
                !set.is_empty(),
                "a press under {mode:?}"
            );
        }
    }
}
