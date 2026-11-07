// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET FIELD: the Network State band's line 1 at 60–250 Macs
//! (`docs/DESIGN-fleet-field-2026-11-06.md`).
//!
//! Pure: [`project`] turns the fleet glance's slot (or, with no glance, the observer's peers,
//! [`observer`]) into a [`Field`]; [`paint`] draws it after the Local cell, one stable 7 pt
//! column per seat and then the words, without a `+N` at any count; [`details`] is its lens.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use aterm_types::needs::NeedWord;
use aterm_types::network_state::{NetworkObservation, Reachability, Visibility};

use super::{PlaceCaption, Target};
use crate::fleet_glance::{
    AgentWord, FleetMac, FleetSlot, FleetSnapshot, Reach, TaskWord, Unanswered,
};
use crate::stats_paint::{Hit, HitKind, Painter, panel};
use crate::widget::{DrawPrim, rgba};

#[cfg(test)]
mod tests;

/// Where seat 0's column starts, pt from the band's left edge (the Local cell sits at 100).
pub(crate) const SEATS_X_PT: f32 = 124.0;
/// One seat's column, pt: a 5 pt mark and its 2 pt overlay lane.
pub(crate) const PITCH_PT: f32 = 7.0;
/// A seat mark's width and height, pt.
pub(crate) const MARK_PT: f32 = 5.0;
/// The gap between the last column and the words, and the band's right margin, pt.
pub(crate) const GAP_PT: f32 = 12.0;
/// Fewer columns than this lay out as a tally (Tier C) once the seats overflow them.
pub(crate) const COLS_MIN: usize = 8;
/// The top mark's top (presence and build), pt below the band's top.
const TOP_Y_PT: f32 = 1.0;
/// The activity mark's bottom, pt below the band's top.
const ACT_BOTTOM_PT: f32 = 13.0;
/// The words' baseline, pt below the band's top (today's status words').
const BASELINE_PT: f32 = 10.0;
/// A seat's hit height, pt.
const HIT_H_PT: f32 = 14.0;
/// The observed machines the Local lens lists one by one.
const LOCAL_ROWS_MAX: usize = 8;

/// A seat's identity, never its index: a fleet seat's hashed dial, or an observed peer's slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SeatKey {
    /// A fleet seat ([`seat_key`]).
    Fleet(u64),
    /// One of the observer's authenticated peers, by its slot.
    Observed(u64),
}

/// A class of seats: a tally group (Tier C) and a lens group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum SeatClass {
    Answered,
    Carried,
    Silent,
    NotCounted,
    Busy,
    Waiting,
    Walled,
    Stuck,
    OtherBuild,
}

impl SeatClass {
    /// Every class, in the tally's order.
    pub(crate) const ALL: [Self; 9] = [
        Self::Answered,
        Self::Carried,
        Self::Silent,
        Self::NotCounted,
        Self::Busy,
        Self::Waiting,
        Self::Walled,
        Self::Stuck,
        Self::OtherBuild,
    ];

    /// What the lens and a reader call it.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Carried => "carried",
            Self::Silent => "not answering",
            Self::NotCounted => "not counted",
            Self::Busy => "busy",
            Self::Waiting => "waiting on a person",
            Self::Walled => "at a limit",
            Self::Stuck => "stuck",
            Self::OtherBuild => "on another build",
        }
    }

    const fn presence(self) -> Option<Presence> {
        match self {
            Self::Answered => Some(Presence::Answered),
            Self::Carried => Some(Presence::Carried),
            Self::Silent => Some(Presence::Silent),
            Self::NotCounted => Some(Presence::NotCounted),
            _ => None,
        }
    }
}

/// What the field draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    /// A fresh fleet read.
    Fleet,
    /// No fleet glance: the observer's peers, presence only.
    Observer,
    /// The last read is past [`crate::fleet_glance::COUNT_FRESH`] (`Some(age)`), or ahead of
    /// this clock (`None`): no seat is drawn.
    Stale { age_ms: Option<u64> },
    /// In a fleet, before the first read.
    Reading,
    /// In a fleet, no read yet, and the last one failed.
    NotRead,
}

/// A seat's top body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Presence {
    /// ■ answered this read.
    Answered,
    /// ◧ its last answer, still counting.
    Carried,
    /// □ not answering, silent, or rows past counting.
    Silent,
    /// ▣ answered, its counts not used (an older aterm, an unproven key).
    NotCounted,
}

/// A seat's activity mark (Tier A), by precedence: stuck > walled > waiting > busy > quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Activity {
    /// Read: nothing busy, waiting, walled or stuck.
    Quiet,
    /// Agents busy.
    Busy(u16),
    /// Something waits on a person.
    Waiting,
    /// At a limit.
    Walled,
    /// Stuck.
    Stuck,
}

/// Where a seat's facts are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SeatSrc {
    /// `snapshot.macs[i]`.
    Mac(usize),
    /// `snapshot.unanswered[i]`.
    Unanswered(usize),
    /// The observer's peer of this slot.
    Peer(u64),
}

/// One seat as the strip draws it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SeatMark {
    pub(crate) key: SeatKey,
    pub(crate) name: String,
    pub(crate) presence: Presence,
    /// An observed peer whose reachability is unknown: the warning tick.
    pub(crate) warn: bool,
    /// Its build is not the prevailing one.
    pub(crate) other_build: bool,
    /// `aterm fleet` says it needs an update.
    pub(crate) needs_update: bool,
    /// What its agents do; `None` where not read. Absent, not zero.
    pub(crate) activity: Option<Activity>,
    /// ↑/↓ visit it and it has its own meter.
    pub(crate) attention: bool,
    pub(crate) src: SeatSrc,
}

impl SeatMark {
    /// Whether it is of `class`.
    pub(crate) fn is(&self, class: SeatClass) -> bool {
        match class.presence() {
            Some(presence) => self.presence == presence,
            None => match class {
                SeatClass::Busy => matches!(self.activity, Some(Activity::Busy(_))),
                SeatClass::Waiting => self.activity == Some(Activity::Waiting),
                SeatClass::Walled => self.activity == Some(Activity::Walled),
                SeatClass::Stuck => self.activity == Some(Activity::Stuck),
                _ => self.other_build,
            },
        }
    }
}

/// Counts the words, the tally and the lens read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Tallies {
    /// Seats in all.
    pub(crate) seats: usize,
    pub(crate) answered: usize,
    /// Agents busy, waiting and walled on the counted Macs that answered; `None` where no such
    /// Mac's activity was read.
    pub(crate) busy: Option<usize>,
    pub(crate) waiting: usize,
    pub(crate) walled: usize,
    /// The Macs the walled agents are on.
    pub(crate) walled_macs: usize,
    /// Stuck needs.
    pub(crate) stuck: usize,
    pub(crate) other_builds: usize,
    pub(crate) needs_update: usize,
    /// Macs whose tables lag this Mac's (this Mac's copy only).
    pub(crate) behind: usize,
}

/// The field.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Field {
    pub(crate) source: Source,
    /// In seat order ([`seat_order`]).
    pub(crate) seats: Vec<SeatMark>,
    /// The words, longest first; the last is the reserved shortest form. Empty for
    /// [`Source::Observer`], whose words [`paint`] reads off the observation and the place.
    pub(crate) words: Vec<String>,
    pub(crate) tallies: Tallies,
    /// A counted Mac left rows or instances out: its figures are at least what they say.
    pub(crate) lower_bound: bool,
    /// The prevailing build: the mode of the builds read, a tie to the highest release.
    pub(crate) prevailing: Option<String>,
    /// The read it stands on.
    pub(crate) snapshot: Option<Arc<FleetSnapshot>>,
    /// The read is this Mac's copy (`snapshot.seats`), not the survey.
    pub(crate) copy_path: bool,
    /// The last read failed.
    pub(crate) failed: bool,
    /// When it was projected (unix ms): the lens's ages are as of it.
    pub(crate) now_ms: u64,
    /// Moves exactly when what it draws or says moves.
    pub(crate) key: u64,
}

/// FNV-1a of `bytes`.
fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A fleet seat's key: its dial, the name the book knows it by. A Mac answers with its node id
/// and goes silent without one, so the dial is what both sides carry.
pub(crate) fn seat_key(dial: &str) -> SeatKey {
    SeatKey::Fleet(fnv64(dial.as_bytes()))
}

/// The natural order of seat names: `m1 < m4 < m22 < m104`; `node-` names last, by hex; ties
/// by the dial.
fn seat_order(name: &str, dial: &str) -> (bool, String, String, Option<u128>, String, String) {
    let node = name.starts_with("node-");
    let digits_at = name
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(name.len());
    let (prefix, rest) = name.split_at(digits_at);
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (number, tail) = rest.split_at(digits_end);
    if node {
        return (
            true,
            name.to_owned(),
            String::new(),
            None,
            String::new(),
            dial.to_owned(),
        );
    }
    (
        false,
        String::new(),
        prefix.to_owned(),
        number.parse().ok(),
        tail.to_owned(),
        dial.to_owned(),
    )
}

/// A build's release numbers (`0.160.0+gabc` → `[0, 160, 0]`).
fn release(build: &str) -> Vec<u64> {
    build
        .split('+')
        .next()
        .unwrap_or(build)
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// The mode of `builds`, a tie to the highest release.
pub(crate) fn prevailing<'a>(builds: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for build in builds {
        match counts.iter_mut().find(|(b, _)| *b == build) {
            Some((_, n)) => *n += 1,
            None => counts.push((build, 1)),
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| release(a.0).cmp(&release(b.0))))
        .map(|(build, _)| build.to_owned())
}

/// What a Mac's rows say its agents do, by precedence, where read.
fn activity_of(mac: &FleetMac) -> Option<Activity> {
    let need = |words: &[NeedWord]| mac.needs.iter().any(|need| words.contains(&need.word));
    let tally = mac.activity;
    if need(&[
        NeedWord::Stalled,
        NeedWord::Frozen,
        NeedWord::Stopped,
        NeedWord::Memory,
    ]) {
        return Some(Activity::Stuck);
    }
    if tally.is_some_and(|t| t.walled > 0) || need(&[NeedWord::Wall, NeedWord::Login]) {
        return Some(Activity::Walled);
    }
    if tally.is_some_and(|t| t.waiting > 0)
        || need(&[NeedWord::Ask, NeedWord::Approval, NeedWord::Question])
    {
        return Some(Activity::Waiting);
    }
    let tally = tally?;
    Some(if tally.busy > 0 {
        Activity::Busy(tally.busy)
    } else {
        Activity::Quiet
    })
}

/// The field for `slot` and the observation `obs` at `now_ms`: the observer's peers where no
/// slot is given or this Mac is in no fleet.
pub(crate) fn project(slot: Option<&FleetSlot>, obs: &NetworkObservation, now_ms: u64) -> Field {
    #[cfg(test)]
    tests::PROJECTIONS.with(|n| n.set(n.get() + 1));
    let Some(slot) = slot.filter(|slot| slot.book.in_fleet) else {
        return observer(obs);
    };
    let failed = slot.error.is_some();
    let Some(snapshot) = slot.snapshot.clone() else {
        let source = if failed {
            Source::NotRead
        } else {
            Source::Reading
        };
        let words = vec![
            if failed {
                "fleet not read"
            } else {
                "reading the fleet"
            }
            .to_owned(),
        ];
        return keyed(
            Field {
                source,
                seats: Vec::new(),
                words,
                tallies: Tallies::default(),
                lower_bound: false,
                prevailing: None,
                snapshot: None,
                copy_path: false,
                failed,
                now_ms,
                key: 0,
            },
            slot.generation,
        );
    };
    let copy_path = snapshot.seats.is_some();
    let fresh = snapshot.fresh(now_ms);
    let mut seats = Vec::with_capacity(snapshot.macs.len() + snapshot.unanswered.len());
    for (i, mac) in snapshot.macs.iter().enumerate() {
        let presence = if mac.carried {
            if mac.fresh(now_ms) {
                Presence::Carried
            } else {
                Presence::Silent
            }
        } else if !mac.counts() {
            Presence::NotCounted
        } else {
            Presence::Answered
        };
        seats.push(SeatMark {
            key: seat_key(&mac.dial),
            name: mac.label.clone().unwrap_or_else(|| mac.name.clone()),
            presence,
            warn: false,
            other_build: false,
            needs_update: mac.needs_update,
            activity: (presence == Presence::Answered)
                .then(|| activity_of(mac))
                .flatten(),
            attention: false,
            src: SeatSrc::Mac(i),
        });
    }
    for (i, silent) in snapshot.unanswered.iter().enumerate() {
        // A carried Mac is listed among the Macs too: one seat.
        if snapshot.macs.iter().any(|mac| mac.dial == silent.dial) {
            continue;
        }
        seats.push(SeatMark {
            key: seat_key(&silent.dial),
            name: silent.label.clone().unwrap_or_else(|| silent.name.clone()),
            presence: Presence::Silent,
            warn: false,
            other_build: false,
            needs_update: silent.needs_update,
            activity: None,
            attention: false,
            src: SeatSrc::Unanswered(i),
        });
    }
    let dial_of = |seat: &SeatMark| match seat.src {
        SeatSrc::Mac(i) => snapshot.macs[i].dial.as_str(),
        SeatSrc::Unanswered(i) => snapshot.unanswered[i].dial.as_str(),
        SeatSrc::Peer(_) => "",
    };
    seats.sort_by_cached_key(|seat| seat_order(&seat.name, dial_of(seat)));
    let build_of = |seat: &SeatMark| match seat.src {
        SeatSrc::Mac(i) => snapshot.macs[i].build.as_deref(),
        SeatSrc::Unanswered(i) => snapshot.unanswered[i].build.as_deref(),
        SeatSrc::Peer(_) => None,
    };
    let prevailing = prevailing(seats.iter().filter_map(build_of));
    for seat in &mut seats {
        seat.other_build = build_of(seat).is_some_and(|b| Some(b) != prevailing.as_deref());
        let needs = match seat.src {
            SeatSrc::Mac(i) => snapshot.macs[i].counts() && !snapshot.macs[i].needs.is_empty(),
            _ => false,
        };
        seat.attention = needs
            || matches!(
                seat.activity,
                Some(Activity::Walled | Activity::Waiting | Activity::Stuck)
            )
            || seat.presence == Presence::Silent
            || seat.other_build
            || seat.needs_update;
    }
    let mut tallies = Tallies {
        seats: seats.len(),
        answered: seats
            .iter()
            .filter(|s| matches!(s.presence, Presence::Answered | Presence::NotCounted))
            .count(),
        other_builds: seats.iter().filter(|s| s.other_build).count(),
        needs_update: seats.iter().filter(|s| s.needs_update).count(),
        ..Tallies::default()
    };
    let mut lower_bound = false;
    for mac in snapshot.macs.iter().filter(|m| !m.carried && m.counts()) {
        lower_bound |= mac.left_out > 0 || mac.instances_unread > 0;
        tallies.stuck += mac
            .needs
            .iter()
            .filter(|n| {
                matches!(
                    n.word,
                    NeedWord::Stalled | NeedWord::Frozen | NeedWord::Stopped | NeedWord::Memory
                )
            })
            .count();
        if let Some(tally) = mac.activity {
            *tallies.busy.get_or_insert(0) += usize::from(tally.busy);
            tallies.waiting += usize::from(tally.waiting);
            // A session the tally does not count waiting (a shell; or an agent an older node
            // still counts by its word alone) whose need a person decides waits too, as its
            // seat's diamond says; counted only where the read listed every agent, so no agent is
            // counted twice.
            if mac.sessions_more == 0 {
                tallies.waiting += mac
                    .needs
                    .iter()
                    .filter(|need| need.word.severity() == aterm_types::needs::Severity::Decide)
                    .filter(|need| {
                        !mac.agent_sessions
                            .iter()
                            .any(|s| s.sid == need.sid && s.agent == AgentWord::Waiting)
                    })
                    .count();
            }
            tallies.walled += usize::from(tally.walled);
            tallies.walled_macs += usize::from(tally.walled > 0);
        }
    }
    #[cfg(unix)]
    {
        tallies.behind = snapshot
            .macs
            .iter()
            .filter(|m| matches!(m.tables, Some(aterm_mesh::presence::Behind::Behind { .. })))
            .count();
    }
    let source = if fresh {
        Source::Fleet
    } else {
        Source::Stale {
            age_ms: now_ms.checked_sub(snapshot.read_at_ms),
        }
    };
    let words = fleet_words(source, &tallies, copy_path, lower_bound, failed, obs);
    keyed(
        Field {
            source,
            seats,
            words,
            tallies,
            lower_bound,
            prevailing,
            snapshot: Some(snapshot),
            copy_path,
            failed,
            now_ms,
            key: 0,
        },
        slot.generation,
    )
}

/// The field of the observer's peers, by slot: no fleet glance, or no fleet.
pub(crate) fn observer(obs: &NetworkObservation) -> Field {
    let mut peers: Vec<_> = obs.peers.iter().collect();
    peers.sort_by_key(|peer| peer.slot);
    let seats = peers
        .into_iter()
        .map(|peer| {
            let reachability = super::observed_reachability(obs.visibility, peer.reachability);
            SeatMark {
                key: SeatKey::Observed(peer.slot),
                name: super::machine_name(&peer.identity),
                presence: if reachability == Reachability::Reachable {
                    Presence::Answered
                } else {
                    Presence::Silent
                },
                warn: reachability == Reachability::Unknown,
                other_build: false,
                needs_update: false,
                activity: None,
                attention: true,
                src: SeatSrc::Peer(peer.slot),
            }
        })
        .collect::<Vec<_>>();
    keyed(
        Field {
            source: Source::Observer,
            tallies: Tallies {
                seats: seats.len(),
                ..Tallies::default()
            },
            seats,
            words: Vec::new(),
            lower_bound: false,
            prevailing: None,
            snapshot: None,
            copy_path: false,
            failed: false,
            now_ms: 0,
            key: 0,
        },
        0,
    )
}

fn keyed(mut field: Field, generation: u16) -> Field {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    field.source.hash(&mut h);
    generation.hash(&mut h);
    field.failed.hash(&mut h);
    field.seats.hash(&mut h);
    field.words.hash(&mut h);
    field.lower_bound.hash(&mut h);
    field.key = h.finish() | 1;
    field
}

/// `n`, with a `+` where the counted Macs left some out.
fn figure(n: usize, lower_bound: bool) -> String {
    if lower_bound {
        format!("{n}+")
    } else {
        n.to_string()
    }
}

/// The observer's status word when its visibility is not Active (it leads the fleet's words).
fn observer_lead(obs: &NetworkObservation) -> Option<&'static str> {
    (obs.visibility != Visibility::Active).then_some(match obs.visibility {
        Visibility::Checking => "Checking",
        Visibility::Offline => "Offline",
        Visibility::Disabled => "Off",
        Visibility::Waiting => "Waiting",
        Visibility::Unavailable | Visibility::Active => "Unavailable",
    })
}

/// Rule F's forms, longest first; a term appears only where its source was read.
fn fleet_words(
    source: Source,
    t: &Tallies,
    copy_path: bool,
    lower_bound: bool,
    failed: bool,
    obs: &NetworkObservation,
) -> Vec<String> {
    let mut forms = match source {
        Source::Stale { age_ms } => {
            let age = age_ms.map_or_else(
                || "ahead of this clock".to_owned(),
                |age| format!("{} ago", aterm_netplace::words::age_words(age)),
            );
            return vec![format!("fleet read {age}"), age];
        }
        Source::Fleet => {
            let (a, n) = (t.answered, t.seats);
            let terms = |long: bool| {
                let mut terms = Vec::new();
                let f = |n| figure(n, lower_bound);
                if copy_path {
                    if t.needs_update > 0 {
                        terms.push(format!(
                            "{} need{} update",
                            t.needs_update,
                            if t.needs_update == 1 { "s" } else { "" }
                        ));
                    }
                    if t.behind > 0 {
                        terms.push(format!("{} behind", t.behind));
                    }
                } else {
                    if let Some(busy) = t.busy {
                        terms.push(format!("{} busy", f(busy)));
                        if t.waiting > 0 {
                            terms.push(format!(
                                "{} {}",
                                f(t.waiting),
                                if long { "waiting" } else { "wait" }
                            ));
                        }
                        if t.walled > 0 {
                            terms.push(format!(
                                "{} {}",
                                f(t.walled),
                                if long { "walled" } else { "wall" }
                            ));
                        }
                    }
                    if t.stuck > 0 {
                        terms.push(format!("{} stuck", t.stuck));
                    }
                }
                if long && t.other_builds > 0 {
                    terms.push(count(t.other_builds, "other build", "other builds"));
                }
                terms
            };
            let verb = if copy_path { "up" } else { "answered" };
            let join = |head: String, terms: Vec<String>| {
                std::iter::once(head)
                    .chain(terms)
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            let mut forms = vec![
                join(format!("{a} of {n} {verb}"), terms(true)),
                join(format!("{a}/{n} {verb}"), terms(false)),
            ];
            let first = terms(false).into_iter().next();
            if let Some(first) = first {
                forms.push(join(format!("{a}/{n} {verb}"), vec![first]));
            }
            forms.push(format!("{a}/{n} {verb}"));
            forms.push(format!("{a}/{n}"));
            forms.dedup();
            forms
        }
        Source::Observer | Source::Reading | Source::NotRead => return Vec::new(),
    };
    let last = forms.len() - 1;
    if failed {
        for form in &mut forms[..last] {
            form.push_str(" · last read failed");
        }
    }
    if let Some(lead) = observer_lead(obs) {
        for form in &mut forms[..last] {
            *form = format!("{lead} · {form}");
        }
    }
    forms
}

/// How the strip lays its seats out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    /// One seat a column: presence on top, activity below.
    A,
    /// Two seats a column, column-major: presence and build only.
    B,
    /// A tally: glyph and figure per class.
    C,
}

/// The tier of `n` seats in `cols` columns: one a column while they fit, two a column while
/// they fit so, a tally past that, and a tally at once below [`COLS_MIN`] columns, where a strip
/// of pairs is no strip.
pub(crate) const fn tier(n: usize, cols: usize) -> Tier {
    if n <= cols {
        Tier::A
    } else if cols < COLS_MIN || n > 2 * cols {
        Tier::C
    } else {
        Tier::B
    }
}

/// The columns a band `w` px wide holds at `scale`, once `words_min` px of words is reserved.
pub(crate) fn cols(w: f32, scale: f32, words_min: f32) -> usize {
    let room = w - (SEATS_X_PT + GAP_PT + GAP_PT) * scale - words_min;
    (room / (PITCH_PT * scale)).floor().max(0.0) as usize
}

/// The width of `s` in the band's 9.5 pt UI face at `scale`, in whole device px: what
/// [`Painter::ui_text_w`] measures, without a painter, so a layout and its keys agree.
fn text_w(s: &str, scale: f32) -> f32 {
    crate::tray_raster::ui_text_width_for(crate::widget::TextFace::Ui, s, 9.5 * scale).ceil()
}

/// The observer's words: its status, then what fits of its optional words (the forms the band
/// drew before the field, now fitted rather than dropped).
fn observer_words(
    scale: f32,
    obs: &NetworkObservation,
    caption: Option<&PlaceCaption>,
    retention_limited: bool,
    room: f32,
) -> String {
    use super::NetworkStatus;
    let status = super::network_status_of(obs);
    let (long, short): (String, String) = match status {
        NetworkStatus::HistoryUnavailable => ("History unavailable".into(), "History error".into()),
        NetworkStatus::HistoryRestoring => ("Restoring history".into(), "Restoring".into()),
        NetworkStatus::Visibility(Visibility::Active) => {
            let reachable = obs
                .peers
                .iter()
                .filter(|p| p.reachability == Reachability::Reachable)
                .count();
            let words = format!("{reachable} reachable");
            (words.clone(), words)
        }
        NetworkStatus::Visibility(Visibility::Checking) => ("Checking".into(), "Checking".into()),
        NetworkStatus::Visibility(Visibility::Offline) => ("Offline".into(), "Offline".into()),
        NetworkStatus::Visibility(Visibility::Disabled) => ("Off".into(), "Off".into()),
        NetworkStatus::Visibility(Visibility::Waiting) => ("Waiting".into(), "Waiting".into()),
        NetworkStatus::Visibility(Visibility::Unavailable) => {
            ("Unavailable".into(), "Unavailable".into())
        }
    };
    let fits = |s: &str| text_w(s, scale) <= room;
    let mut text = if fits(&long) { long } else { short };
    let mut extras = Vec::new();
    if status == NetworkStatus::Visibility(Visibility::Active) && obs.via_local_process.is_some() {
        extras.push("via local aterm".to_owned());
    }
    if status != NetworkStatus::Visibility(Visibility::Active) && !obs.peers.is_empty() {
        extras.push(format!("{} known", obs.peers.len()));
    }
    if let Some(caption) = caption {
        // Place does not establish observer or history health: keep their status first.
        if super::place_only(status, caption) {
            text = if fits(&caption.long) {
                caption.long.clone()
            } else {
                caption.short.clone()
            };
            extras.clear();
        } else {
            extras.push(caption.short.clone());
        }
    }
    if retention_limited {
        extras.push("partial history".to_owned());
    }
    for extra in extras {
        let composed = format!("{text} · {extra}");
        if fits(&composed) {
            text = composed;
        }
    }
    text
}

/// The shortest observer status: what its words reserve.
fn observer_words_min(scale: f32, obs: &NetworkObservation) -> f32 {
    observer_words(scale, obs, None, false, 0.0)
        .split(" · ")
        .next()
        .map_or(0.0, |s| text_w(s, scale))
}

/// One group a tally (Tier C) draws: its class, its figure, and where its glyph starts, px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TallyGroup {
    pub(crate) class: SeatClass,
    pub(crate) count: usize,
    pub(crate) x: f32,
}

/// How line 1 lays a field out on one band: what [`paint`] draws and what [`targets`] visits,
/// so the keys reach exactly what is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    pub(crate) tier: Tier,
    /// The seats the strip draws: none for a read it does not draw.
    pub(crate) n: usize,
    /// A tally's groups that fit beside the reserved words, in the tally's order.
    pub(crate) groups: Vec<TallyGroup>,
    /// Where the words start, px.
    pub(crate) words_x: f32,
}

/// The layout of `field` on a band `w` px wide at `scale`, the observation `obs` giving the
/// observer's words: its shortest words reserved before any seat or group is laid out.
pub(crate) fn layout(field: &Field, obs: &NetworkObservation, w: f32, scale: f32) -> Layout {
    let words_min = if field.source == Source::Observer {
        observer_words_min(scale, obs)
    } else {
        field.words.last().map_or(0.0, |form| text_w(form, scale))
    };
    layout_with(field, w, scale, words_min)
}

/// [`layout`] with `words_min` px of words reserved.
fn layout_with(field: &Field, w: f32, s: f32, words_min: f32) -> Layout {
    let seats_x = SEATS_X_PT * s;
    let right = w - GAP_PT * s;
    let drawn = matches!(field.source, Source::Fleet | Source::Observer);
    let n = if drawn { field.seats.len() } else { 0 };
    let tier = tier(n, cols(w, s, words_min));
    let mut groups = Vec::new();
    let words_x = match tier {
        Tier::A | Tier::B => {
            let used = n.div_ceil(if tier == Tier::A { 1 } else { 2 });
            if used == 0 {
                seats_x
            } else {
                seats_x + (used as f32 * PITCH_PT + GAP_PT) * s
            }
        }
        Tier::C => {
            // Every group, presence and activity alike, leaves the reserved words their room:
            // the groups that fit are drawn in the tally's order, and the words' `a/n` counts
            // every seat when a narrow band holds fewer.
            let limit = right - words_min - GAP_PT * s;
            let mut x = seats_x;
            let mut end = None;
            let mut last: Option<bool> = None;
            for class in SeatClass::ALL {
                let count = field.seats.iter().filter(|seat| seat.is(class)).count();
                if count == 0 {
                    continue;
                }
                let activity_side = class.presence().is_none();
                // `·` between the presence groups and the activity groups.
                let sep = if last.is_some_and(|was| was != activity_side) {
                    4.0 * s
                } else {
                    0.0
                };
                let width = (MARK_PT + 2.0) * s + text_w(&count.to_string(), s);
                // A wide group that does not fit leaves the room to a narrower one after it.
                if x + sep + width > limit {
                    continue;
                }
                x += sep;
                groups.push(TallyGroup { class, count, x });
                x += width;
                end = Some(x);
                x += 6.0 * s;
                last = Some(activity_side);
            }
            end.map_or(seats_x, |end| end + GAP_PT * s)
        }
    };
    Layout {
        tier,
        n,
        groups,
        words_x,
    }
}

/// Seat glyph geometry, in device px.
struct Glyph {
    x: f32,
    y: f32,
    side: f32,
    s: f32,
    hair: f32,
}

fn stroke(x: f32, y: f32, w: f32, h: f32, width: f32, color: aterm_vitals::ink::Rgb) -> DrawPrim {
    DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius: 0.0,
        width,
        color: rgba(color, 255),
    }
}

fn line(
    p: &mut Painter<'_>,
    (x1, y1): (f32, f32),
    (x2, y2): (f32, f32),
    g: &Glyph,
    c: aterm_vitals::ink::Rgb,
) {
    p.prims.push(DrawPrim::Line {
        x1,
        y1,
        x2,
        y2,
        width: g.hair,
        color: rgba(c, 255),
    });
}

/// A seat's top body at `g` (its top-left).
fn body(p: &mut Painter<'_>, g: &Glyph, presence: Presence) {
    let inks = *p.inks;
    let Glyph {
        x, y, side, hair, ..
    } = *g;
    match presence {
        Presence::Answered => p
            .prims
            .push(panel(x, y, side, side, 0.0, rgba(inks.machine, 255))),
        Presence::Carried => {
            p.prims
                .push(panel(x, y, side / 2.0, side, 0.0, rgba(inks.machine, 255)));
            p.prims.push(stroke(x, y, side, side, hair, inks.machine));
        }
        Presence::Silent => p.prims.push(stroke(x, y, side, side, hair, inks.hollow)),
        Presence::NotCounted => {
            p.prims.push(stroke(x, y, side, side, hair, inks.machine));
            p.prims.push(panel(
                x + (side - hair) / 2.0,
                y + (side - hair) / 2.0,
                hair,
                hair,
                0.0,
                rgba(inks.machine, 255),
            ));
        }
    }
}

/// The 2 pt square at a body's top-right: the band's warning convention.
fn corner(p: &mut Painter<'_>, g: &Glyph) {
    let over = p.inks.over;
    p.prims.push(panel(
        g.x + g.side,
        g.y,
        2.0 * g.s,
        2.0 * g.s,
        0.0,
        rgba(over, 255),
    ));
}

/// The hair under a body: another build.
fn build_hair(p: &mut Painter<'_>, g: &Glyph) {
    let words2 = p.inks.words2;
    p.prims.push(panel(
        g.x,
        g.y + g.side + (g.s - g.hair).max(0.0) / 2.0,
        g.side,
        g.hair,
        0.0,
        rgba(words2, 255),
    ));
}

/// An activity mark in the box `g` (its top-left), bottom-anchored.
fn activity(p: &mut Painter<'_>, g: &Glyph, mark: Activity) {
    let inks = *p.inks;
    let Glyph {
        x,
        y,
        side,
        s,
        hair,
    } = *g;
    let bottom = y + side;
    match mark {
        Activity::Quiet => p.prims.push(panel(
            x,
            bottom - hair,
            side,
            hair,
            0.0,
            rgba(inks.grid, 255),
        )),
        Activity::Busy(n) => {
            // ▌: a 3 pt bar, so a full one never reads as an answered body.
            let h = if n >= 2 { side } else { (2.5 * s).round() };
            p.prims.push(panel(
                x,
                bottom - h,
                (3.0 * s).round(),
                h,
                0.0,
                rgba(inks.machine, 255),
            ));
        }
        Activity::Waiting => {
            let (cx, cy) = (x + side / 2.0, y + side / 2.0);
            let points = [(x, cy), (cx, y), (x + side, cy), (cx, bottom), (x, cy)];
            for pair in points.windows(2) {
                line(p, pair[0], pair[1], g, inks.aterm);
            }
        }
        Activity::Walled => {
            p.prims
                .push(panel(x, y, side, hair, 0.0, rgba(inks.over, 255)));
            p.prims.push(panel(
                x,
                y + 2.0 * hair,
                side,
                side - 2.0 * hair,
                0.0,
                rgba(inks.over, 255),
            ));
        }
        Activity::Stuck => {
            line(p, (x, y), (x + side, bottom), g, inks.over);
            line(p, (x + side, y), (x, bottom), g, inks.over);
        }
    }
}

/// One tally glyph for `class` at `g`.
fn class_glyph(p: &mut Painter<'_>, g: &Glyph, class: SeatClass) {
    if let Some(presence) = class.presence() {
        return body(p, g, presence);
    }
    match class {
        SeatClass::Busy => activity(p, g, Activity::Busy(1)),
        SeatClass::Waiting => activity(p, g, Activity::Waiting),
        SeatClass::Walled => activity(p, g, Activity::Walled),
        SeatClass::Stuck => activity(p, g, Activity::Stuck),
        _ => {
            body(p, g, Presence::Silent);
            build_hair(p, g);
        }
    }
}

/// Draw `field` on line 1 of a band `w` px wide whose top is `top`, after the Local cell, and
/// insert its hits at the front of `hits`. `obs` and `caption` give the observer's words and
/// the place words the frame captured.
pub(crate) fn paint(
    p: &mut Painter<'_>,
    field: &Field,
    obs: &NetworkObservation,
    caption: Option<&PlaceCaption>,
    top: f32,
    w: f32,
    hits: &mut Vec<Hit>,
) {
    let s = p.s;
    let inks = *p.inks;
    let hair = p.hair();
    let seats_x = SEATS_X_PT * s;
    let right = w - GAP_PT * s;
    let Layout {
        tier,
        n,
        groups,
        words_x,
    } = layout(field, obs, w, s);
    let glyph = |x: f32, y_pt: f32| Glyph {
        x: x.round(),
        y: (top + y_pt * s).round(),
        side: (MARK_PT * s).round(),
        s,
        hair,
    };
    let mut seat_hits = Vec::new();
    match tier {
        Tier::A | Tier::B => {
            let per = if tier == Tier::A { 1 } else { 2 };
            for (i, seat) in field.seats.iter().enumerate().take(n) {
                let col = i / per;
                let row = i % per;
                let x0 = seats_x + col as f32 * PITCH_PT * s;
                let y_pt = if row == 0 {
                    TOP_Y_PT
                } else {
                    ACT_BOTTOM_PT - MARK_PT
                };
                let g = glyph(x0, y_pt);
                body(p, &g, seat.presence);
                if seat.other_build {
                    build_hair(p, &g);
                }
                if seat.needs_update || seat.warn {
                    corner(p, &g);
                }
                if tier == Tier::A
                    && let Some(mark) = seat.activity
                {
                    activity(p, &glyph(x0, ACT_BOTTOM_PT - MARK_PT), mark);
                }
                let (hy, hh) = match (tier, row) {
                    (Tier::A, _) => (top, HIT_H_PT * s),
                    (_, 0) => (top, HIT_H_PT / 2.0 * s),
                    _ => (top + HIT_H_PT / 2.0 * s, HIT_H_PT / 2.0 * s),
                };
                seat_hits.push(Hit {
                    rect: (x0, hy, PITCH_PT * s, hh),
                    kind: HitKind::Network(Target::Seat(seat.key)),
                });
            }
        }
        Tier::C => {
            for group in groups {
                let start = group.x;
                class_glyph(p, &glyph(start, BASELINE_PT - MARK_PT - 1.0), group.class);
                let x = start + (MARK_PT + 2.0) * s;
                let figure = group.count.to_string();
                let end = x + p.ui_text_w(&figure, false);
                p.ui_text(x, top + BASELINE_PT * s, figure, inks.words2, false);
                seat_hits.push(Hit {
                    rect: (start - 2.0 * s, top, end - start + 2.0 * s, HIT_H_PT * s),
                    kind: HitKind::Network(Target::Group(group.class)),
                });
            }
        }
    }
    let room = (right - words_x).max(0.0);
    let text = if field.source == Source::Observer {
        observer_words(s, obs, caption, obs.retention_limited, room)
    } else {
        let mut text = field
            .words
            .iter()
            .find(|form| p.ui_text_w(form, false) <= room)
            .or(field.words.last())
            .cloned()
            .unwrap_or_default();
        let mut extras = Vec::new();
        if obs.visibility == Visibility::Active && obs.via_local_process.is_some() {
            extras.push("via local aterm".to_owned());
        }
        if let Some(caption) = caption {
            extras.push(caption.short.clone());
        }
        for extra in extras {
            let composed = format!("{text} · {extra}");
            if p.ui_text_w(&composed, false) <= room {
                text = composed;
            }
        }
        text
    };
    if !text.is_empty() {
        p.ui_text(words_x, top + BASELINE_PT * s, text, inks.words2, false);
    }
    if field.source != Source::Observer && room > 0.0 {
        hits.insert(
            0,
            Hit {
                rect: (words_x, top, room, HIT_H_PT * s),
                kind: HitKind::Network(Target::Fleet),
            },
        );
    }
    for hit in seat_hits {
        hits.insert(0, hit);
    }
}

/// The targets ↑/↓ visit on the field as `layout` draws it, in strip order: the Fleet words,
/// then the seats that need attention, or a tally's drawn groups (every observed seat in
/// observer fallback). A stale read has none.
pub(crate) fn targets(field: &Field, layout: &Layout) -> Vec<Target> {
    let seats = field
        .seats
        .iter()
        .filter(|seat| seat.attention)
        .map(|seat| Target::Seat(seat.key));
    match field.source {
        Source::Observer => seats.collect(),
        Source::Fleet if layout.tier == Tier::C => std::iter::once(Target::Fleet)
            .chain(layout.groups.iter().map(|group| Target::Group(group.class)))
            .collect(),
        Source::Fleet => std::iter::once(Target::Fleet).chain(seats).collect(),
        Source::Stale { .. } | Source::Reading | Source::NotRead => vec![Target::Fleet],
    }
}

/// A reason's words.
pub(crate) const fn reach_words(reach: Reach) -> &'static str {
    match reach {
        Reach::NoName => "name did not resolve",
        Reach::NothingListens => "nothing listens",
        Reach::NoAnswer => "did not answer in time",
        Reach::NoRoute => "no route from this network",
        Reach::RefusedHere => "refused here",
        Reach::Reset => "connection reset",
        Reach::Connect => "could not connect",
        Reach::NoHandshake => "no handshake",
        Reach::Handshake => "handshake failed",
        Reach::NoReply => "no reply",
        Reach::Identity => "not the node it should be",
        Reach::NotDialled => "not dialled",
        Reach::Gone => "gone off the link",
        Reach::Silent { .. } => "silent",
        Reach::NeverUp => "never seen up here",
        Reach::PastCap => "past the peers one read keeps",
        Reach::NoHeader => "answered with no header",
        Reach::Unread => "reason not read",
    }
}

/// The group a reason falls in: its token, or the glance's own kind of reason.
fn reach_group(reach: Reach) -> Reach {
    match reach {
        Reach::Silent { .. } => Reach::Silent { since_ms: 0 },
        other => other,
    }
}

/// `Not answering`'s lines: one per reason, each Mac with its build and silent age where read.
pub(crate) fn silence_groups(field: &Field) -> Vec<(Reach, Vec<String>)> {
    let Some(snapshot) = field.snapshot.as_deref() else {
        return Vec::new();
    };
    let mut groups: Vec<(Reach, Vec<String>)> = Vec::new();
    for seat in field
        .seats
        .iter()
        .filter(|s| s.presence == Presence::Silent)
    {
        let (reach, build, since) = match seat.src {
            SeatSrc::Unanswered(i) => {
                let u: &Unanswered = &snapshot.unanswered[i];
                (u.reason, u.build.as_deref(), u.silent_since_ms)
            }
            SeatSrc::Mac(i) => {
                let mac = &snapshot.macs[i];
                let reason = snapshot
                    .unanswered
                    .iter()
                    .find(|u| u.dial == mac.dial)
                    .map_or(Reach::Unread, |u| u.reason);
                (reason, mac.build.as_deref(), Some(mac.read_at_ms))
            }
            SeatSrc::Peer(_) => continue,
        };
        let mut facts = Vec::new();
        if let Some(build) = build {
            facts.push(build.to_owned());
        }
        if let Some(since) = since {
            facts.push(aterm_netplace::words::age_words(
                field.now_ms.saturating_sub(since),
            ));
        }
        let entry = if facts.is_empty() {
            seat.name.clone()
        } else {
            format!("{} ({})", seat.name, facts.join(", "))
        };
        let group = reach_group(reach);
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, names)) => names.push(entry),
            None => groups.push((group, vec![entry])),
        }
    }
    groups
}

/// Why a Mac's agents went uncounted, as the lens says it: on this Mac's copy, why its
/// `now/activity` row did not count ([`crate::fleet_glance::FleetMac::activity_absent`]).
fn activity_absent_words(mac: &crate::fleet_glance::FleetMac) -> String {
    #[cfg(unix)]
    {
        use crate::fleet_glance::ActivityAbsent;
        match mac.activity_absent {
            Some(ActivityAbsent::Unread) => "not read: the node read no activity rows".into(),
            Some(ActivityAbsent::Unpublished) => {
                "not published: an older aterm, or no window counted".into()
            }
            Some(ActivityAbsent::Stale {
                behind_ms: Some(behind),
            }) => format!(
                "not read for {}: its last count trails its presence",
                aterm_netplace::words::age_words(behind)
            ),
            Some(ActivityAbsent::Stale { behind_ms: None }) => {
                "not read: its last count is stale".into()
            }
            None => "not read".into(),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = mac;
        "not read".into()
    }
}

fn time_and_age(at_ms: u64, now_ms: u64) -> String {
    format!(
        "{} ({} ago)",
        super::observation_time(at_ms),
        aterm_netplace::words::age_words(now_ms.saturating_sub(at_ms))
    )
}

#[cfg(unix)]
fn behind_words(behind: aterm_mesh::presence::Behind) -> String {
    use aterm_mesh::presence::Behind;
    match behind {
        Behind::InStep => "in step".into(),
        Behind::Behind { at_least } => format!(
            "behind by at least {}",
            aterm_netplace::words::age_words(
                u64::try_from(at_least.as_millis()).unwrap_or(u64::MAX)
            )
        ),
        Behind::Apart => "apart: it holds records this Mac lacks".into(),
        Behind::Unknown => "not read".into(),
    }
}

fn agent_word(word: &AgentWord) -> String {
    match word {
        AgentWord::Busy => "busy".into(),
        AgentWord::Idle => "idle".into(),
        AgentWord::Waiting => "waiting".into(),
        AgentWord::Walled(kind) => format!("wall:{kind}"),
        AgentWord::Unknown => "unknown".into(),
    }
}

const fn task_word(task: TaskWord) -> &'static str {
    match task {
        TaskWord::Working => "working",
        TaskWord::Done => "done",
        TaskWord::Failed => "failed",
        TaskWord::Exited => "exited",
        TaskWord::Stalled => "stalled",
    }
}

fn pct(v: Option<u8>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{v} %"))
}

/// The Fleet's `Now` line.
pub(crate) fn now_line(field: &Field) -> String {
    let Some(snapshot) = field.snapshot.as_deref() else {
        return field.words.first().cloned().unwrap_or_default();
    };
    let t = &field.tallies;
    let verb = if field.copy_path { "up" } else { "answered" };
    let mut line = format!(
        "{} of {} {verb} at {}",
        t.answered,
        t.seats,
        time_and_age(snapshot.read_at_ms, field.now_ms)
    );
    if let Source::Stale { .. } = field.source {
        line = format!("{}; this read no longer counts", field.words[0]);
    }
    if let Some(busy) = t.busy {
        let least = if field.lower_bound { "at least " } else { "" };
        line.push_str(&format!(
            " · {least}{busy} agents busy, {} waiting on a person, {} walled on {}",
            t.waiting,
            t.walled,
            count(t.walled_macs, "Mac", "Macs")
        ));
    }
    if t.stuck > 0 {
        line.push_str(&format!(", {} stuck", t.stuck));
    }
    line.push_str(if field.copy_path {
        " · this Mac's node view"
    } else {
        " · survey of every Mac each minute"
    });
    if field.failed {
        line.push_str(" · the last read failed");
    }
    line
}

/// The lens rows for `target` on `field`; empty for a target that is not the field's.
pub(crate) fn details(field: &Field, target: Target) -> Vec<(String, String)> {
    match target {
        Target::Fleet => fleet_details(field),
        Target::Seat(key @ SeatKey::Fleet(_)) => seat_details(field, key),
        Target::Group(class) => group_details(field, class),
        _ => Vec::new(),
    }
}

fn fleet_details(field: &Field) -> Vec<(String, String)> {
    let mut rows = vec![(
        "Fleet".to_owned(),
        field.words.first().cloned().unwrap_or_default(),
    )];
    let Some(snapshot) = field.snapshot.as_deref() else {
        return rows;
    };
    rows.push(("Now".into(), now_line(field)));
    let name_of = |mac: &FleetMac| mac.label.clone().unwrap_or_else(|| mac.name.clone());
    let counted = || snapshot.macs.iter().filter(|m| !m.carried && m.counts());
    // Waiting on a person: an agent's word or a need, once a session.
    let mut waiting: Vec<(String, String)> = Vec::new();
    let mut walls: Vec<(String, Vec<String>)> = Vec::new();
    let mut stuck = Vec::new();
    for mac in counted() {
        let name = name_of(mac);
        for need in &mac.needs {
            let age = need.age_ms.map_or_else(String::new, |a| {
                format!(" · {}", aterm_netplace::words::age_words(a))
            });
            let line = format!(
                "{name} · {} · {} · {}{age}",
                need.sid,
                need.who,
                need.word.word()
            );
            match need.word {
                NeedWord::Ask | NeedWord::Approval | NeedWord::Question => {
                    if !waiting.iter().any(|(sid, _)| *sid == need.sid) {
                        waiting.push((need.sid.clone(), line));
                    }
                }
                NeedWord::Wall | NeedWord::Login => {
                    let group = format!("need {}", need.word.word());
                    push_group(&mut walls, group, format!("{name} · {}", need.sid));
                }
                _ => stuck.push(line),
            }
        }
        for session in &mac.agent_sessions {
            let program = session.program.as_deref().unwrap_or("–");
            match &session.agent {
                AgentWord::Waiting => {
                    if !waiting.iter().any(|(sid, _)| *sid == session.sid) {
                        waiting.push((
                            session.sid.clone(),
                            format!("{name} · {} · {program} · waiting", session.sid),
                        ));
                    }
                }
                AgentWord::Walled(kind) => push_group(
                    &mut walls,
                    format!("wall:{kind}"),
                    format!(
                        "{name} · {} · 7d {}",
                        session.sid,
                        pct(session.usage_7d_pct)
                    ),
                ),
                _ => {}
            }
        }
    }
    if !waiting.is_empty() {
        rows.push((
            "Waiting on a person".into(),
            waiting
                .into_iter()
                .map(|(_, l)| l)
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    if !walls.is_empty() {
        rows.push((
            "At a limit".into(),
            walls
                .into_iter()
                .map(|(g, l)| format!("{g} — {}", l.join(", ")))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    if !stuck.is_empty() {
        rows.push(("Stuck".into(), stuck.join("; ")));
    }
    let groups = silence_groups(field);
    if !groups.is_empty() {
        rows.push((
            "Not answering".into(),
            groups
                .iter()
                .map(|(reach, names)| format!("{} — {}", reach_words(*reach), names.join(", ")))
                .collect::<Vec<_>>()
                .join("; "),
        ));
        let words: Vec<String> = field
            .seats
            .iter()
            .filter(|s| s.presence == Presence::Silent)
            .filter_map(|s| match s.src {
                SeatSrc::Unanswered(i) => Some((s, &snapshot.unanswered[i].why)),
                _ => None,
            })
            .filter(|(_, why)| !why.is_empty())
            .map(|(s, why)| format!("{}: {why}", s.name))
            .collect();
        if !words.is_empty() {
            rows.push(("Their words".into(), words.join("; ")));
        }
    }
    let builds = builds_line(field, snapshot);
    if !builds.is_empty() {
        rows.push(("Builds".into(), builds));
    }
    if t_read(field) {
        rows.push(("Activity spread".into(), spread_line(field, snapshot)));
    }
    let mut tasks = [0usize; 5];
    for mac in counted() {
        for session in &mac.agent_sessions {
            if let Some(task) = session.task {
                tasks[task as usize] += 1;
            }
        }
    }
    if tasks.iter().any(|n| *n > 0) {
        rows.push((
            "Tasks".into(),
            format!(
                "{} working · {} done · {} failed",
                tasks[TaskWord::Working as usize],
                tasks[TaskWord::Done as usize],
                tasks[TaskWord::Failed as usize]
            ),
        ));
    }
    if let Some(seats) = &snapshot.seats {
        if let Some(step) = seats.in_step {
            let mut line = format!("in step {} of {}", step.with, step.of);
            let behind = field.tallies.behind;
            if behind > 0 {
                line.push_str(&format!(" · behind {behind}"));
                if let Some(ms) = step.slowest_ms {
                    line.push_str(&format!(
                        " (slowest {})",
                        aterm_netplace::words::age_words(ms)
                    ));
                }
            }
            rows.push(("Tables".into(), line));
        }
        if let Some((held, bound)) = seats.log {
            rows.push(("Log".into(), format!("{held}/{bound}")));
        }
    }
    rows
}

/// `35 Macs busy · 6 waiting or at a limit · 13 quiet · 4 no agents · 6 not read`: every seat in
/// one bucket, from its Mac's tally, so the figures sum to the seats and a wall never hides
/// busy agents.
fn spread_line(field: &Field, snapshot: &FleetSnapshot) -> String {
    let (mut busy, mut held, mut quiet, mut none, mut not_read) = (0, 0, 0, 0, 0);
    for seat in &field.seats {
        let tally = match seat.src {
            SeatSrc::Mac(i) if seat.activity.is_some() => snapshot.macs[i].activity,
            _ => None,
        };
        match tally {
            None => not_read += 1,
            Some(t) if t.busy > 0 => busy += 1,
            Some(t) if t.agents == 0 => none += 1,
            Some(_)
                if matches!(
                    seat.activity,
                    Some(Activity::Waiting | Activity::Walled | Activity::Stuck)
                ) =>
            {
                held += 1;
            }
            Some(_) => quiet += 1,
        }
    }
    let mut terms = vec![format!("{} busy", count(busy, "Mac", "Macs"))];
    if held > 0 {
        terms.push(format!("{held} waiting, at a limit or stuck"));
    }
    terms.push(format!("{quiet} quiet"));
    terms.push(format!("{none} no agents"));
    terms.push(format!("{not_read} not read"));
    terms.join(" · ")
}

/// `n` and its noun, singular for one: `1 other build`, `5 other builds`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn t_read(field: &Field) -> bool {
    field.tallies.busy.is_some()
}

fn push_group<T>(groups: &mut Vec<(String, Vec<T>)>, group: String, entry: T) {
    match groups.iter_mut().find(|(g, _)| *g == group) {
        Some((_, entries)) => entries.push(entry),
        None => groups.push((group, vec![entry])),
    }
}

/// `0.160.0 ×58 (m9 needs update) · 0.156.0 m4 · 0.147.0 m106 (needs update)`: the prevailing
/// build is counted, yet a seat on it that needs an update is still named.
fn builds_line(field: &Field, snapshot: &FleetSnapshot) -> String {
    let mut builds: Vec<(String, Vec<(String, bool)>)> = Vec::new();
    for seat in &field.seats {
        let build = match seat.src {
            SeatSrc::Mac(i) => snapshot.macs[i].build.clone(),
            SeatSrc::Unanswered(i) => snapshot.unanswered[i].build.clone(),
            SeatSrc::Peer(_) => None,
        };
        let Some(build) = build else { continue };
        push_group(&mut builds, build, (seat.name.clone(), seat.needs_update));
    }
    builds.sort_by_key(|(build, _)| std::cmp::Reverse(release(build)));
    builds
        .into_iter()
        .map(|(build, seats)| {
            if Some(&build) == field.prevailing.as_ref() {
                let due: Vec<&str> = seats
                    .iter()
                    .filter(|(_, needs_update)| *needs_update)
                    .map(|(name, _)| name.as_str())
                    .collect();
                if due.is_empty() {
                    format!("{build} ×{}", seats.len())
                } else {
                    format!("{build} ×{} ({} needs update)", seats.len(), due.join(", "))
                }
            } else {
                let names: Vec<String> = seats
                    .into_iter()
                    .map(|(name, needs_update)| {
                        if needs_update {
                            format!("{name} (needs update)")
                        } else {
                            name
                        }
                    })
                    .collect();
                format!("{build} {}", names.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn seat_details(field: &Field, key: SeatKey) -> Vec<(String, String)> {
    let Some(seat) = field.seats.iter().find(|s| s.key == key) else {
        return vec![("Machine".into(), "No longer among the fleet's seats".into())];
    };
    let Some(snapshot) = field.snapshot.as_deref() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let build_row = |build: Option<&str>, rows: &mut Vec<(String, String)>| {
        if let Some(build) = build {
            let mut words = build.to_owned();
            if seat.needs_update {
                words.push_str(" · needs update");
            } else if let Some(prevailing) =
                field.prevailing.as_deref().filter(|_| seat.other_build)
            {
                words.push_str(&format!(" · not the fleet's {prevailing}"));
            }
            rows.push(("Build".into(), words));
        }
    };
    match seat.src {
        SeatSrc::Unanswered(i) => {
            let u = &snapshot.unanswered[i];
            rows.push(("Machine".into(), format!("{} · {}", seat.name, u.dial)));
            let mut presence = reach_words(u.reason).to_owned();
            if let Some(since) = u.silent_since_ms {
                presence.push_str(&format!(
                    " · silent {}",
                    aterm_netplace::words::age_words(field.now_ms.saturating_sub(since))
                ));
            }
            if !u.why.is_empty() {
                presence.push_str(&format!(" · {}", u.why));
            }
            rows.push(("Presence".into(), presence));
            build_row(u.build.as_deref(), &mut rows);
        }
        SeatSrc::Mac(i) => {
            let mac = &snapshot.macs[i];
            let mut machine = format!("{} · {}", seat.name, mac.dial);
            if let Some(node) = &mac.node {
                machine.push_str(&format!(" · node {node}"));
            }
            rows.push(("Machine".into(), machine));
            rows.push((
                "Presence".into(),
                match seat.presence {
                    Presence::Carried => {
                        format!(
                            "carried: its last answer, {}",
                            time_and_age(mac.read_at_ms, field.now_ms)
                        )
                    }
                    Presence::Silent => format!(
                        "not answering; its last answer {} no longer counts",
                        time_and_age(mac.read_at_ms, field.now_ms)
                    ),
                    _ => format!("answered at {}", time_and_age(mac.read_at_ms, field.now_ms)),
                },
            ));
            build_row(mac.build.as_deref(), &mut rows);
            #[cfg(unix)]
            if let Some(tables) = mac.tables {
                rows.push(("Tables".into(), behind_words(tables)));
            }
            rows.push((
                "Agents".into(),
                if mac.older {
                    "not read: older aterm".into()
                } else if mac.unproven {
                    "not counted: unproven".into()
                } else {
                    match mac.activity {
                        None => activity_absent_words(mac),
                        Some(t) => {
                            let least = if mac.left_out > 0 || mac.instances_unread > 0 {
                                "at least "
                            } else {
                                ""
                            };
                            format!(
                                "{least}{} agents: {} busy, {} idle, {} waiting, {} walled, {} unknown",
                                t.agents, t.busy, t.idle, t.waiting, t.walled, t.unknown
                            )
                        }
                    }
                },
            ));
            if !mac.agent_sessions.is_empty() {
                let mut lines: Vec<String> = mac
                    .agent_sessions
                    .iter()
                    .map(|s| {
                        format!(
                            "{} · {} · {} · {} · ctx {} · 5h/7d {}/{}",
                            s.sid,
                            s.program.as_deref().unwrap_or("–"),
                            agent_word(&s.agent),
                            s.task.map_or("–", task_word),
                            pct(s.ctx_left_pct),
                            pct(s.usage_5h_pct),
                            pct(s.usage_7d_pct),
                        )
                    })
                    .collect();
                if mac.sessions_more > 0 {
                    lines.push(format!("{} more", mac.sessions_more));
                }
                rows.push(("Sessions".into(), lines.join("; ")));
            }
            if !mac.needs.is_empty() {
                rows.push((
                    "Needs".into(),
                    mac.needs
                        .iter()
                        .map(|n| format!("{} · {} · {}", n.sid, n.who, n.word.word()))
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
            if !mac.held_here.is_empty() {
                rows.push((
                    "Held here".into(),
                    mac.held_here
                        .iter()
                        .map(|h| format!("{} · {}", h.sid, h.who))
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
            if !mac.managers.is_empty() {
                rows.push((
                    "Managers".into(),
                    mac.managers
                        .iter()
                        .map(|m| format!("{} · {} workers", m.label, m.workers))
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
            rows.push((
                "Rows read at".into(),
                super::observation_time(mac.read_at_ms),
            ));
        }
        SeatSrc::Peer(_) => {}
    }
    rows
}

/// The fact that puts `seat` in `class`.
fn class_fact(field: &Field, seat: &SeatMark, class: SeatClass) -> String {
    match class {
        SeatClass::OtherBuild => field
            .snapshot
            .as_deref()
            .and_then(|s| match seat.src {
                SeatSrc::Mac(i) => s.macs[i].build.clone(),
                SeatSrc::Unanswered(i) => s.unanswered[i].build.clone(),
                SeatSrc::Peer(_) => None,
            })
            .unwrap_or_default(),
        SeatClass::Busy => match seat.activity {
            Some(Activity::Busy(n)) => format!("{n} busy"),
            _ => String::new(),
        },
        _ => class.word().to_owned(),
    }
}

fn group_details(field: &Field, class: SeatClass) -> Vec<(String, String)> {
    let mut rows = vec![(
        "Group".to_owned(),
        format!(
            "{} {}",
            field.seats.iter().filter(|s| s.is(class)).count(),
            class.word()
        ),
    )];
    rows.extend(
        field
            .seats
            .iter()
            .filter(|s| s.is(class))
            .map(|s| (s.name.clone(), class_fact(field, s, class))),
    );
    rows
}

/// The strip's accessibility value: its seats by class.
pub(crate) fn strip_words(field: &Field) -> String {
    [
        SeatClass::Answered,
        SeatClass::Carried,
        SeatClass::Silent,
        SeatClass::NotCounted,
    ]
    .into_iter()
    .filter_map(|class| {
        let names: Vec<&str> = field
            .seats
            .iter()
            .filter(|s| s.is(class))
            .map(|s| s.name.as_str())
            .collect();
        (!names.is_empty()).then(|| format!("{} {}", class.word(), names.join(", ")))
    })
    .collect::<Vec<_>>()
    .join("; ")
}

/// The Local lens's `Observed machines` rows: the observer's peers apart from the fleet.
pub(crate) fn observed_rows(obs: &NetworkObservation) -> Vec<(String, String)> {
    let reachable = obs
        .peers
        .iter()
        .filter(|p| {
            super::observed_reachability(obs.visibility, p.reachability) == Reachability::Reachable
        })
        .count();
    let last_seen = obs
        .peers
        .iter()
        .filter(|p| p.reachability == Reachability::LastSeen)
        .count();
    let mut line = format!(
        "{} known · {reachable} reachable · {last_seen} last seen",
        obs.peers.len()
    );
    if obs.unverified_endpoints > 0 {
        line.push_str(&format!(
            " · {} unverified endpoints; no machine identity or data access proved",
            obs.unverified_endpoints
        ));
    }
    let mut rows = vec![("Observed machines".to_owned(), line)];
    let mut peers: Vec<_> = obs
        .peers
        .iter()
        .filter(|p| {
            super::observed_reachability(obs.visibility, p.reachability) != Reachability::Reachable
        })
        .collect();
    peers.sort_by_key(|p| p.slot);
    let rest = peers.len().saturating_sub(LOCAL_ROWS_MAX);
    for peer in peers.into_iter().take(LOCAL_ROWS_MAX) {
        let seen = format!("last seen {}", super::observation_time(peer.last_seen_ms));
        let mut words = match super::observed_reachability(obs.visibility, peer.reachability) {
            Reachability::Unknown => format!("not checked now · {seen}"),
            Reachability::LastSeen | Reachability::Reachable => seen,
        };
        if let Some(repair) = &peer.last_repair {
            words.push_str(&format!(
                " · repair: {}",
                super::repair::outcome_words(repair.outcome)
            ));
        }
        rows.push((super::machine_name(&peer.identity), words));
    }
    if rest > 0 {
        rows.push(("More".into(), format!("{rest} more not reachable")));
    }
    if reachable > 0 {
        rows.push(("Reachable".into(), "the rest reachable".into()));
    }
    rows
}
