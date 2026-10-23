// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AUTOPILOT'S TIMER: Cruise until a time, then Land (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;

use crate::App;
use crate::claude_lights::{Clock, LandAtGlass, Weekday};

/// The longest `+DURATION` (§9.3.5): a day of elapsed time.
pub(crate) const MAX_IN_MINUTES: u32 = 24 * 60;

/// What `until=<when>` names, before it is resolved against now (§9.3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum When {
    /// A local clock time, `HH:MM`: minute of the day.
    Clock(u16),
    /// Elapsed time from now, in minutes (1 to [`MAX_IN_MINUTES`]).
    In(u32),
}

/// Parse `<when>`: `HH:MM` (two digits each, `00:00` to `23:59`), or `+<n>h`, `+<n>m`, `+<n>h<n>m` from 1 minute to 24 hours (`+90m`, `+1h30m`, `+24h`).
#[must_use]
pub(crate) fn parse_when(text: &str) -> Option<When> {
    if let Some(span) = text.strip_prefix('+') {
        let (hours, rest) = match span.split_once('h') {
            Some((h, rest)) => (Some(h), rest),
            None => (None, span),
        };
        let minutes = match rest {
            "" => None,
            m => Some(m.strip_suffix('m')?),
        };
        if hours.is_none() && minutes.is_none() {
            return None;
        }
        let number = |s: &str| -> Option<u32> {
            (!s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse().ok())
                .flatten()
        };
        let total = hours.map_or(Some(0), number)? * 60 + minutes.map_or(Some(0), number)?;
        return (1..=MAX_IN_MINUTES)
            .contains(&total)
            .then_some(When::In(total));
    }
    aterm_agent::supervise::LandAt::parse(text)
        .filter(|_| text == text.trim())
        .map(|at| When::Clock(at.minute()))
}

/// A proleptic Gregorian civil date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Date {
    pub(crate) year: i64,
    pub(crate) month: u8,
    pub(crate) day: u8,
}

impl Date {
    /// The date `days` days after 1970-01-01 (Howard Hinnant's `civil_from_days`).
    #[must_use]
    pub(crate) fn of_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month: u8::try_from(month).unwrap_or(1),
            day: u8::try_from(day).unwrap_or(1),
        }
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// The local day number (days since 1970-01-01, local) of unix second `at`
/// under offset `offset_s`.
const fn local_days(at: i64, offset_s: i64) -> i64 {
    (at + offset_s).div_euclid(86_400)
}

/// Why `until=` names no instant (§9.3.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UntilRefused {
    /// Not `HH:MM` or a duration in range: the usage line alone.
    Malformed,
    /// `HH:MM` does not exist on `date`, the day it would fall: the clocks go forward over it.
    Skipped { clock: String, date: Date },
    /// The local zone cannot be read here, so no `HH:MM` can be placed.
    NoZone,
}

impl UntilRefused {
    /// The reason an `ERR` names before the usage line, or `None` for the usage line alone.
    #[must_use]
    pub(crate) fn reason(&self) -> Option<String> {
        match self {
            Self::Malformed => None,
            Self::Skipped { clock, date } => Some(format!(
                "{clock} does not exist on {date} (the clocks go forward)"
            )),
            Self::NoZone => {
                Some("the local time zone cannot be read here; give until=+DURATION".to_string())
            }
        }
    }
}

/// An instant `until=` resolved to: unix ms, and the zone's offset there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// When the timed Cruise lands, unix ms (whole seconds).
    pub(crate) at_ms: i64,
    /// The local zone's offset from UTC at that instant, in seconds, where it could be read.
    pub(crate) offset_s: Option<i64>,
}

impl Resolved {
    /// The reply's tail (§9.3.5): `land_at=<unix s> local=<date>T<HH:MM><±HH:MM>` (`local=-` where the zone cannot be read), so a time that fell on the next day is never a surprise.
    #[must_use]
    pub(crate) fn reply(self) -> String {
        format!(
            "land_at={} local={}",
            self.at_ms.div_euclid(1000),
            local_stamp(self.at_ms, self.offset_s)
        )
    }
}

/// `2026-10-17T16:00+01:00`: the local reading of unix ms `at_ms` at `offset_s`, or `-` without one.
#[must_use]
pub(crate) fn local_stamp(at_ms: i64, offset_s: Option<i64>) -> String {
    let Some(offset) = offset_s else {
        return "-".to_string();
    };
    let at = at_ms.div_euclid(1000);
    let date = Date::of_days(local_days(at, offset));
    let minute = (at + offset).rem_euclid(86_400) / 60;
    let sign = if offset < 0 { '-' } else { '+' };
    let off = offset.unsigned_abs() / 60;
    format!(
        "{date}T{:02}:{:02}{sign}{:02}:{:02}",
        minute / 60,
        minute % 60,
        off / 60,
        off % 60
    )
}

/// Resolve `when` against `now_ms` (unix ms) in the zone `offset_at` reads
/// (unix second → its offset from UTC in seconds; `presence::local_offset_at`
/// in the window, a fixed or a DST zone in a test). §9.3.5's rules:
///
/// * `In(m)`: `m` minutes of elapsed time from now, from the next whole
///   second, so the instant is never short of what was asked;
/// * `Clock(hh:mm)`: the earliest instant STRICTLY after now whose local
///   reading is `hh:mm`, looked for on today's local date, then tomorrow's.
///   A fall-back night that reads it twice gives the earlier reading still
///   ahead. A reading a spring-forward skips refuses with the date, but only
///   on the day it would fall: a skipped reading already behind now is no
///   refusal, and tomorrow's is taken.
pub(crate) fn resolve(
    when: When,
    now_ms: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> Result<Resolved, UntilRefused> {
    let now_s = now_ms.div_euclid(1000) + i64::from(now_ms.rem_euclid(1000) != 0);
    let minute = match when {
        When::In(minutes) => {
            let at = now_s + i64::from(minutes) * 60;
            return Ok(Resolved {
                at_ms: at * 1000,
                offset_s: offset_at(at),
            });
        }
        When::Clock(minute) => i64::from(minute),
    };
    let today = local_days(
        now_ms.div_euclid(1000),
        offset_at(now_ms.div_euclid(1000)).ok_or(UntilRefused::NoZone)?,
    );
    for day in [today, today + 1] {
        let local = day * 86_400 + minute * 60;
        let around = [local - 86_400, local, local + 86_400];
        let mut readings: Vec<(i64, i64)> = Vec::with_capacity(3);
        for probe in around {
            let Some(offset) = offset_at(probe) else {
                return Err(UntilRefused::NoZone);
            };
            let at = local - offset;
            if offset_at(at) == Some(offset) && !readings.iter().any(|(r, _)| *r == at) {
                readings.push((at, offset));
            }
        }
        readings.sort_unstable();
        if readings.is_empty() {
            let before = offset_at(local - 86_400).ok_or(UntilRefused::NoZone)?;
            if (local - before) * 1000 > now_ms {
                return Err(UntilRefused::Skipped {
                    clock: format!("{:02}:{:02}", minute / 60, minute % 60),
                    date: Date::of_days(day),
                });
            }
            continue;
        }
        if let Some(&(at, offset)) = readings.iter().find(|(at, _)| at * 1000 > now_ms) {
            return Ok(Resolved {
                at_ms: at * 1000,
                offset_s: Some(offset),
            });
        }
    }
    Err(UntilRefused::Malformed)
}

/// The face's time for an instant (§9.3.2): its local `HH:MM` and, where it falls on another local date than `now_ms`'s, its weekday.
#[must_use]
pub(crate) fn glass_of(
    at_ms: i64,
    now_ms: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<LandAtGlass> {
    let at = at_ms.div_euclid(1000);
    let now = now_ms.div_euclid(1000);
    let (at_offset, now_offset) = (offset_at(at)?, offset_at(now)?);
    let minute = (at + at_offset).rem_euclid(86_400) / 60;
    let day = local_days(at, at_offset);
    Some(LandAtGlass {
        clock: Clock::of_minute(u16::try_from(minute).unwrap_or(0)),
        other_day: (day != local_days(now, now_offset)).then(|| Weekday::of_days(day)),
    })
}

/// THE OFFER'S WORDS (§9.3.3, §9.3.4): the face of the instant `[harness] land_at` (`at`) arms when chosen at `now_ms` ([`resolve`]: today, or tomorrow once today's reading has passed) — its clock and, where that instant is not today, its weekday — so the menu's `Cruise until Sat 18:36, then land` and the page's `at Sat 18:36` say what the state line will say once it is armed (`lands Sat 18:36`).
#[must_use]
pub(crate) fn offer_glass(
    at: aterm_agent::supervise::LandAt,
    now_ms: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> LandAtGlass {
    resolve(When::Clock(at.minute()), now_ms, offset_at)
        .ok()
        .and_then(|resolved| glass_of(resolved.at_ms, now_ms, offset_at))
        .unwrap_or(LandAtGlass {
            clock: Clock::of_minute(at.minute()),
            other_day: None,
        })
}

/// WHEN THE OFFER'S WORDS TURN (§9.3.3): the first wall instant after `now_ms` at which [`offer_glass`] reads differently — the instant the offer arms now (at it, the next one is tomorrow's, which names its day), or the next local midnight (past it, tomorrow is today, and the day goes), whichever is sooner.
#[must_use]
pub(crate) fn offer_turns_at(
    at: aterm_agent::supervise::LandAt,
    now_ms: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<i64> {
    [
        resolve(When::Clock(at.minute()), now_ms, offset_at)
            .ok()
            .map(|resolved| resolved.at_ms),
        day_words_turn_at(now_ms, offset_at),
    ]
    .into_iter()
    .flatten()
    .min()
}

/// When the next local date starts: the day words an open menu or page
/// shows need refreshing there, whether or not an exact 00:00 exists.
fn day_words_turn_at(now_ms: i64, offset_at: &dyn Fn(i64) -> Option<i64>) -> Option<i64> {
    const DAY_S: i64 = 86_400;
    let offset = offset_at(now_ms.div_euclid(1000))?;
    let local = now_ms.div_euclid(1000).checked_add(offset)?;
    let noon = local
        .div_euclid(DAY_S)
        .checked_add(1)?
        .checked_mul(DAY_S)?
        .checked_add(DAY_S / 2)?;
    let probe_s = noon.checked_sub(offset)?;
    offset_at(probe_s)?;
    let turn = crate::harness_screen::local_midnight_ms(probe_s.checked_mul(1000)?, offset_at);
    (turn > now_ms).then_some(turn)
}

/// The wall clock, unix ms: read once per event that needs it (an arm, a fire, a moved wall), never per frame.
#[must_use]
pub(crate) fn wall_ms() -> i64 {
    #[cfg(test)]
    if let Some((_, wall)) = FAKE_CLOCKS.get() {
        return wall;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The monotonic clock the index derives its dues on.
#[must_use]
fn mono_now() -> Instant {
    #[cfg(test)]
    if let Some((now, _)) = FAKE_CLOCKS.get() {
        return now;
    }
    Instant::now()
}

#[cfg(test)]
thread_local! {
    /// A test's two clocks, on its own thread: `(monotonic, wall unix ms)`.
    static FAKE_CLOCKS: std::cell::Cell<Option<(Instant, i64)>> =
        const { std::cell::Cell::new(None) };
}

/// Pin this test thread's clocks (`None`: the real ones again): the index, the fire and the wire's `until=` read them.
#[cfg(test)]
pub(crate) fn set_fake_clocks(clocks: Option<(Instant, i64)>) {
    FAKE_CLOCKS.set(clocks);
}

/// The local zone the window reads: the C library's rules for this process's zone at each instant (`presence::local_offset_at`).
pub(crate) fn local_offset_at(unix: i64) -> Option<i64> {
    crate::presence::local_offset_at(unix)
}

/// One armed timer in the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimerEntry {
    /// The stored instant it was indexed from, unix ms: what the fire compares against the store.
    pub(crate) land_at_ms: i64,
    /// When to look, on the monotonic clock: derived from the wall when indexed or re-derived ([`AutopilotTimers::rederive`]).
    pub(crate) due: Instant,
    /// The face's time, formatted once ([`glass_of`]); `None` where the zone cannot be read.
    pub(crate) glass: Option<LandAtGlass>,
    /// Indexed by a restore with its instant already passed: what was closed while it passed, which the record names (§9.3.7).
    pub(crate) passed_while: Option<ClosedOver>,
}

/// What was closed over the gap a restore path indexed a timer across — and so, where its instant passed meanwhile, what the fire's record says it passed while (§9.3.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosedOver {
    /// aterm itself: a cold restore, a crash's, or a seamless update's carry — "passed while aterm was closed".
    Aterm,
    /// Only the tab, with aterm running (a reopened closed tab): "passed while the tab was closed".
    Tab,
}

/// The App's index of armed timers, by session local id (§9.3.6, item 3).
#[derive(Debug, Default)]
pub(crate) struct AutopilotTimers {
    entries: HashMap<u64, TimerEntry>,
    /// A seamless update's successor rebuilt the index after its Commit
    /// ([`App::settle_autopilot_commit`]): owed once, then never again.
    committed: bool,
    /// When an open Autopilot menu's or Settings ▸ Harness page's `land_at`
    /// offer turns its words ([`offer_turns_at`]): the wall instant, unix ms,
    /// and the monotonic `due` derived from it, noted where the offer is
    /// built ([`App::note_land_at_offer`]); `None` with nothing offered.
    offer_due: Option<(i64, Instant)>,
}

impl AutopilotTimers {
    /// Whether no timer is armed: the frame then looks nothing up here (`App::chip_glass`).
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Session `local_id`'s armed timer as the chip shows it, or `None`.
    #[must_use]
    pub(crate) fn glass(&self, local_id: u64) -> Option<LandAtGlass> {
        self.entries.get(&local_id).and_then(|entry| entry.glass)
    }

    /// Session `local_id`'s entry.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn entry(&self, local_id: u64) -> Option<&TimerEntry> {
        self.entries.get(&local_id)
    }

    /// The instant session `local_id`'s armed timer was indexed at, unix ms: what a cancel offered beside its face names ([`App::cancel_autopilot_timer`]).
    #[must_use]
    pub(crate) fn land_at_ms(&self, local_id: u64) -> Option<i64> {
        self.entries.get(&local_id).map(|entry| entry.land_at_ms)
    }

    /// Every indexed session, in id order (introspection and tests).
    #[must_use]
    pub(crate) fn ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.entries.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Session `local_id` is retired, or holds no timer any more: its entry goes, so no past instant is folded again.
    pub(crate) fn forget(&mut self, local_id: u64) -> bool {
        self.entries.remove(&local_id).is_some()
    }

    /// The noted turn of an open offer's words (tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn offer_due(&self) -> Option<(i64, Instant)> {
        self.offer_due
    }

    /// Every entry goes (the Commit's rebuild starts from nothing).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Index session `local_id` at `land_at_ms`, its `due` derived from the wall (`wall_ms`, read at `now`): an instant already passed is due at once.
    pub(crate) fn arm(
        &mut self,
        local_id: u64,
        land_at_ms: i64,
        now: Instant,
        wall_ms: i64,
        offset_at: &dyn Fn(i64) -> Option<i64>,
        closed: Option<ClosedOver>,
    ) -> bool {
        let before = self
            .entries
            .get(&local_id)
            .copied()
            .filter(|entry| entry.land_at_ms == land_at_ms);
        let entry = TimerEntry {
            land_at_ms,
            due: due_at(land_at_ms, now, wall_ms),
            glass: glass_of(land_at_ms, wall_ms, offset_at),
            passed_while: closed
                .filter(|_| land_at_ms <= wall_ms)
                .or_else(|| before.and_then(|entry| entry.passed_while)),
        };
        self.entries.insert(local_id, entry);
        before.is_none_or(|before| {
            (before.glass, before.passed_while) != (entry.glass, entry.passed_while)
        })
    }

    /// The earliest `due`, or `None` with nothing armed.
    #[must_use]
    pub(crate) fn earliest(&self) -> Option<Instant> {
        self.entries.values().map(|entry| entry.due).min()
    }

    /// Take every entry due at `now` OUT of the index, in id order: each is looked at once, whatever follows.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<(u64, TimerEntry)> {
        let mut due: Vec<(u64, TimerEntry)> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.due <= now)
            .map(|(id, entry)| (*id, *entry))
            .collect();
        due.sort_unstable_by_key(|(id, _)| *id);
        for (id, _) in &due {
            self.entries.remove(id);
        }
        due
    }

    /// The wall clock moved under the monotonic one (a sleep, a clock set forward, a zone change): every `due`, face time and weekday is derived again from the wall.
    pub(crate) fn rederive(
        &mut self,
        now: Instant,
        wall_ms: i64,
        offset_at: &dyn Fn(i64) -> Option<i64>,
    ) -> Vec<u64> {
        let mut moved = Vec::new();
        for (id, entry) in &mut self.entries {
            entry.due = due_at(entry.land_at_ms, now, wall_ms);
            let glass = glass_of(entry.land_at_ms, wall_ms, offset_at);
            if glass != entry.glass {
                entry.glass = glass;
                moved.push(*id);
            }
        }
        moved.sort_unstable();
        moved
    }
}

/// The monotonic instant the wall reads `land_at_ms` at, from a reading of
/// both clocks (`now`, `wall_ms`): `now` itself where it has passed.
fn due_at(land_at_ms: i64, now: Instant, wall_ms: i64) -> Instant {
    let ahead = u64::try_from(land_at_ms.saturating_sub(wall_ms)).unwrap_or(0);
    now + Duration::from_millis(ahead)
}

/// What a timer choice from the window names (the menu, Settings ▸ Harness: `HarnessTimerRequest`; §9.3.3, §9.3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimerChoice {
    /// Cruise this many minutes, then land.
    In(u32),
    /// Cruise until `[harness] land_at`, then land.
    At,
    /// Cancel the landing: Cruise with no timer.
    Cancel,
}

impl App {
    /// The earliest instant an armed timer is due, or `None` with none armed: then nothing is folded.
    #[must_use]
    pub(crate) fn autopilot_timer_deadline(&self) -> Option<Instant> {
        if self.incoming_handoff_pending {
            return None;
        }
        self.autopilot_timers.earliest()
    }

    /// Index session `local_id`'s timer from its STORED pair (read under its meta guard): armed where the word is `cruise` with a `land_at`, gone otherwise.
    pub(crate) fn index_autopilot_timer(&mut self, local_id: u64, closed: Option<ClosedOver>) {
        if self.index_autopilot_timer_unstamped(local_id, closed) {
            self.restamp_for_hover(local_id);
        }
    }

    /// [`Self::index_autopilot_timer`] for a caller that stamps the strips itself after it (a meta move's `refresh_meta_dependent_chrome`), so a timed write stamps each strip once: whether what the chip shows moved, for the caller to stamp where it does not.
    pub(crate) fn index_autopilot_timer_unstamped(
        &mut self,
        local_id: u64,
        closed: Option<ClosedOver>,
    ) -> bool {
        let stored = self
            .pool
            .get(local_id)
            .and_then(|session| stored_timer(&session.ctx));
        self.index_autopilot_timer_at(local_id, stored, closed)
    }

    /// [`Self::index_autopilot_timer`] for a session not yet registered (a cold restore's respawn, a reopened closed tab): its own context.
    pub(crate) fn index_autopilot_timer_of(
        &mut self,
        local_id: u64,
        ctx: &crate::SessionCtx,
        closed: Option<ClosedOver>,
    ) {
        if self.index_autopilot_timer_at(local_id, stored_timer(ctx), closed) {
            self.restamp_for_hover(local_id);
        }
    }

    /// The index follows `stored`; where what the chip shows moved, the
    /// footer is posted and an open menu built again.
    fn index_autopilot_timer_at(
        &mut self,
        local_id: u64,
        stored: Option<i64>,
        closed: Option<ClosedOver>,
    ) -> bool {
        let moved = match stored {
            Some(land_at_ms) => self.autopilot_timers.arm(
                local_id,
                land_at_ms,
                mono_now(),
                wall_ms(),
                &local_offset_at,
                closed,
            ),
            None => self.autopilot_timers.forget(local_id),
        };
        if moved {
            crate::claude_footer::post_changed(local_id);
            self.refresh_autopilot_menu(local_id);
        }
        moved
    }

    /// THE OFFER IS BUILT (the Autopilot menu's rows, Settings ▸ Harness's publish): note when its words turn ([`offer_turns_at`]), so a menu or a page held open across that minute is built again then ([`Self::expire_autopilot_offer`]) and never offers `18:36` for a press that arms tomorrow's.
    pub(crate) fn note_land_at_offer(&mut self) {
        let land_at = self.harness_policy.get().policy.land_at;
        let (now, wall) = (mono_now(), wall_ms());
        let turn = match land_at {
            Some(at) => offer_turns_at(at, wall, &local_offset_at),
            None => day_words_turn_at(wall, &local_offset_at),
        };
        self.autopilot_timers.offer_due = turn.map(|turn| (turn, due_at(turn, now, wall)));
    }

    /// When the offer an open menu or page shows turns its words, or `None`: with neither open, or nothing offered, nothing is folded.
    #[must_use]
    pub(crate) fn autopilot_offer_deadline(&self) -> Option<Instant> {
        let (_, due) = self.autopilot_timers.offer_due?;
        (self.autopilot_menu.is_some() || self.harness_page_shown()).then_some(due)
    }

    /// The offer's words turned under an open menu or page: build them again (each notes its next turn).
    pub(crate) fn expire_autopilot_offer(&mut self) {
        if self.autopilot_timers.offer_due.is_none() {
            return;
        }
        match self.autopilot_offer_deadline() {
            None => self.autopilot_timers.offer_due = None,
            Some(due) if due <= mono_now() => self.autopilot_offer_turned(),
            Some(_) => {}
        }
    }

    /// The menu's rows and the page built again from the wall as it reads
    /// now (each notes the next turn); the note dropped first, so with
    /// neither open none is left.
    fn autopilot_offer_turned(&mut self) {
        self.autopilot_timers.offer_due = None;
        if let Some(session) = self.autopilot_menu.as_ref().map(|menu| menu.session) {
            self.refresh_autopilot_menu(session);
        }
        if self.harness_page_shown()
            && self
                .harness_screen
                .stats
                .day_ended(wall_ms(), local_offset_at)
        {
            self.start_harness_stats_refresh();
        }
        self.harness_screen_moved();
    }

    /// THE COMMIT'S REBUILD, owed once (§9.3.6, item 3): a seamless update's successor drops `MetaChanged` until its Commit (`Wake::ActivateCommittedHandoff` lifts `incoming_handoff_pending`), while drivers may already write meta, so the first park after the Commit builds the index again from every session's stored pair ([`Self::rebuild_autopilot_timers`]) — run by [`Self::expire_autopilot_timers`] before anything fires, so no path that lifts the gate can skip it.
    pub(crate) fn settle_autopilot_commit(&mut self) {
        if !self.handoff_successor
            || self.incoming_handoff_pending
            || self.autopilot_timers.committed
        {
            return;
        }
        self.autopilot_timers.committed = true;
        self.rebuild_autopilot_timers();
    }

    /// The index built again from every session's stored pair (the Commit's,
    /// [`Self::settle_autopilot_commit`]).
    fn rebuild_autopilot_timers(&mut self) {
        let stored: Vec<(u64, i64)> = self
            .pool
            .iter()
            .filter_map(|session| Some((session.id, stored_timer(&session.ctx)?)))
            .collect();
        let had: Vec<u64> = self.autopilot_timers.ids();
        self.autopilot_timers.clear();
        if !stored.is_empty() {
            let (now, wall) = (mono_now(), wall_ms());
            for (id, land_at_ms) in &stored {
                self.autopilot_timers
                    .arm(*id, *land_at_ms, now, wall, &local_offset_at, None);
            }
        }
        let ids: std::collections::BTreeSet<u64> = had
            .into_iter()
            .chain(stored.iter().map(|(id, _)| *id))
            .collect();
        for id in ids {
            crate::claude_footer::post_changed(id);
            self.refresh_autopilot_menu(id);
            self.restamp_for_hover(id);
        }
    }

    /// THE WALL CLOCK MOVED under the monotonic deadline (`Wake::WallMoved`: the watchdog saw the Mac sleep or the clock set forward), or a window gained focus: every armed timer is derived again from the wall — its due, its face's time and weekday — with one wall read, and none at all while nothing is armed.
    pub(crate) fn on_wall_moved(&mut self) {
        self.harness_ab_wall_moved();
        if let Some((turn, _)) = self.autopilot_timers.offer_due {
            let (now, wall) = (mono_now(), wall_ms());
            if turn <= wall {
                self.autopilot_offer_turned();
            } else {
                self.autopilot_timers.offer_due = Some((turn, due_at(turn, now, wall)));
            }
        }
        if self.autopilot_timers.is_empty() {
            return;
        }
        for id in self
            .autopilot_timers
            .rederive(mono_now(), wall_ms(), &local_offset_at)
        {
            crate::claude_footer::post_changed(id);
            self.refresh_autopilot_menu(id);
        }
    }

    /// Fire every due timer, each a compare-and-set on its session's stored pair (§9.3.6, item 5) — after a successor's Commit, from the index that Commit owes ([`Self::settle_autopilot_commit`]).
    pub(crate) fn expire_autopilot_timers(&mut self) {
        self.settle_autopilot_commit();
        let Some(earliest) = self.autopilot_timer_deadline() else {
            return;
        };
        let now = mono_now();
        if earliest > now {
            return;
        }
        self.expire_autopilot_timers_at(now, wall_ms());
    }

    /// [`Self::expire_autopilot_timers`] on given clocks: `now` decides which entries are due, `wall_ms` what each fire compares against.
    pub(crate) fn expire_autopilot_timers_at(&mut self, now: Instant, wall_ms: i64) {
        if self.incoming_handoff_pending {
            return;
        }
        for (local_id, entry) in self.autopilot_timers.take_due(now) {
            let Some(ctx) = self.pool.get(local_id).map(|s| s.ctx.clone()) else {
                continue;
            };
            match crate::session_timeline::fire_land_at(&ctx, entry.land_at_ms, wall_ms) {
                crate::session_timeline::Fired::Landed { revision } => {
                    self.autopilot_timer_landed(local_id, entry, revision);
                }
                crate::session_timeline::Fired::NotYet(remaining) => {
                    let again = TimerEntry {
                        due: now + Duration::from_millis(u64::try_from(remaining).unwrap_or(0)),
                        ..entry
                    };
                    self.autopilot_timers.entries.insert(local_id, again);
                }
                crate::session_timeline::Fired::Moved(stored) => {
                    if let Some(land_at_ms) = stored {
                        self.autopilot_timers.arm(
                            local_id,
                            land_at_ms,
                            now,
                            wall_ms,
                            &local_offset_at,
                            None,
                        );
                    }
                    crate::claude_footer::post_changed(local_id);
                    self.refresh_autopilot_menu(local_id);
                    self.restamp_for_hover(local_id);
                }
            }
        }
    }

    /// The fan-out after a fire stored `land` (the same as a click's): the
    /// tab chrome and the session's `events` watchers, the footer, the A/B
    /// arm, the page, and the message log's record — which says what the
    /// fire found ([`Self::autopilot_fire_over`]), and who set it: the timer
    /// (§11.7.1, `set at the 18:00 timer`).
    fn autopilot_timer_landed(&mut self, local_id: u64, entry: TimerEntry, revision: u64) {
        self.autopilot_recorded.insert(local_id, revision);
        self.journal_owner_words(local_id);
        self.refresh_meta_dependent_chrome(local_id);
        if let Some(session) = self.pool.get(local_id) {
            session.ctx.watchers.ring_watchers();
        }
        crate::claude_footer::post_changed(local_id);
        self.refresh_autopilot_menu(local_id);
        self.note_harness_arm(local_id);
        self.harness_screen_moved();
        let sid = self
            .pool
            .get(local_id)
            .map(|s| s.ctx.self_id.as_str().to_string());
        if let Some(sid) = sid {
            let over = self.autopilot_fire_over(local_id, &sid);
            let place = match over {
                crate::message_reporters::FireOver::NoAgent { .. } => self.pane_place(&sid).0,
                _ => self.session_place(&sid),
            };
            let clock = entry.glass.as_ref().map(|g| g.clock.as_str());
            let set_by = clock.map_or_else(
                || "set by its timer".to_string(),
                |c| format!("set at the {c} timer"),
            );
            self.record_message(crate::message_reporters::autopilot_timer_ended(
                &sid,
                &place,
                clock,
                entry.passed_while,
                over,
                Some(&set_by),
            ));
        }
    }

    /// WHAT THE FIRE FOUND on session `local_id` (`sid`, §10.2.3 item 8),
    /// for its record: the window's harness off (no host, the master
    /// switch), no agent in the pane (and whether its tab holds other
    /// panes, [`Self::pane_place`]: the line then says `this pane`, since
    /// another may run an agent), the agent's own switch off, else work the
    /// harness lands.
    fn autopilot_fire_over(&self, local_id: u64, sid: &str) -> crate::message_reporters::FireOver {
        use crate::autopilot::HarnessPosture;
        use crate::message_reporters::FireOver;
        let posture = self.harness_posture();
        match posture {
            HarnessPosture::Unhosted => return FireOver::Unhosted,
            HarnessPosture::MasterOff => return FireOver::MasterOff,
            HarnessPosture::On(_)
            | HarnessPosture::Restricted(..)
            | HarnessPosture::AgentOff(_) => {}
        }
        let Some(agent) = self.session_agent(local_id) else {
            return FireOver::NoAgent {
                split: self.pane_place(sid).1,
            };
        };
        if let HarnessPosture::AgentOff(agent) = posture.for_agent(Some(agent)) {
            return FireOver::AgentOff(agent);
        }
        FireOver::Work
    }

    /// The instant a timed choice from the window arms for session `local_id` (§9.3.3, §9.3.4), unix ms, or `None` where it names nothing (`At` with no `[harness] land_at`, a zone that cannot be read, or the cancel, which arms nothing).
    #[must_use]
    pub(crate) fn resolve_timer_choice(&self, choice: TimerChoice) -> Option<i64> {
        let when = match choice {
            TimerChoice::Cancel => return None,
            TimerChoice::In(minutes) => When::In(minutes),
            TimerChoice::At => When::Clock(self.harness_policy.get().policy.land_at?.minute()),
        };
        #[cfg(test)]
        crate::work_counts::timer_resolved();
        resolve(when, wall_ms(), &local_offset_at)
            .ok()
            .map(|resolved| resolved.at_ms)
    }

    /// Settings ▸ Harness's timer press (`AppEffect::HarnessTimer`): the session's choice written ([`Self::choose_autopilot_timer`]) from the page, and the page republished either way so it converges on what is stored.
    pub(crate) fn execute_harness_timer(
        &mut self,
        request: crate::native_app::HarnessTimerRequest,
    ) {
        self.choose_autopilot_timer(
            request.session,
            request.choice,
            crate::autopilot::Surface::Settings,
        );
        self.harness_screen_moved();
    }

    /// A timer choice from the window (the menu, Settings ▸ Harness's disclosure): a timed Cruise written through the window's one writer ([`Self::set_session_autopilot_timed`]) with its instant, or the cancel of whatever timer is armed ([`Self::cancel_autopilot_timer`]), from `surface` (its setter, §11.7.3, item 2).
    pub(crate) fn choose_autopilot_timer(
        &mut self,
        local_id: u64,
        choice: TimerChoice,
        surface: crate::autopilot::Surface,
    ) -> bool {
        self.choose_autopilot_timer_naming(local_id, choice, None, surface)
    }

    /// [`Self::choose_autopilot_timer`] from a row that named the instant its cancel drops (`cancels`, the open menu's): the cancel then drops that timer and no other.
    pub(crate) fn choose_autopilot_timer_naming(
        &mut self,
        local_id: u64,
        choice: TimerChoice,
        cancels: Option<i64>,
        surface: crate::autopilot::Surface,
    ) -> bool {
        if self
            .session_harness_posture(local_id)
            .default_mode()
            .is_none()
        {
            return false;
        }
        if choice == TimerChoice::Cancel {
            return self.cancel_autopilot_timer(local_id, cancels, surface);
        }
        let Some(land_at) = self.resolve_timer_choice(choice) else {
            return false;
        };
        self.set_session_autopilot_timed(local_id, Some(Autopilot::Cruise), Some(land_at), surface)
    }

    /// "Cancel landing" (the menu's row, Settings' `Cancel`): session `local_id`'s timer dropped with its Cruise kept — a COMPARE-AND-SET on the stored pair (`session_timeline::cancel_land_at`), so it writes nothing unless the store still holds `cruise` with a timer (the very instant `expected` names, where the row named one).
    pub(crate) fn cancel_autopilot_timer(
        &mut self,
        local_id: u64,
        expected: Option<i64>,
        surface: crate::autopilot::Surface,
    ) -> bool {
        let Some(ctx) = self.pool.get(local_id).map(|s| s.ctx.clone()) else {
            return false;
        };
        let write = crate::session_timeline::cancel_land_at(&ctx, expected, surface);
        self.autopilot_pair_written(local_id, write)
    }
}

/// The timer session `ctx` STORES: its `land_at` where its word is `cruise`.
fn stored_timer(ctx: &crate::SessionCtx) -> Option<i64> {
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.land_at
        .filter(|_| meta.autopilot_word() == Some(Autopilot::Cruise))
}

#[cfg(test)]
#[path = "autopilot_timers_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "autopilot_timers_conformance_tests.rs"]
mod conformance_tests;
