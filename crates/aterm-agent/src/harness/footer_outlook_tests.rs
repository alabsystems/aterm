// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The footer's account unit under the accounts outlook (`docs/DESIGN-accounts-outlook-2026-10-25.md`
//! §2): who · this week · this 5h · outlook, its ladder and its spans.

use super::*;
use crate::harness::outlook::{BarCell, Glance, VerdictWords};
use crate::harness::profile::{SubscriptionDisplay, WeeklyGap};

fn cells(used: usize, more: usize, tick: bool) -> [BarCell; 10] {
    std::array::from_fn(|i| match i {
        i if i < used => BarCell::Used,
        i if i < used + more => BarCell::More,
        i if tick && i == used + more => BarCell::Tick,
        _ => BarCell::Left,
    })
}

/// A PAUSED RECOVERY KEEPS OBSERVATIONS WITHOUT FORECAST SEGMENTS on today's footer: a paused or
/// busy recovery keeps the glance's observations and drops `OUTLOOK_MARK` and every forecast
/// meter cell from the live segments. It stays beside the quota view's
/// `a_paused_recovery_shows_no_forecast` until S5 deletes this file with the glance footer, since
/// the footer it guards is live until then. Negative control: `fact_segments` reading
/// `facts.glance` in place of `facts.glance_view()` keeps `OUTLOOK_MARK` under a pause.
#[test]
fn paused_recovery_keeps_observations_without_forecast_segments() {
    use crate::harness::account_progress::{AccountProgress, ProgressPhase};
    for name in ['A', 'F'] {
        let mut facts = scene(name);
        let raw = facts.glance.clone().unwrap();
        let subscription = facts.subscription.clone();
        assert!(
            segments(&facts)
                .iter()
                .any(|segment| segment.mark == OUTLOOK_MARK)
        );
        for (phase, paused, busy) in [
            (ProgressPhase::Paused, false, false),
            (ProgressPhase::Paused, false, true),
            (ProgressPhase::Waiting, false, true),
            (ProgressPhase::Unknown, false, true),
            (
                ProgressPhase::Switching {
                    to: "alternate".into(),
                },
                true,
                false,
            ),
        ] {
            let mut progress = AccountProgress::unknown(100);
            progress.phase = phase.clone();
            progress.forecasts_paused = paused;
            facts.account_progress = Some(progress);
            facts.account_progress_busy = busy;
            let known_pause = paused || phase == ProgressPhase::Paused;
            assert_eq!(
                facts.account_forecasts_paused(),
                known_pause,
                "{phase:?}, busy={busy}"
            );
            assert_eq!(
                facts.account_forecast_refusal(),
                Some(if known_pause {
                    crate::harness::outlook::Refusal::SessionPaused
                } else {
                    crate::harness::outlook::Refusal::ObserverBusy
                })
            );
            let view = facts.glance_view().unwrap();
            assert_eq!((&view.reset, &view.five), (&raw.reset, &raw.five));
            assert!(view.verdict.is_none() && view.title_verdict.is_none());
            assert!(view.title.is_none() && view.spoken.is_none());
            if let (Some((ten, five)), Some((raw_ten, raw_five))) = (view.bar, raw.bar) {
                for (shown, observed) in
                    ten.iter().chain(&five).zip(raw_ten.iter().chain(&raw_five))
                {
                    assert_eq!(*shown == BarCell::Used, *observed == BarCell::Used);
                    assert!(matches!(shown, BarCell::Used | BarCell::Left));
                }
            }
            let segments = segments(&facts);
            assert!(segments.iter().all(|segment| segment.mark != OUTLOOK_MARK));
            assert!(
                segments
                    .iter()
                    .filter_map(|segment| segment.meter.as_ref())
                    .all(|meter| {
                        meter
                            .drawn()
                            .iter()
                            .all(|cell| matches!(cell, BarCell::Used | BarCell::Left))
                    })
            );
            assert_eq!(facts.subscription, subscription);
            assert_eq!(facts.glance.as_ref(), Some(&raw));
        }
        facts.account_progress_busy = false;
        for phase in [ProgressPhase::Unknown, ProgressPhase::Disabled] {
            let mut progress = AccountProgress::unknown(101);
            progress.phase = phase;
            facts.account_progress = Some(progress);
            assert!(!facts.account_forecasts_paused());
            assert_eq!(facts.account_forecast_refusal(), None);
            assert_eq!(facts.glance_view().as_deref(), Some(&raw));
            assert!(
                segments(&facts)
                    .iter()
                    .any(|segment| segment.mark == OUTLOOK_MARK)
            );
        }
    }
}

fn half(ten: [BarCell; 10]) -> [BarCell; 5] {
    std::array::from_fn(|i| ten[i * 2 + 1].max_by_draw(ten[i * 2]))
}

trait Draw {
    fn max_by_draw(self, other: Self) -> Self;
}

impl Draw for BarCell {
    /// The heavier of two cells, as a bar halves (`━` over `╍` over `┽` over `─`).
    fn max_by_draw(self, other: Self) -> Self {
        let rank = |c: BarCell| match c {
            BarCell::Used => 3,
            BarCell::More => 2,
            BarCell::Tick => 1,
            _ => 0,
        };
        if rank(self) >= rank(other) {
            self
        } else {
            other
        }
    }
}

fn words(full: &str, short: &str, shortest: Option<&str>, warn: bool) -> VerdictWords {
    VerdictWords {
        full: full.into(),
        short: short.into(),
        shortest: shortest.map(str::to_owned),
        warn,
        kept_narrow: warn || full.starts_with('\u{2192}'),
    }
}

/// Scenario `name` of §2.4 (A covered, B a gap, C an alt unread, D a 5-hour stop, E no pace yet,
/// F walled now, G an aged figure, H no figure) as the resolver hands it to the footer, for the
/// account `email`.
fn scene_for(name: char, email: &str) -> FooterFacts {
    let bar = |ten: [BarCell; 10]| Some((ten, half(ten)));
    let glance = |ten, five: &str, verdict| Glance {
        bar: bar(ten),
        reset: Some("Thu 09:00".into()),
        five: Some(five.into()),
        verdict,
        title: Some("58% used, ~98% by its reset at today's pace".into()),
        title_verdict: None,
        spoken: None,
    };
    let continuous = words("continuous 7d", "continuous 7d", None, false);
    let (weekly, gap, wall, glance) = match name {
        'A' => (
            "weekly 42% left",
            None,
            false,
            glance(cells(6, 3, true), "5h 62% left", Some(continuous)),
        ),
        'B' => (
            "weekly 42% left",
            None,
            false,
            glance(
                cells(6, 4, false),
                "5h 62% left",
                Some(words(
                    "\u{2573} ~Wed 22:00\u{2013}Thu 09:00",
                    "\u{2573} ~Wed 22:00",
                    Some("\u{2573} ~Wed 22:00"),
                    true,
                )),
            ),
        ),
        'C' => (
            "weekly 42% left",
            None,
            false,
            glance(
                cells(6, 4, false),
                "5h 62% left",
                Some(words(
                    "continuous to ~Wed 16:30, then pool1 ?",
                    "to ~Wed 16:30 ?",
                    None,
                    false,
                )),
            ),
        ),
        'D' => (
            "weekly 42% left",
            None,
            false,
            glance(
                cells(6, 3, true),
                "5h 19% left",
                Some(words(
                    "\u{2573} ~20:10\u{2013}21:10",
                    "\u{2573} ~20:10",
                    Some("\u{2573} ~20:10"),
                    true,
                )),
            ),
        ),
        'E' => (
            "weekly 97% left",
            None,
            false,
            Glance {
                reset: Some("Sun 1 Nov 18:10".into()),
                verdict: None,
                ..glance(cells(1, 0, false), "5h 62% left", None)
            },
        ),
        'F' => (
            "",
            None,
            true,
            Glance {
                reset: None,
                five: None,
                ..glance(
                    cells(10, 0, false),
                    "",
                    Some(words(
                        "\u{2192} alt1 at turn end",
                        "\u{2192} alt1",
                        Some("\u{2192} alt1"),
                        false,
                    )),
                )
            },
        ),
        'G' => (
            "weekly 42% left \u{b7} 16m ago",
            None,
            false,
            glance(
                cells(6, 3, true),
                "5h 62% left",
                Some(words("continuous 7d", "continuous 7d", None, false)),
            ),
        ),
        // A glance whose bar came from an older store figure: the week's own words have no
        // figure, so no bar may be drawn from it.
        _ => (
            "",
            Some(WeeklyGap::Untapped),
            false,
            Glance {
                five: None,
                verdict: None,
                ..glance(cells(5, 4, true), "", None)
            },
        ),
    };
    let base = if wall {
        narrow_rule_walled()
    } else {
        narrow_rule_facts()
    };
    FooterFacts {
        subscription: Some(SubscriptionDisplay {
            email: Some(email.into()),
            plan: Some("Max 20x".into()),
            weekly: (!weekly.is_empty()).then(|| weekly.to_owned()),
            weekly_gap: gap,
            ..Default::default()
        }),
        glance: Some(glance),
        ..base
    }
}

fn scene(name: char) -> FooterFacts {
    scene_for(name, "me@example.test")
}

const SCENES: [char; 8] = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];

/// An account whose name alone outruns the narrow rungs, so the ladder cuts it.
const LONG: &str = "release.maintainer.of.the.week@example.test";

fn shows(rung: &[Segment], mark: char) -> bool {
    rung.iter().any(|s| s.mark == mark)
}

fn meter_of(rung: &[Segment]) -> Option<Meter> {
    rung.iter().find_map(|s| s.meter)
}

fn reset_shown(rung: &[Segment]) -> bool {
    rung.iter()
        .any(|s| s.mark == WEEKLY_MARK && s.text.contains(RESET_MARK))
}

/// The account's name cut to a width (`release.mai…`).
fn email_cut(rung: &[Segment]) -> bool {
    rung.iter()
        .any(|s| s.mark == ACCOUNT_MARK && s.text.ends_with('\u{2026}'))
}

/// T18: THE LADDER'S INVARIANTS (§2.3) over every scenario A-H and every room from 0 to 200:
/// each fit fits its room; before the wall rungs, a part shown at rung k+1 is shown at rung k;
/// no rung shows a week, a bar, a 5-hour part or a verdict without the account; a stop or
/// a wall verdict shows on every rung that shows the week; the bar is gone wherever the email is
/// cut; no bar is drawn from a week with no figure (H); the bar's cells sit on whole cells at
/// pitch 1 with no nudge. Negative controls: the meter set at `FACT_PITCH`; a bar drawn from a
/// week with no figure; a ladder that cuts the email before it drops the bar.
#[test]
fn the_units_ladder_keeps_its_invariants() {
    let mut cut = 0;
    for (name, email) in SCENES
        .into_iter()
        .flat_map(|name| [(name, "me@example.test"), (name, LONG)])
    {
        let facts = scene_for(name, email);
        let ladder = subscription_ladder(&facts);
        let walls_at = ladder
            .iter()
            .position(|rung| rung.len() == 1 && rung[0].mark == WALL_MARK)
            .unwrap_or(ladder.len());
        let verdict_kept = facts
            .glance
            .as_ref()
            .and_then(|g| g.verdict.as_ref())
            .is_some_and(|v| v.kept_narrow);
        for (k, rung) in ladder.iter().enumerate() {
            let account = shows(rung, ACCOUNT_MARK);
            for part in [WEEKLY_MARK, FIVE_MARK, OUTLOOK_MARK] {
                assert!(
                    account || !shows(rung, part),
                    "{name} rung {k}: {part} without the account: {rung:?}"
                );
            }
            if verdict_kept && shows(rung, WEEKLY_MARK) {
                assert!(
                    shows(rung, OUTLOOK_MARK),
                    "{name} rung {k}: a stop or wall verdict kept with the week: {rung:?}"
                );
            }
            if email_cut(rung) {
                cut += 1;
                assert!(
                    meter_of(rung).is_none(),
                    "{name} rung {k}: the bar is gone where the email is cut: {rung:?}"
                );
            }
            if name == 'H' {
                assert!(meter_of(rung).is_none(), "H rung {k}: a bar with no figure");
            }
            let set = set_facts(rung);
            let meter: Vec<&SetGlyph> = set.glyphs.iter().filter(|g| g.meter).collect();
            assert_eq!(
                meter.len(),
                meter_of(rung).map_or(0, |m| m.drawn().len()),
                "{name} rung {k}: every drawn cell set"
            );
            for pair in meter.windows(2) {
                assert_eq!(
                    pair[1].cell,
                    pair[0].cell + 1,
                    "{name} rung {k}: whole cells"
                );
            }
            assert!(
                meter.iter().all(|g| g.nudge == 0 && g.cols == 1),
                "{name} rung {k}: no nudge"
            );
            if k + 1 < walls_at {
                let next = &ladder[k + 1];
                for part in [
                    ACCOUNT_MARK,
                    WEEKLY_MARK,
                    FIVE_MARK,
                    OUTLOOK_MARK,
                    MODEL_MARK,
                ] {
                    assert!(
                        shows(rung, part) || !shows(next, part),
                        "{name} rung {k}: {part} comes back at rung {}",
                        k + 1
                    );
                }
                assert!(meter_of(rung).is_some() || meter_of(next).is_none());
                assert!(reset_shown(rung) || !reset_shown(next));
                assert!(
                    meter_of(rung).map_or(0, |m| m.n) >= meter_of(next).map_or(0, |m| m.n),
                    "{name} rung {k}: the bar only narrows"
                );
            }
        }
        if !matches!(name, 'F' | 'H') {
            assert!(
                meter_of(&ladder[0]).is_some(),
                "CONTROL {name}: the widest rung draws its bar"
            );
        }
        for room in 0..=200 {
            let fit = fit_rule(&facts, room, None);
            assert!(
                fit.segments.is_empty() || fit.width(None) <= room,
                "{name} at {room}: {fit:?}"
            );
        }
    }
    assert!(cut > 0, "CONTROL: some rung cuts the email");
}

/// THE OUTLOOK NEVER COSTS THE ACCOUNT: in every scenario A-H, for either email and every room
/// from 0 to 200, a rule that names the account with no outlook names it with the outlook, and
/// a stop kept narrow outlasts the week's figure. Negative control: the ladder's last two rungs
/// (the account with the stop, then alone) removed, the stop scenes at a narrow room lose the
/// account.
#[test]
fn the_outlook_never_costs_the_account() {
    use crate::harness::account_progress::{AccountProgress, ProgressPhase};
    let mut narrow_stops = 0;
    for observed in [false, true] {
        for (name, email) in SCENES
            .into_iter()
            .flat_map(|name| [(name, "me@example.test"), (name, LONG)])
        {
            let mut facts = scene_for(name, email);
            if observed {
                let mut progress = AccountProgress::unknown(100);
                progress.phase = ProgressPhase::Waiting;
                progress.wall = Some("7d".into());
                facts.account_progress = Some(progress);
            }
            let bare = FooterFacts {
                glance: None,
                ..facts.clone()
            };
            for room in 0..=200 {
                let without = fit_rule(&bare, room, None);
                let with = fit_rule(&facts, room, None);
                assert!(
                    !shows(&without.segments, ACCOUNT_MARK) || shows(&with.segments, ACCOUNT_MARK),
                    "{name} {email} observed={observed} at {room}: the outlook pushed the account out: {with:?} (no \
                 outlook: {without:?})"
                );
                // A projected stop's words lead with `╳` (`⧗` is a wall standing now).
                let stop = with
                    .segments
                    .iter()
                    .any(|s| s.mark == OUTLOOK_MARK && s.text.starts_with('\u{2573}'));
                if stop && !shows(&with.segments, WEEKLY_MARK) {
                    narrow_stops += 1;
                }
            }
        }
    }
    assert!(
        narrow_stops > 0,
        "CONTROL: some room keeps a stop beside the account alone"
    );
}

/// T19: A WEEK THE BAR SPLITS KEEPS THE FACTS' SPANS: the words after the bar continue the week
/// with no mark drawn ([`Segment::attached`]), so the marks after it still name their own facts;
/// the model's cells stay `Model`'s and the whole unit (the bar, the 5 hours and the verdict
/// included) is one `Account` span. The plan is off the rule at rest, so the design's attached
/// plan is gone and the split week is the one attached part left. Negative control: the words
/// after the bar drawn behind a mark of their own shift the model into the account.
#[test]
fn a_split_week_keeps_the_fact_spans() {
    for name in ['A', 'B', 'D'] {
        let facts = scene(name);
        let segs = segments(&facts);
        assert!(
            segs.iter()
                .any(|s| s.mark == WEEKLY_MARK && s.meter.is_some())
        );
        assert!(
            segs.iter().all(|s| !s.text.contains("Max 20x")),
            "{name}: the plan is off the rule at rest"
        );
        let set = set_facts(&segs);
        let spans = fact_spans(&set, &segs);
        let kinds: Vec<FactKind> = spans.iter().flatten().map(|s| s.kind).collect();
        assert_eq!(kinds[0], FactKind::Account, "{name}: {kinds:?}");
        assert_eq!(kinds[1], FactKind::Model, "{name}: {kinds:?}");
        let unit = spans[0].expect("the unit");
        let model = spans[1].expect("the model");
        let in_span = |span: FactSpan, g: &SetGlyph| {
            (usize::from(span.cells.0)..usize::from(span.cells.1)).contains(&g.cell)
        };
        let unit_text: String = set
            .glyphs
            .iter()
            .filter(|g| in_span(unit, g))
            .map(|g| g.ch)
            .collect();
        for part in ["me", "weekly", "5h", "left"] {
            assert!(
                unit_text.contains(part),
                "{name}: {part} in the unit's span: {unit_text}"
            );
        }
        assert!(
            set.glyphs
                .iter()
                .filter(|g| g.meter)
                .all(|g| in_span(unit, g)),
            "{name}: the bar is the unit's"
        );
        let model_text: String = set
            .glyphs
            .iter()
            .filter(|g| in_span(model, g) && !g.mark)
            .map(|g| g.ch)
            .collect();
        assert!(model_text.starts_with("Opus5.5"), "{name}: {model_text}");
    }
}
