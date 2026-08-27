// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATUS BARS — two thin, full-width, single-row chrome bands at the top of
//! every window: one for the ALab toolchain that `atpkg` is installing, one for
//! aterm's own self-update. Each bar exists ONLY while its lane has something to
//! say; it takes one terminal row while it is up (the grid is re-fitted under it,
//! exactly as the in-grid tab strip reserves its rows) and folds away when the
//! work ends, giving the row back.
//!
//! # Why rows, not an overlay
//!
//! The owner's brief (docs/design/STATUS-SURFACE.md, refined 2026-08-26): *"two
//! narrow status bars all the way across the top … push down the other windows
//! and when it's done fold up and the space expands … without overlaying the
//! terminal itself — the overlay blocks use of the terminal."* A floating card
//! covers cells a program may be painting and eats the presses that land on it;
//! a chrome ROW covers nothing — the terminal is exactly as usable with the bar
//! up as with it down, one row shorter. So the bars are composed-frame rows
//! (see `App::splice_tab_strip_with`), never a card in the notice slot.
//!
//! # What the bars may claim
//!
//! * The toolchain bar renders ONLY what [`crate::PkgProgressSnapshot`] supports
//!   (the classified read of `<prefix>/progress.json`): a not-running snapshot
//!   names its terminal outcome and never a live phase; an unknown schema `v`
//!   is one generic "Installing packages…" line. Program names are untrusted
//!   until they round-trip [`atpkg::store::ToolName`]; error text is
//!   control-stripped and capped ([`atpkg::progress::sanitize_for_tty`]).
//! * The update bar renders [`aterm_update::Progress`] as the updater reported it
//!   from inside its own check — download bytes are the `.part` file's size
//!   against the release asset's declared size (0 ⇒ no meter, honestly).
//!
//! # Lifecycle, and why a bar can never get stuck
//!
//! A bar opens on a lane's first report and stays while the lane is LIVE. Every
//! terminal report (installed / failed / staged / …) arms a fold deadline
//! ([`HOLD_OK`] for good news, [`HOLD_WARN`] for bad — long enough to read, short
//! enough that an ignored failure does not keep a row forever; the durable record
//! is Settings ▸ Packages / Settings ▸ Software Update, which a click on the bar
//! opens). [`StatusBars::settle`] retires expired bars; the App folds
//! [`StatusBars::deadline`] into its single `about_to_wait` deadline so the fold
//! happens on time and costs no idle wake otherwise (FL-1: hidden bars have a
//! `0` fingerprint and no deadline — byte-identical to the no-bar path).
//!
//! # Structure
//!
//! Everything here is PURE: [`StatusBars`] is the state machine (inputs are the
//! lane reports, a clock is injected), [`paint_rows`] turns it into
//! [`RenderCell`] rows at a width, and [`layout`] is the width law between them —
//! all three unit-test without a window.

use std::time::{Duration, Instant};

use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::Theme;

use crate::chrome_band::{self, BandColors};
use crate::settings::{blank_row, write_str};

use atpkg::progress::{PROGRESS_VERSION, Phase, sanitize_for_tty};

/// How long a bar holds a GOOD terminal outcome before folding.
pub(crate) const HOLD_OK: Duration = Duration::from_secs(8);
/// How long a bar holds a BAD terminal outcome (failed / partial / unusable /
/// stopped) before folding. Longer than [`HOLD_OK`] because the user has to
/// read a sentence and may want to click through; bounded because a status bar
/// that never leaves is the floating card's mistake in a different shape.
pub(crate) const HOLD_WARN: Duration = Duration::from_secs(45);
/// How long a LIVE toolchain bar fed by the progress tailer may go without a
/// report before it folds quietly. The tailer posts every heartbeat change
/// (≤ 2 s apart while atpkg is alive) and one final read at child exit, so a
/// silence this long means the tailer is gone — a foreign writer the GUI cannot
/// tail, a wedged child — and the bar has nothing honest left to say.
const TAILED_STALE: Duration = Duration::from_secs(30);
/// The cap on an ANNOUNCEMENT-only toolchain bar (`seed-starting:` with no
/// progress file to tail — no store layout): a terminal marker normally answers
/// it; if none ever comes, it folds after the same hold the old pill had.
const ANNOUNCE_STALE: Duration = Duration::from_secs(20 * 60);
/// The cap on a LIVE update bar. The updater's download poller reports only on
/// size CHANGE and its verify phase (codesign / Gatekeeper) can sit silent for
/// tens of seconds, so this is long — but a check is bounded by curl's own
/// `--max-time` and always ends in a `Staged` / `Failed` / `Deferred` report
/// from the same process, so this only ever fires for a process that died.
const UPDATE_STALE: Duration = Duration::from_secs(15 * 60);
/// The meter's preferred width in cells, and the narrowest it is drawn at all.
const METER_PREFERRED: usize = 20;
const METER_MIN: usize = 8;
/// Left / right margin cells so the text never touches the window edge.
const MARGIN: usize = 1;
/// Cap on a sanitized failure reason inside a bar.
const ERROR_CAP: usize = 60;
/// Cap on a program name inside a bar (the store's own names are short).
const NAME_CAP: usize = 24;

/// Which bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// The ALab toolchain install (`atpkg seed` / `atpkg update`).
    Toolchain,
    /// aterm's own self-update (download → verify → staged).
    Update,
}

/// The bar's colour mood — information, a good end, or something the user
/// should look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Info,
    Success,
    Warn,
}

/// The words on one bar, in the order they are laid out. Pure data so the
/// layout law and the painter are testable on literal values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BarText {
    /// The leading pictogram (⇣ ↻ ✓ ⚠ ⏸).
    pub glyph: char,
    /// Bold, always shown (truncated only on an absurdly narrow window).
    pub title: String,
    /// Secondary text after the title; the first thing dropped when narrow.
    pub detail: String,
    /// Right-aligned figures ("512 MB / 1.2 GB · 3 of 10"); dropped before
    /// the meter is, since the meter says the same thing at a glance.
    pub stats: String,
    pub tone: Tone,
}

/// One live bar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bar {
    pub text: BarText,
    /// Determinate fill `0..=1`, or `None` — no meter (unknown total, or a
    /// phase that is not a byte stream).
    pub fill: Option<f32>,
    /// When this bar folds on its own; `None` while its lane is live.
    pub fold_at: Option<Instant>,
    /// For a LIVE bar: when it folds if no further report arrives (see
    /// [`TAILED_STALE`] and friends) — a reserved row must never outlive the
    /// process feeding it. `None` on a terminal bar (`fold_at` rules there).
    pub stale_at: Option<Instant>,
    /// The toolchain pass this bar was built from (`pass`, `started_unix`), so a
    /// live meter is clamped to its own high-water mark and never inherits a
    /// different pass's. `None` off the toolchain lane / before a snapshot.
    pub pass_id: Option<(String, u64)>,
}

impl Bar {
    fn terminal(&self) -> bool {
        self.fold_at.is_some()
    }

    /// The instant this bar leaves on its own: its hold, or its staleness cap.
    fn retires_at(&self) -> Option<Instant> {
        self.fold_at.or(self.stale_at)
    }
}

/// The two-lane state.
#[derive(Default, Debug)]
pub(crate) struct StatusBars {
    toolchain: Option<Bar>,
    update: Option<Bar>,
}

impl StatusBars {
    /// How many chrome rows the bars take right now (0, 1 or 2).
    pub(crate) fn rows(&self) -> u16 {
        u16::from(self.toolchain.is_some()) + u16::from(self.update.is_some())
    }

    /// The live bars, top to bottom: toolchain above update.
    pub(crate) fn bars(&self) -> impl Iterator<Item = (Lane, &Bar)> {
        self.toolchain
            .iter()
            .map(|b| (Lane::Toolchain, b))
            .chain(self.update.iter().map(|b| (Lane::Update, b)))
    }

    /// Which lane occupies bar row `index` (0 = topmost), if any.
    pub(crate) fn lane_at(&self, index: usize) -> Option<Lane> {
        self.bars().nth(index).map(|(lane, _)| lane)
    }

    /// Retire every bar whose hold has elapsed — or whose feed went silent past
    /// its staleness cap. Returns whether the ROW COUNT changed — the caller's
    /// re-grid trigger.
    pub(crate) fn settle(&mut self, now: Instant) -> bool {
        let before = self.rows();
        for slot in [&mut self.toolchain, &mut self.update] {
            if slot
                .as_ref()
                .is_some_and(|b| b.retires_at().is_some_and(|at| now >= at))
            {
                *slot = None;
            }
        }
        self.rows() != before
    }

    /// The next instant a bar leaves on its own: a held outcome's fold, or a
    /// live bar's staleness cap. `None` with no bar up.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.bars().filter_map(|(_, b)| b.retires_at()).min()
    }

    /// The repaint-key term: **exactly `0` when no bar is up** (the byte-identical
    /// no-bar key), else a nonzero FNV-1a over everything the painter reads.
    pub(crate) fn fingerprint(&self) -> u64 {
        if self.rows() == 0 {
            return 0;
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut fold = |bytes: &[u8]| {
            for &b in bytes {
                h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for (lane, bar) in self.bars() {
            fold(&[lane as u8, bar.text.tone as u8, u8::from(bar.terminal())]);
            let mut buf = [0u8; 4];
            fold(bar.text.glyph.encode_utf8(&mut buf).as_bytes());
            fold(bar.text.title.as_bytes());
            fold(&[0]);
            fold(bar.text.detail.as_bytes());
            fold(&[0]);
            fold(bar.text.stats.as_bytes());
            fold(&[0]);
            // Quantized to whole percent — the resolution the "NN%" readout
            // shows and finer than any meter the layout draws — so a byte tick
            // that cannot move a glyph does not re-present the frame. (The stats
            // string is MB-granular, so a download re-presents at most once per
            // megabyte; the tailer's 10 Hz is the ceiling either way.)
            let q = bar
                .fill
                .map_or(u32::MAX, |f| (f.clamp(0.0, 1.0) * 100.0) as u32);
            fold(&q.to_le_bytes());
        }
        h | 1
    }

    // ---- the toolchain lane -------------------------------------------------

    /// `atpkg` announced a pass (`seed-starting:` / `net-starting:`) — the bar
    /// opens NOW, before `progress.json` exists, because the extraction that
    /// follows is minutes long and gigabytes wide and an app doing that silently
    /// is indistinguishable from one that is misbehaving. `detail` is atpkg's
    /// own sentence; its trailing "(…)" carries the size, which the bar keeps.
    pub(crate) fn toolchain_announced(&mut self, detail: &str, now: Instant) {
        let size = detail
            .rsplit_once('(')
            .and_then(|(_, t)| t.strip_suffix(')'))
            .map(|s| sanitize_for_tty(s, 40))
            .filter(|s| !s.is_empty());
        self.toolchain = Some(Bar {
            text: BarText {
                glyph: '\u{21e3}',
                title: "Installing the ALab toolchain".to_string(),
                detail: size
                    .map_or_else(|| "starting…".to_string(), |s| format!("{s} — starting…")),
                stats: String::new(),
                tone: Tone::Info,
            },
            fill: None,
            fold_at: None,
            stale_at: Some(now + ANNOUNCE_STALE),
            pass_id: None,
        });
    }

    /// One classified `progress.json` read from the child-scoped tailer
    /// (`Wake::PkgProgress`). `None` = the file vanished at child exit.
    ///
    /// A snapshot may NOT overwrite a terminal outcome a marker already posted
    /// (`installed` / `failed` / …): atpkg prints its markers AFTER `end_pass`,
    /// and the tailer's final read is posted after the marker line was read, so
    /// the marker's richer sentence arrives first and must win. The meter still
    /// completes.
    pub(crate) fn toolchain_snapshot(
        &mut self,
        snap: Option<&crate::PkgProgressSnapshot>,
        now: Instant,
    ) {
        let Some(snap) = snap else {
            // No data: a live bar without its file is a bar with nothing honest
            // to say — unless a terminal marker already gave it its last words.
            if self.toolchain.as_ref().is_some_and(|b| !b.terminal()) {
                self.toolchain = None;
            }
            return;
        };
        let f = &snap.file;
        if self.toolchain.as_ref().is_some_and(Bar::terminal) {
            if let Some(bar) = self.toolchain.as_mut()
                && bar.fill.is_some()
            {
                bar.fill = Some(1.0);
            }
            return;
        }
        if f.v != PROGRESS_VERSION {
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: '\u{21e3}',
                    title: "Installing packages…".to_string(),
                    detail: "a newer aterm is writing this progress format".to_string(),
                    stats: String::new(),
                    tone: Tone::Info,
                },
                fill: None,
                fold_at: if snap.running {
                    None
                } else {
                    Some(now + HOLD_OK)
                },
                stale_at: snap.running.then_some(now + TAILED_STALE),
                pass_id: None,
            });
            return;
        }
        let title = match f.pass.as_str() {
            "seed" => "Preparing the ALab toolchain",
            "net" => "Installing the ALab toolchain",
            _ => "Installing packages",
        };
        let total = f.overall.programs_total;
        let done = f.overall.programs_done;
        // THE NO-OP PASS STAYS INVISIBLE. atpkg begins its "net" pass BEFORE the
        // signed index resolves — at every launch and every 6 h — and the common
        // outcome is a plan of zero programs. A bar that appears for that,
        // re-grids every window, says "nothing to do" and re-grids again is the
        // exact churn the owner's brief rules out; with no plan there is nothing
        // to show, and an announced bar (a pass that SAID it would install) keeps
        // its announcement until the plan lands.
        let planned = total > 0 || !f.programs.is_empty() || !f.queue.is_empty();
        if !planned {
            if !snap.running {
                // Ended having planned nothing: fold whatever was up, quietly.
                self.toolchain = None;
            }
            return;
        }
        if snap.running {
            let (detail, row_fill) = current_program_line(f);
            // A LIVE METER NEVER RUNS BACKWARDS within one pass. atpkg's `.part`
            // poller reports 0 for the instant between curl promoting the file
            // and the watch stopping, and a program's credit can dip for the
            // length of its verify phase (found by the 2026-08-26 progress-model
            // survey) — clamp to this pass's own high-water mark. A different
            // pass (identity: pass name + start stamp) starts fresh.
            let pass_id = Some((f.pass.clone(), f.started_unix));
            let peak = self
                .toolchain
                .as_ref()
                .filter(|b| !b.terminal() && b.pass_id == pass_id)
                .and_then(|b| b.fill);
            let fill = overall_fill(f)
                .or(row_fill)
                .map(|x| peak.map_or(x, |p| x.max(p)));
            let mut stats = String::new();
            if total > 0 {
                stats = format!("{done} of {total}");
            }
            if f.overall.bytes_total > 0 {
                if !stats.is_empty() {
                    stats.push_str(" · ");
                }
                stats.push_str(&format!(
                    "{} / {}",
                    fmt_bytes(f.overall.bytes_done.min(f.overall.bytes_total)),
                    fmt_bytes(f.overall.bytes_total)
                ));
            }
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: '\u{21e3}',
                    title: title.to_string(),
                    detail,
                    stats,
                    tone: Tone::Info,
                },
                fill,
                fold_at: None,
                stale_at: Some(now + TAILED_STALE),
                pass_id,
            });
            return;
        }
        // NOT RUNNING: only a terminal outcome may be claimed (design §3).
        let failed = f
            .programs
            .values()
            .filter(|r| r.phase == Phase::Failed)
            .count();
        if f.ended_unix.is_some() {
            let (detail, tone, hold) = if failed == 0 {
                (
                    if total > 0 {
                        format!("all {total} installed")
                    } else {
                        "nothing to do".to_string()
                    },
                    Tone::Success,
                    HOLD_OK,
                )
            } else {
                (
                    format!(
                        "{} of {total} installed — {failed} failed · see Settings ▸ Packages",
                        done.saturating_sub(u32::try_from(failed).unwrap_or(u32::MAX))
                    ),
                    Tone::Warn,
                    HOLD_WARN,
                )
            };
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: if failed == 0 { '\u{2713}' } else { '\u{26a0}' },
                    title: title.to_string(),
                    detail,
                    stats: String::new(),
                    tone,
                },
                fill: (total > 0).then_some(1.0),
                fold_at: Some(now + hold),
                stale_at: None,
                pass_id: None,
            });
        } else {
            // A live-looking file whose writer is gone (dead pid / stale
            // heartbeat): say so, name the next act, and never animate.
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: '\u{23f8}',
                    title: title.to_string(),
                    detail: "stopped — reopen aterm or run: aterm pkg update".to_string(),
                    stats: if total > 0 {
                        format!("{done} of {total}")
                    } else {
                        String::new()
                    },
                    tone: Tone::Warn,
                },
                fill: overall_fill(f),
                fold_at: Some(now + HOLD_WARN),
                stale_at: None,
                pass_id: None,
            });
        }
    }

    /// `seed-installed:` / `net-installed:` — the toolchain is here. `text` is
    /// the same sentence the pill used to carry (roster + the "open a new tab"
    /// clause, or the shell-integration caveat), authored by the caller.
    pub(crate) fn toolchain_installed(&mut self, text: &str, now: Instant) {
        // The sentence was authored for a pill with no title of its own; the
        // bar has one, so its opening is not repeated as the detail.
        let detail = text
            .strip_prefix("\u{2713} ALab toolchain installed: ")
            .unwrap_or(text);
        self.toolchain = Some(Bar {
            text: BarText {
                glyph: '\u{2713}',
                title: "ALab toolchain installed".to_string(),
                detail: sanitize_for_tty(detail, 160),
                stats: String::new(),
                tone: Tone::Success,
            },
            fill: Some(1.0),
            fold_at: Some(now + HOLD_OK),
            stale_at: None,
            pass_id: None,
        });
    }

    /// A bad terminal outcome for the toolchain lane (`seed-partial:` /
    /// `seed-failed:` / `net-failed:` / `seed-unusable:` / the synthetic
    /// "child died after announcing"). `what` is the whole sentence.
    pub(crate) fn toolchain_failed(&mut self, what: &str, now: Instant) {
        let fill = self.toolchain.as_ref().and_then(|b| b.fill);
        self.toolchain = Some(Bar {
            text: BarText {
                glyph: '\u{26a0}',
                title: "ALab toolchain".to_string(),
                detail: sanitize_for_tty(what, 160),
                stats: String::new(),
                tone: Tone::Warn,
            },
            fill,
            fold_at: Some(now + HOLD_WARN),
            stale_at: None,
            pass_id: None,
        });
    }

    // ---- the update lane ----------------------------------------------------

    /// One report from inside the updater's own check ([`aterm_update::Progress`]).
    /// Only DOWNLOAD-and-later phases raise the bar: a check that finds nothing
    /// to do (the common case, every few minutes) must never move the grid.
    pub(crate) fn update_progress(&mut self, p: &aterm_update::Progress, now: Instant) {
        use aterm_update::Progress as P;
        let v = |version: &str| sanitize_for_tty(version, 32);
        match p {
            P::Downloading {
                version,
                bytes_done,
                bytes_total,
            } => {
                let (fill, stats) = if *bytes_total > 0 {
                    let done = (*bytes_done).min(*bytes_total);
                    (
                        Some(done as f32 / *bytes_total as f32),
                        format!("{} / {}", fmt_bytes(done), fmt_bytes(*bytes_total)),
                    )
                } else {
                    (None, fmt_bytes(*bytes_done))
                };
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{21bb}',
                        title: format!("aterm update v{}", v(version)),
                        detail: "downloading…".to_string(),
                        stats,
                        tone: Tone::Info,
                    },
                    fill,
                    fold_at: None,
                    stale_at: Some(now + UPDATE_STALE),
                    pass_id: None,
                });
            }
            P::Verifying { version } => {
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{21bb}',
                        title: format!("aterm update v{}", v(version)),
                        detail: "verifying and staging…".to_string(),
                        stats: String::new(),
                        tone: Tone::Info,
                    },
                    fill: None,
                    fold_at: None,
                    stale_at: Some(now + UPDATE_STALE),
                    pass_id: None,
                });
            }
            P::Staged { version, build } => {
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{2713}',
                        title: format!("aterm v{} is ready", v(version)),
                        detail: format!("build {build} — verified; restart aterm to apply"),
                        stats: String::new(),
                        tone: Tone::Success,
                    },
                    fill: Some(1.0),
                    fold_at: Some(now + HOLD_OK),
                    stale_at: None,
                    pass_id: None,
                });
            }
            P::Deferred { detail } => {
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{21bb}',
                        title: "aterm update deferred".to_string(),
                        detail: sanitize_for_tty(detail, 120),
                        stats: String::new(),
                        tone: Tone::Info,
                    },
                    fill: None,
                    fold_at: Some(now + HOLD_OK),
                    stale_at: None,
                    pass_id: None,
                });
            }
            P::Failed { detail } => {
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{26a0}',
                        title: "aterm update failed".to_string(),
                        detail: format!(
                            "{} · see Settings ▸ Software Update",
                            sanitize_for_tty(detail, 100)
                        ),
                        stats: String::new(),
                        tone: Tone::Warn,
                    },
                    fill: None,
                    fold_at: Some(now + HOLD_WARN),
                    stale_at: None,
                    pass_id: None,
                });
            }
        }
    }
}

/// The overall meter: the pass's download rollup, `None` when the pass planned
/// no bytes (a seed pass before its plan lands, or nothing to do).
fn overall_fill(f: &atpkg::progress::ProgressFile) -> Option<f32> {
    (f.overall.bytes_total > 0).then(|| {
        (f.overall.bytes_done.min(f.overall.bytes_total)) as f32 / f.overall.bytes_total as f32
    })
}

/// The program the pass is working on right now, as one honest phrase, plus
/// that program's own byte meter when its phase has one — the fallback fill for
/// a pass whose overall rollup is silent (the sealed-seed pass moves no download
/// bytes, so its rollup jumps per program; the extract meter is the live truth).
fn current_program_line(f: &atpkg::progress::ProgressFile) -> (String, Option<f32>) {
    // Mid-flight phases first, in pass order; a queued front-of-queue program
    // only when nothing is mid-flight.
    let active = f
        .programs
        .iter()
        .filter(|(_, r)| {
            matches!(
                r.phase,
                Phase::Download | Phase::Verify | Phase::Extract | Phase::Link
            )
        })
        .min_by_key(|(_, r)| r.phase as u8);
    let Some((raw, row)) =
        active.or_else(|| f.queue.first().and_then(|n| f.programs.get_key_value(n)))
    else {
        return (String::new(), None);
    };
    let Some(name) = admitted_name(raw) else {
        return (String::new(), None);
    };
    let metered = row.bytes_total > 0;
    let frac =
        metered.then(|| (row.bytes_done.min(row.bytes_total)) as f32 / row.bytes_total as f32);
    let line = match row.phase {
        Phase::Queued => format!("{name} — queued"),
        Phase::Download => {
            if metered {
                format!(
                    "{name} — downloading {} / {}",
                    fmt_bytes(row.bytes_done.min(row.bytes_total)),
                    fmt_bytes(row.bytes_total)
                )
            } else {
                format!("{name} — downloading")
            }
        }
        // Label-only phases: not byte streams atpkg can meter, and the bar does
        // not pretend otherwise.
        Phase::Verify => format!("{name} — verifying"),
        Phase::Extract => {
            if metered {
                format!(
                    "{name} — extracting {} / {}",
                    fmt_bytes(row.bytes_done.min(row.bytes_total)),
                    fmt_bytes(row.bytes_total)
                )
            } else {
                format!("{name} — extracting")
            }
        }
        Phase::Link => format!("{name} — linking"),
        Phase::Done => format!("{name} — installed"),
        Phase::Failed => match row.error.as_deref() {
            Some(e) => format!("{name} — failed: {}", sanitize_for_tty(e, ERROR_CAP)),
            None => format!("{name} — failed"),
        },
        Phase::Skipped => format!("{name} — already current"),
    };
    let line = if row.bumped {
        format!("{line} (you asked for this)")
    } else {
        line
    };
    (
        line,
        if matches!(row.phase, Phase::Download | Phase::Extract) {
            frac
        } else {
            None
        },
    )
}

/// A program name is UNTRUSTED until it round-trips the store's name gate; one
/// that fails simply has no words on the bar.
fn admitted_name(raw: &str) -> Option<String> {
    let name = atpkg::store::ToolName::new(raw)?;
    Some(sanitize_for_tty(name.as_str(), NAME_CAP))
}

/// Human byte figure, decimal units like every download dialog: `812 KB`,
/// `512 MB`, `1.2 GB`.
pub(crate) fn fmt_bytes(n: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    let f = n as f64;
    if f >= GB {
        format!("{:.1} GB", f / GB)
    } else if f >= MB {
        format!("{:.0} MB", f / MB)
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

// ---------------------------------------------------------------------------
// The width law.
// ---------------------------------------------------------------------------

/// Where each piece of a bar lands at `cols` — the pure half of [`paint_rows`].
/// Columns are absolute; a `None` piece was dropped for want of room.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Layout {
    pub glyph_col: usize,
    pub title_col: usize,
    /// The title as drawn (possibly truncated with `…`).
    pub title: String,
    /// `(col, text)` — the detail as drawn, or `None` when dropped.
    pub detail: Option<(usize, String)>,
    /// `(col, width)` of the meter, or `None` (no fill, or no room).
    pub meter: Option<(usize, usize)>,
    /// `(col, text)` of the percentage, beside the meter.
    pub pct: Option<(usize, String)>,
    /// `(col, text)` of the right-aligned stats, or `None` when dropped.
    pub stats: Option<(usize, String)>,
}

/// Lay one bar out at `cols`. Priority when the row is too narrow, in the order
/// pieces are DROPPED: the stats (the meter already says what they say), then
/// the detail truncates and goes, then the meter shrinks to [`METER_MIN`], then
/// the meter goes, then the title truncates. The glyph and (some of) the title
/// always survive. The detail outranks the stats: "trust — extracting 120 MB /
/// 900 MB" is the sentence a user reads; "3 of 10 · 512 MB / 1.2 GB" is a figure
/// the meter repeats.
pub(crate) fn layout(text: &BarText, fill: Option<f32>, cols: usize) -> Layout {
    let width = |s: &str| s.chars().count();
    let budget = cols.saturating_sub(2 * MARGIN);
    let mut out = Layout {
        glyph_col: MARGIN,
        title_col: MARGIN + 2,
        ..Layout::default()
    };
    // "<glyph> <title>" — the head.
    let head_fixed = 2; // glyph + space
    let mut title = text.title.clone();
    if head_fixed + width(&title) > budget {
        title = truncate(&title, budget.saturating_sub(head_fixed));
    }
    let head = head_fixed + width(&title);
    out.title = title;
    let mut used = head;
    // The pieces that want to live on the right: meter + pct, then stats.
    let pct_text = fill.map(|f| format!("{:>3}%", (f.clamp(0.0, 1.0) * 100.0).floor() as u32));
    let pct_w = pct_text.as_ref().map_or(0, |p| 1 + width(p)); // " NN%"
    let meter_w_min = fill.map_or(0, |_| METER_MIN);
    let detail_w = if text.detail.is_empty() {
        0
    } else {
        2 + width(&text.detail)
    };
    let stats_w = if text.stats.is_empty() {
        0
    } else {
        2 + width(&text.stats)
    };

    // 1. meter at minimum width (+ pct) if it fits.
    let mut meter_w = 0;
    let mut have_pct = false;
    if fill.is_some() && used + 2 + meter_w_min + pct_w <= budget {
        meter_w = meter_w_min;
        have_pct = true;
        used += 2 + meter_w + pct_w;
    }
    // 2. the detail in full and the stats, if both fit; else the detail in full
    //    alone; else the detail truncated to what is left (the stats go first).
    let mut detail: Option<String> = None;
    let mut have_stats = false;
    if detail_w > 0 && used + detail_w + stats_w <= budget {
        detail = Some(text.detail.clone());
        have_stats = stats_w > 0;
        used += detail_w + stats_w;
    } else if detail_w > 0 && used + detail_w <= budget {
        detail = Some(text.detail.clone());
        used += detail_w;
    } else if detail_w > 0 {
        let room = budget.saturating_sub(used);
        if room >= 2 + 4 {
            let d = truncate(&text.detail, room - 2);
            used += 2 + width(&d);
            detail = Some(d);
        }
    } else if stats_w > 0 && used + stats_w <= budget {
        have_stats = true;
        used += stats_w;
    }
    // 4. grow the meter toward its preferred width with what remains.
    if meter_w > 0 {
        let spare = budget.saturating_sub(used);
        let grow = (METER_PREFERRED - meter_w).min(spare);
        meter_w += grow;
        used += grow;
    }
    // ---- place: head, detail left-to-right; stats, pct, meter right-to-left.
    let mut col = MARGIN + head;
    if let Some(d) = detail {
        col += 2;
        out.detail = Some((col, d));
    }
    let _ = col;
    let mut right = MARGIN + budget; // one past the last usable column
    if have_stats {
        right -= width(&text.stats);
        out.stats = Some((right, text.stats.clone()));
        right -= 2;
    }
    if have_pct && let Some(p) = pct_text {
        right -= width(&p);
        out.pct = Some((right, p));
        right -= 1;
    }
    if meter_w > 0 {
        right -= meter_w;
        out.meter = Some((right, meter_w));
    }
    let _ = used;
    out
}

/// `s` cut to at most `max` cells, ending in `…` when anything was cut.
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('\u{2026}');
    t
}

// ---------------------------------------------------------------------------
// The painter.
// ---------------------------------------------------------------------------

/// Paint every live bar as one `cols`-wide row each, top to bottom, on the
/// chrome band's material ([`chrome_band::band_colors`] — the same tone the
/// find bar and the config notices already use, so the window's in-grid chrome
/// reads as one surface). The LAST bar row carries the hairline that closes the
/// chrome against the terminal content beneath it.
pub(crate) fn paint_rows(bars: &StatusBars, cols: usize, theme: Theme) -> Vec<Vec<RenderCell>> {
    let c = chrome_band::band_colors(theme);
    let n = bars.rows() as usize;
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(n);
    for (i, (_, bar)) in bars.bars().enumerate() {
        let mut row = blank_row(cols, c.label, c.bar_bg, false);
        paint_bar(&mut row, cols, bar, &c);
        if i + 1 == n {
            // The content-facing edge: one unbroken rule under the last bar,
            // text and all — the find bar's seam, the strip's `seal_strip_bottom`.
            for cell in &mut row {
                cell.underline = UnderlineStyle::Single;
                cell.underline_color = Some(c.label);
            }
        }
        rows.push(row);
    }
    rows
}

fn paint_bar(row: &mut [RenderCell], cols: usize, bar: &Bar, c: &BandColors) {
    let l = layout(&bar.text, bar.fill, cols);
    let ink = match bar.text.tone {
        Tone::Info => c.value,
        Tone::Success => c.value,
        Tone::Warn => c.warn,
    };
    let accent = match bar.text.tone {
        Tone::Warn => c.warn,
        _ => c.accent,
    };
    // Glyph: text presentation on purpose — ⚠ and ✓ have emoji forms, and a
    // colour emoji in a chrome row would be two cells wide in one.
    if l.glyph_col < cols {
        let mut g = chrome_band::cell(bar.text.glyph, accent, c.bar_bg, true, false);
        g.text_presentation = true;
        row[l.glyph_col] = g;
    }
    write_str(row, cols, l.title_col, &l.title, ink, c.bar_bg, true);
    if let Some((col, d)) = &l.detail {
        write_str(row, cols, *col, d, c.label, c.bar_bg, false);
    }
    if let Some((col, w)) = l.meter
        && let Some(f) = bar.fill
    {
        // Half-cell resolution: `2w` steps, a full cell is two, a half cell is
        // the left-half block in the accent over the track.
        let steps = (f.clamp(0.0, 1.0) * (2 * w) as f32).round() as usize;
        for i in 0..w {
            let x = col + i;
            if x >= cols {
                break;
            }
            let cell_steps = steps.saturating_sub(2 * i).min(2);
            row[x] = match cell_steps {
                2 => chrome_band::cell(' ', c.meter_track, accent, false, false),
                1 => chrome_band::cell('\u{258c}', accent, c.meter_track, false, false),
                _ => chrome_band::cell(' ', c.meter_track, c.meter_track, false, false),
            };
        }
    }
    if let Some((col, p)) = &l.pct {
        write_str(row, cols, *col, p, ink, c.bar_bg, false);
    }
    if let Some((col, s)) = &l.stats {
        write_str(row, cols, *col, s, c.label, c.bar_bg, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn t0() -> Instant {
        Instant::now()
    }

    fn file(running_pid: Option<u32>, pass: &str) -> atpkg::progress::ProgressFile {
        atpkg::progress::ProgressFile {
            v: PROGRESS_VERSION,
            pid: running_pid,
            pass: pass.to_string(),
            started_unix: 1_700_000_000,
            heartbeat_unix: 1_700_000_000,
            overall: atpkg::progress::Overall {
                programs_done: 3,
                programs_total: 10,
                bytes_done: 512_000_000,
                bytes_total: 1_200_000_000,
            },
            queue: vec!["ty".into(), "ay".into()],
            programs: BTreeMap::from([
                (
                    "trust".to_string(),
                    atpkg::progress::ProgramProgress {
                        phase: Phase::Extract,
                        bytes_done: 120_000_000,
                        bytes_total: 900_000_000,
                        build: Some(5520),
                        bumped: false,
                        error: None,
                    },
                ),
                (
                    "ty".to_string(),
                    atpkg::progress::ProgramProgress {
                        phase: Phase::Queued,
                        bytes_done: 0,
                        bytes_total: 0,
                        build: None,
                        bumped: false,
                        error: None,
                    },
                ),
            ]),
            ended_unix: None,
        }
    }

    fn snap(running: bool) -> crate::PkgProgressSnapshot {
        crate::PkgProgressSnapshot {
            file: file(Some(7), "net"),
            running,
        }
    }

    fn text_of(row: &[RenderCell]) -> String {
        row.iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn hidden_bars_are_the_zero_key_and_no_deadline() {
        let bars = StatusBars::default();
        assert_eq!(bars.rows(), 0);
        assert_eq!(bars.fingerprint(), 0);
        assert_eq!(bars.deadline(), None);
        assert!(paint_rows(&bars, 80, Theme::default()).is_empty());
    }

    #[test]
    fn an_announcement_opens_the_toolchain_bar_before_any_snapshot() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_announced(
            "installing 10 ALab program(s) from the bundled registry (about 3 GB on disk when finished)",
            now,
        );
        assert_eq!(bars.rows(), 1);
        let (lane, bar) = bars.bars().next().unwrap();
        assert_eq!(lane, Lane::Toolchain);
        assert_eq!(bar.text.title, "Installing the ALab toolchain");
        assert!(
            bar.text.detail.contains("about 3 GB on disk when finished"),
            "{}",
            bar.text.detail
        );
        assert_eq!(bar.fill, None, "no meter before the file exists");
        assert_eq!(bar.fold_at, None, "live: no fold");
        assert_eq!(bar.stale_at, Some(now + ANNOUNCE_STALE), "…but a cap");
        assert_ne!(bars.fingerprint(), 0);
        // A pass that plans nothing never opens a bar of its own, and closes an
        // announced one when it ends empty — the every-6-hours no-op stays invisible.
        let mut quiet = StatusBars::default();
        let mut f = file(Some(7), "net");
        f.overall = atpkg::progress::Overall::default();
        f.queue.clear();
        f.programs.clear();
        quiet.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: true,
            }),
            now,
        );
        assert_eq!(quiet.rows(), 0, "unplanned running pass: no bar");
        f.pid = None;
        f.ended_unix = Some(1);
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: false,
            }),
            now,
        );
        assert_eq!(bars.rows(), 0, "announced, then ended empty: folded");
    }

    #[test]
    fn a_live_meter_never_runs_backwards_within_a_pass() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        let high = bars.bars().next().unwrap().1.fill.unwrap();
        let mut dip = file(Some(7), "net");
        dip.overall.bytes_done = 10;
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: dip.clone(),
                running: true,
            }),
            now,
        );
        assert_eq!(
            bars.bars().next().unwrap().1.fill,
            Some(high),
            "the dip is clamped"
        );
        // A NEW pass identity starts from its own truth.
        dip.started_unix += 1;
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: dip,
                running: true,
            }),
            now,
        );
        assert!(bars.bars().next().unwrap().1.fill.unwrap() < high);
    }

    #[test]
    fn a_live_bar_whose_feed_went_silent_folds_at_its_cap() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        assert_eq!(bars.deadline(), Some(now + TAILED_STALE));
        assert!(!bars.settle(now + TAILED_STALE / 2));
        assert!(bars.settle(now + TAILED_STALE), "no report for 30 s ⇒ gone");
        assert_eq!(bars.rows(), 0);
        // A fresh report re-arms the cap.
        bars.toolchain_snapshot(Some(&snap(true)), now);
        bars.toolchain_snapshot(Some(&snap(true)), now + Duration::from_secs(20));
        assert!(!bars.settle(now + TAILED_STALE));
        assert_eq!(
            bars.deadline(),
            Some(now + Duration::from_secs(20) + TAILED_STALE)
        );
    }

    #[test]
    fn a_running_snapshot_reports_the_current_program_and_the_rollup() {
        let mut bars = StatusBars::default();
        bars.toolchain_snapshot(Some(&snap(true)), t0());
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Installing the ALab toolchain");
        assert_eq!(bar.text.detail, "trust — extracting 120 MB / 900 MB");
        assert_eq!(bar.text.stats, "3 of 10 · 512 MB / 1.2 GB");
        let f = bar.fill.unwrap();
        assert!((f - 512.0 / 1200.0).abs() < 1e-3, "{f}");
        assert_eq!(bar.fold_at, None);
    }

    #[test]
    fn the_seed_pass_has_its_own_title_and_falls_back_to_the_extract_meter() {
        let mut bars = StatusBars::default();
        let mut f = file(Some(7), "seed");
        f.overall.bytes_total = 0;
        f.overall.bytes_done = 0;
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: true,
            }),
            t0(),
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Preparing the ALab toolchain");
        let fill = bar
            .fill
            .expect("the extract meter stands in for a silent rollup");
        assert!((fill - 120.0 / 900.0).abs() < 1e-3);
        assert_eq!(bar.text.stats, "3 of 10");
    }

    #[test]
    fn not_running_claims_only_terminal_states() {
        // Ended cleanly: success, folds after HOLD_OK.
        let mut bars = StatusBars::default();
        let mut f = file(None, "net");
        f.ended_unix = Some(1_700_000_100);
        let now = t0();
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.tone, Tone::Success);
        assert_eq!(bar.text.detail, "all 10 installed");
        assert_eq!(bar.fold_at, Some(now + HOLD_OK));
        assert_eq!(bar.fill, Some(1.0));
        // Ended with a failure: warn, the longer hold.
        f.programs.get_mut("trust").unwrap().phase = Phase::Failed;
        let mut bars = StatusBars::default();
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.tone, Tone::Warn);
        assert!(bar.text.detail.contains("1 failed"), "{}", bar.text.detail);
        assert_eq!(bar.fold_at, Some(now + HOLD_WARN));
        // Dead writer, no clean end: "stopped", names the next act, never a live phase.
        f.ended_unix = None;
        let mut bars = StatusBars::default();
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert!(
            bar.text.detail.starts_with("stopped — "),
            "{}",
            bar.text.detail
        );
        assert!(!bar.text.detail.contains("extracting"));
        assert_eq!(bar.fold_at, Some(now + HOLD_WARN));
    }

    #[test]
    fn a_marker_outcome_outranks_the_final_snapshot_and_none_never_erases_it() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        bars.toolchain_installed(
            "✓ ALab toolchain installed: ay, trust — open a new tab to use them",
            now,
        );
        // The tailer's final read lands AFTER the marker: it may complete the
        // meter, never replace the words.
        let mut f = file(None, "net");
        f.ended_unix = Some(1);
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "ALab toolchain installed");
        assert!(bar.text.detail.contains("open a new tab"));
        assert_eq!(bar.fill, Some(1.0));
        // …and the vanished-file clear keeps the last words too.
        bars.toolchain_snapshot(None, now);
        assert_eq!(bars.rows(), 1);
        // Whereas a LIVE bar whose file vanished has nothing honest left to say.
        let mut live = StatusBars::default();
        live.toolchain_snapshot(Some(&snap(true)), now);
        live.toolchain_snapshot(None, now);
        assert_eq!(live.rows(), 0);
    }

    #[test]
    fn settle_folds_expired_bars_and_reports_the_row_change() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_failed(
            "⚠ ALab toolchain install failed — see Settings ▸ Packages",
            now,
        );
        bars.update_progress(
            &aterm_update::Progress::Staged {
                version: "0.48.0".into(),
                build: 99,
            },
            now,
        );
        assert_eq!(bars.rows(), 2);
        assert_eq!(
            bars.deadline(),
            Some(now + HOLD_OK),
            "the earliest fold wins"
        );
        assert!(!bars.settle(now), "nothing due yet");
        assert!(bars.settle(now + HOLD_OK), "the update bar folded");
        assert_eq!(bars.rows(), 1);
        assert_eq!(bars.lane_at(0), Some(Lane::Toolchain));
        assert!(bars.settle(now + HOLD_WARN));
        assert_eq!(bars.rows(), 0);
        assert_eq!(bars.fingerprint(), 0);
    }

    #[test]
    fn update_reports_map_to_bar_states() {
        use aterm_update::Progress as P;
        let mut bars = StatusBars::default();
        let now = t0();
        bars.update_progress(
            &P::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 74_000_000,
            },
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "aterm update v0.48.0");
        assert_eq!(bar.text.stats, "45 MB / 74 MB");
        assert!((bar.fill.unwrap() - 45.0 / 74.0).abs() < 1e-3);
        assert_eq!(bar.fold_at, None);
        // An unknown total is honest: no meter, just the bytes so far.
        bars.update_progress(
            &P::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 0,
            },
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.fill, None);
        assert_eq!(bar.text.stats, "45 MB");
        bars.update_progress(
            &P::Verifying {
                version: "0.48.0".into(),
            },
            now,
        );
        assert_eq!(
            bars.bars().next().unwrap().1.text.detail,
            "verifying and staging…"
        );
        bars.update_progress(
            &P::Failed {
                detail: "zip sha256 mismatch".into(),
            },
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.tone, Tone::Warn);
        assert!(bar.text.detail.contains("Software Update"));
        assert_eq!(bar.fold_at, Some(now + HOLD_WARN));
    }

    #[test]
    fn hostile_strings_are_sanitized_before_they_become_cells() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_failed("bad\u{1b}[31m thing\u{7}", now);
        let d = &bars.bars().next().unwrap().1.text.detail;
        assert!(!d.contains('\u{1b}') && !d.contains('\u{7}'), "{d:?}");
        // A program name that fails the store gate has no row words at all.
        let mut bars = StatusBars::default();
        let mut f = file(Some(7), "net");
        f.programs.clear();
        f.queue = vec!["../evil".into()];
        f.programs.insert(
            "../evil".into(),
            atpkg::progress::ProgramProgress {
                phase: Phase::Download,
                bytes_done: 1,
                bytes_total: 2,
                build: None,
                bumped: false,
                error: None,
            },
        );
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: true,
            }),
            now,
        );
        assert_eq!(bars.bars().next().unwrap().1.text.detail, "");
    }

    #[test]
    fn the_layout_degrades_in_priority_order() {
        let text = BarText {
            glyph: '\u{21e3}',
            title: "Installing the ALab toolchain".into(),
            detail: "trust — extracting 120 MB / 900 MB".into(),
            stats: "3 of 10 · 512 MB / 1.2 GB".into(),
            tone: Tone::Info,
        };
        // Wide: everything, meter at its preferred width, stats flush right.
        let l = layout(&text, Some(0.43), 160);
        assert_eq!(l.title, text.title);
        assert!(l.detail.is_some());
        assert_eq!(l.meter.map(|m| m.1), Some(METER_PREFERRED));
        assert_eq!(l.pct.as_ref().map(|p| p.1.as_str()), Some(" 43%"));
        let (sc, s) = l.stats.clone().unwrap();
        assert_eq!(
            sc + s.chars().count(),
            160 - MARGIN,
            "stats end at the margin"
        );
        // The meter sits left of " NN%", which sits left of the stats.
        let (mc, mw) = l.meter.unwrap();
        assert_eq!(mc + mw + 1, l.pct.as_ref().unwrap().0);
        assert!(l.pct.as_ref().unwrap().0 + 4 + 2 == sc);
        // Narrower: the stats go first.
        let l = layout(&text, Some(0.43), 70);
        assert!(l.stats.is_none());
        assert!(l.detail.is_some());
        assert!(l.meter.is_some());
        // Narrower still: the detail truncates, then goes; the meter shrinks.
        let l = layout(&text, Some(0.43), 48);
        let d = l.detail.as_ref().map(|d| d.1.clone()).unwrap_or_default();
        assert!(d.is_empty() || d.ends_with('\u{2026}'), "{d}");
        assert!(l.meter.is_some_and(|m| m.1 >= METER_MIN));
        // Tiny: glyph + title only, the title truncated.
        let l = layout(&text, Some(0.43), 16);
        assert!(l.meter.is_none() && l.pct.is_none() && l.detail.is_none());
        assert!(l.title.ends_with('\u{2026}'));
        assert!(l.title.chars().count() + 2 <= 16 - 2 * MARGIN);
        // Every placed piece stays inside the row.
        for cols in [8usize, 12, 20, 33, 47, 61, 80, 200] {
            let l = layout(&text, Some(0.5), cols);
            let end = |c: usize, s: &str| c + s.chars().count();
            assert!(end(l.title_col, &l.title) <= cols - MARGIN, "cols {cols}");
            if let Some((c, d)) = &l.detail {
                assert!(end(*c, d) <= cols - MARGIN, "cols {cols}");
            }
            if let Some((c, w)) = l.meter {
                assert!(c + w <= cols - MARGIN, "cols {cols}");
                assert!(c >= l.title_col + l.title.chars().count(), "cols {cols}");
            }
            if let Some((c, s)) = &l.stats {
                assert_eq!(end(*c, s), cols - MARGIN, "cols {cols}");
            }
        }
    }

    #[test]
    fn painted_rows_are_exactly_cols_wide_and_carry_the_words() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        bars.update_progress(
            &aterm_update::Progress::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 74_000_000,
            },
            now,
        );
        // Wide enough for every piece of the toolchain row (the detail outranks
        // the stats, so a narrower row would drop the figures first).
        let rows = paint_rows(&bars, 140, Theme::default());
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.len(), 140);
        }
        let top = text_of(&rows[0]);
        assert!(top.contains("Installing the ALab toolchain"), "{top}");
        assert!(top.contains("trust — extracting"), "{top}");
        assert!(top.contains("42%"), "{top}");
        assert!(top.ends_with("3 of 10 · 512 MB / 1.2 GB"), "{top}");
        let bottom = text_of(&rows[1]);
        assert!(bottom.contains("aterm update v0.48.0"), "{bottom}");
        assert!(bottom.contains("60%"), "{bottom}");
        // The meter is drawn as background cells: filled ones wear the accent.
        let c = chrome_band::band_colors(Theme::default());
        let l = layout(
            &bars.bars().next().unwrap().1.text,
            Some(512.0 / 1200.0),
            140,
        );
        let (mc, mw) = l.meter.unwrap();
        assert_eq!(rows[0][mc].bg, c.accent, "first meter cell is filled");
        assert_eq!(
            rows[0][mc + mw - 1].bg,
            c.meter_track,
            "last meter cell is track"
        );
        // Only the LAST bar row closes the chrome with the seam.
        assert_eq!(rows[0][0].underline, UnderlineStyle::None);
        assert_eq!(rows[1][0].underline, UnderlineStyle::Single);
        // The glyph is text-presentation, never an emoji.
        assert!(rows[0][MARGIN].text_presentation);
    }

    #[test]
    fn bytes_read_like_a_download_dialog() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(812_000), "812 KB");
        assert_eq!(fmt_bytes(512_000_000), "512 MB");
        assert_eq!(fmt_bytes(1_200_000_000), "1.2 GB");
    }
}
