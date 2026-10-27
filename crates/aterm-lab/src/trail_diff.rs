// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-diff [SCRIPT|DIR]...`: Prism Glass's dark proof (docs/design/PRISM-GLASS.md §15.0.2,
//! §15.1 P0.5 as §15.7.2 amends it). It builds aterm-gui's `trail_dump` example twice, in this
//! checkout (the head) and in a worktree of the base under the lab state dir, feeds both builds
//! the head's `.trail` scripts and compares what they print, frame by frame:
//! - main's state (every `state` word, the bed's ink table and the sound cues) must be equal on
//!   every frame, light frames and frames a synchronized update holds included;
//! - main's streams (every `RenderInput` stream, the CPU pixels and the next deadlines: the
//!   glow's, the pet's and the word latch's) must be equal on every frame where no light arm
//!   drew, whatever its light bits say;
//! - on a frame where some arm drew (in either build), every stream no arm touched must still be
//!   equal; the pixels and the deadlines, which every stream feeds, are not compared there, and
//!   such frames are listed with their arms.
//!
//! The light words are never compared. Both builds run the head's scripts, so the harness
//! closure (the dump, its support files, the rig and the frame reads it drives the App by, every
//! `state_words.rs` and `[profile.trail-diff]`) must be the same in both trees: a head whose
//! closure differs from its base is refused, NOT RUN (§15.0.6). Each tree builds with
//! `--profile trail-diff` into its own target directory under the lab state dir (`*.noindex`, so
//! Spotlight skips it), kept for the next run.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_toolchain::named_lock::{self, Refused, Wait};

use crate::instance::{take_flag, take_value};
use crate::linux::NOT_RUN;
use crate::paths::{from_caller, paths, physical};
use crate::{Fail, Res};

/// The base when none is named: the commit's parent, as §15.0.2 requires.
pub const DEFAULT_BASE: &str = "HEAD~1";
/// `--base merge-base`: where this checkout's `HEAD` left [`MAIN`], so a whole stack compares.
pub const MERGE_BASE: &str = "merge-base";
/// The branch `--base merge-base` measures from.
pub const MAIN: &str = "origin/main";

/// The dump's example, the first member of the harness closure.
pub const DUMP: &str = "crates/aterm-gui/examples/trail_dump.rs";
/// The dump's support files: every file under this folder is in the closure.
pub const SUPPORT: &str = "crates/aterm-gui/examples/support/";
/// The headless App the dump drives: which paths its events take, the clock it pins and the
/// window it draws and reads.
pub const RIG: &str = "crates/aterm-gui/src/trail_rig.rs";
/// The window reads the dump prints: the state words, the light report, the bed's ink table
/// and the deadlines.
pub const READS: &str = "crates/aterm-gui/src/trail_frame_reads.rs";
/// The state-word seams (P0.3): a file of this name anywhere is in the closure.
pub const STATE_WORDS: &str = "state_words.rs";
/// The profile both builds use, a table of the root `Cargo.toml` that is in the closure.
pub const PROFILE: &str = "trail-diff";
/// The folder the corpus lands in (P0.6): the scripts when none is named.
pub const CORPUS: &str = "crates/aterm-effects/tests/fixtures/trail-diff";

/// `--control plant`'s file in the base copy...
pub const PLANT_FILE: &str = "crates/aterm-effects/src/rainbow_kitty/ribbon.rs";
/// ...and the constant it lowers by one (§15.1 P0.5).
pub const PLANT_CONST: &str = "const BODY_FRAME_TOP: f32 = ";

/// The verb's own help (`trail-diff --help`).
pub const HELP: &str = "\
usage: targo --unverified lab trail-diff [--base REF|merge-base] [--control same|plant] [--keep] [--jobs N]
         [SCRIPT.trail|DIR]...

Prism Glass's dark proof (docs/design/PRISM-GLASS.md section 15.0.2, 15.1 P0.5). It builds aterm-gui's trail_dump
example (--profile trail-diff --features bench-support) in this checkout, the head (its working tree), and in a
worktree of the base under the lab state dir, each into its own target directory there (head.noindex, base.noindex),
kept between runs. Both builds run the head's scripts (each .trail file named, and every .trail under each DIR
named; with none, every one under crates/aterm-effects/tests/fixtures/trail-diff), and their dumps are compared
frame by frame:
  state    every state word, the bed's ink table and the sound cues: equal on every frame, light and held frames
           included
  streams  every RenderInput stream, the CPU pixels and the next deadlines: equal on every frame no light arm drew,
           whatever its light bits; on a frame where some arm drew (in either build), each stream no arm touched
           must still be equal, and the frame is listed with its arms
The light words are never compared. It prints the first difference of each script that has one, the frames where
an arm drew, then scripts= frames= differing_compared= arm_frames= state_differing=.

  --base REF         the base commit: HEAD~1 (the commit's parent) by default; merge-base is where HEAD left
                     origin/main, so a whole stack compares
  --control same     the base is HEAD itself: the dumps must not differ (the head's working tree must be HEAD's)
  --control plant    the base is HEAD with BODY_FRAME_TOP in rainbow_kitty/ribbon.rs lowered by one: the difference
                     must be reported
  --keep             keep the base worktree; the next run checks it out at its base instead of adding it afresh,
                     so its build rebuilds only what the bases' trees change
  --jobs N           dumps run at once per build (default half the cores, at most one per script)

Refused, NOT RUN, when the harness closure (crates/aterm-gui/examples/trail_dump.rs, crates/aterm-gui/examples/
support/**, crates/aterm-gui/src/trail_rig.rs, crates/aterm-gui/src/trail_frame_reads.rs, every state_words.rs and
[profile.trail-diff]) differs between the base and the head: a closure change lands alone and is proved by both
controls (section 15.0.6).

exit: 0 the dumps agree; 1 a difference; 2 usage; 3 not run (the reason named). Under --control the exit says
whether the control held: 0 it did (same: no difference; plant: the plant reported), 1 it did not.
";

/// What `--base` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Base {
    /// A commit, by any name git resolves.
    Ref(String),
    /// [`MERGE_BASE`].
    MergeBase,
}

/// A control run (§15.0.6): it proves the differential, not a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// The base is `HEAD` itself: nothing may differ.
    Same,
    /// The base is `HEAD` with [`PLANT_CONST`] lowered by one: the difference must be reported.
    Plant,
}

/// The verb's arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub base: Base,
    pub control: Option<Control>,
    pub keep: bool,
    /// `--jobs`, when given.
    pub jobs: Option<usize>,
    /// Each script or folder named, as typed.
    pub scripts: Vec<String>,
}

/// Parse the verb's arguments.
/// # Errors
/// A usage error: an unknown option, a value missing or malformed, or `--base` beside `--control`.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let base = take_value(&mut a, "--base")?;
    let control = match take_value(&mut a, "--control")?.as_deref() {
        None => None,
        Some("same") => Some(Control::Same),
        Some("plant") => Some(Control::Plant),
        Some(other) => {
            return Err(Fail::usage(format!(
                "trail-diff: --control takes same or plant, not '{other}'"
            )));
        }
    };
    let jobs = take_value(&mut a, "--jobs")?
        .map(|v| {
            v.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| {
                Fail::usage(format!(
                    "trail-diff: --jobs takes a positive count, not '{v}'"
                ))
            })
        })
        .transpose()?;
    let keep = take_flag(&mut a, "--keep");
    if let Some(flag) = a.iter().find(|x| x.starts_with('-') && x.len() > 1) {
        return Err(Fail::usage(format!(
            "trail-diff: unknown option '{flag}' (trail-diff --help)"
        )));
    }
    if control.is_some() && base.is_some() {
        return Err(Fail::usage(
            "trail-diff: --control names its own base (HEAD); drop --base",
        ));
    }
    let base = match base.as_deref() {
        None => Base::Ref(DEFAULT_BASE.to_string()),
        Some(MERGE_BASE) => Base::MergeBase,
        Some("") => return Err(Fail::usage("trail-diff: --base needs a commit")),
        Some(r) => Base::Ref(r.to_string()),
    };
    Ok(Args {
        base,
        control,
        keep,
        jobs,
        scripts: a,
    })
}

/// Whether the repository path `path` is in the harness closure's files (§15.7.2): the dump, its
/// support, the rig and its frame reads (which decide what the dump measures: the events' paths,
/// the pinned clock, the window drawn and read), and every state-word seam.
#[must_use]
pub fn in_closure(path: &str) -> bool {
    path == DUMP
        || path == RIG
        || path == READS
        || path.starts_with(SUPPORT)
        || path == STATE_WORDS
        || path.ends_with(&format!("/{STATE_WORDS}"))
}

/// The closure's files among git's NUL-separated path lists: `changed` (`git diff --name-only
/// --no-renames -z <base>`, what the head's tracked files change) and `untracked` (`git ls-files
/// --others --exclude-standard -z`, what the head adds beside them), sorted, each once.
#[must_use]
pub fn closure_changes(changed: &str, untracked: &str) -> Vec<String> {
    let mut out: Vec<String> = changed
        .split('\0')
        .chain(untracked.split('\0'))
        .map(|p| p.trim_end_matches('\n'))
        .filter(|p| !p.is_empty() && in_closure(p))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The `[profile.<PROFILE>]` table of a `Cargo.toml`'s text: its lines, trimmed, without
/// comments or blank lines; `None` when it has none.
#[must_use]
pub fn profile_table(toml: &str) -> Option<Vec<String>> {
    let header = format!("[profile.{PROFILE}]");
    let mut lines = toml.lines().map(str::trim);
    lines.find(|l| *l == header)?;
    Some(
        lines
            .take_while(|l| !l.starts_with('['))
            .map(|l| l.split_once('#').map_or(l, |(code, _)| code).trim())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Every member of the harness closure that differs between the base and the head: the files
/// [`closure_changes`] finds, then `[profile.trail-diff]` when the two `Cargo.toml` texts'
/// tables differ ([`profile_table`]).
#[must_use]
pub fn closure_differs(
    changed: &str,
    untracked: &str,
    base_toml: &str,
    head_toml: &str,
) -> Vec<String> {
    let mut out = closure_changes(changed, untracked);
    if profile_table(base_toml) != profile_table(head_toml) {
        out.push(format!("[profile.{PROFILE}]"));
    }
    out
}

/// The refusal of a head whose closure differs from its base: NOT RUN, naming each member.
#[must_use]
pub fn closure_refusal(members: &[String]) -> Fail {
    not_run(format!(
        "the harness closure differs between the base and the head ({}): both builds run the \
         head's scripts, so a closure change lands alone, after the API it calls, and is proved \
         by --control same and --control plant (docs/design/PRISM-GLASS.md section 15.0.6)",
        members.join(", ")
    ))
}

/// A run that could not be made, exit 3, its reason named.
#[must_use]
pub fn not_run(why: impl std::fmt::Display) -> Fail {
    Fail::exit(NOT_RUN, format!("trail-diff: not run: {why}"))
}

/// `text` (the plant file) with [`PLANT_CONST`]'s value lowered by one; `None` when the file
/// declares it on no line or with no number.
#[must_use]
pub fn plant(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut planted = false;
    for line in text.split_inclusive('\n') {
        if !planted
            && let Some(at) = line.find(PLANT_CONST)
            && let Some((value, rest)) = line[at + PLANT_CONST.len()..].split_once(';')
            && let Ok(v) = value.trim().parse::<f32>()
        {
            let _ = write!(out, "{}{PLANT_CONST}{:?};{rest}", &line[..at], v - 1.0);
            planted = true;
        } else {
            out.push_str(line);
        }
    }
    planted.then_some(out)
}

// ---- The dump's output ----------------------------------------------------------------------

/// One frame a dump printed (`examples/support/trail_script.rs` has the format).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// `F <n> t=<ms>`'s number and instant, as printed.
    pub n: String,
    pub t: String,
    /// `F <n> t=<ms> held`: a synchronized update held it, so it drew nothing.
    pub held: bool,
    /// `light=`, never compared.
    pub light: String,
    /// `arms=`: each light arm that drew and the streams it touched.
    pub arms: Vec<(String, Vec<String>)>,
    /// The `streams` line's words, `<stream>` and `<len>:<hash>`, in print order.
    pub streams: Vec<(String, String)>,
    /// `px` and `deadline`, which every stream feeds: compared where no arm drew.
    pub drawn: Vec<(String, String)>,
    /// Main's state: `sounds`, each `state` word (`state.n`, `state.<engine>`, `state.bed`) and
    /// every other line the dump prints but `light` and the `W` detail.
    pub state: Vec<(String, String)>,
}

/// One run of a script: `S <label>` (`-`, or the theme `foreach-scheme` gave it) and its frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub label: String,
    pub frames: Vec<Frame>,
}

/// One script's dump: `P <name>` and its runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    pub name: String,
    pub runs: Vec<Run>,
}

/// Parse a dump's output.
/// # Errors
/// A line the format does not have, or a word line before its frame, by line number.
pub fn parse_dump(text: &str) -> Result<Vec<Script>, String> {
    let mut scripts: Vec<Script> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let at = || format!("line {}: {line:?}", i + 1);
        if let Some(name) = line.strip_prefix("P ") {
            scripts.push(Script {
                name: name.to_string(),
                runs: Vec::new(),
            });
        } else if let Some(label) = line.strip_prefix("S ") {
            let script = scripts
                .last_mut()
                .ok_or_else(|| format!("{}: a run before its script", at()))?;
            script.runs.push(Run {
                label: label.to_string(),
                frames: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("F ") {
            let run = scripts
                .last_mut()
                .and_then(|s| s.runs.last_mut())
                .ok_or_else(|| format!("{}: a frame before its run", at()))?;
            run.frames
                .push(frame_head(rest).ok_or_else(|| format!("{}: not a frame line", at()))?);
        } else if let Some(rest) = line.strip_prefix("  ") {
            let frame = scripts
                .last_mut()
                .and_then(|s| s.runs.last_mut())
                .and_then(|r| r.frames.last_mut())
                .ok_or_else(|| format!("{}: a word line before its frame", at()))?;
            let (kind, body) = rest.split_once(' ').unwrap_or((rest, ""));
            match kind {
                "W" | "light" => {}
                "streams" => frame.streams.extend(pairs(body)),
                "px" | "deadline" => frame.drawn.push((kind.to_string(), body.to_string())),
                "state" => frame
                    .state
                    .extend(pairs(body).map(|(k, v)| (format!("state.{k}"), v))),
                _ => frame.state.push((kind.to_string(), body.to_string())),
            }
        } else {
            return Err(format!("{}: not a dump line", at()));
        }
    }
    Ok(scripts)
}

/// `F `'s rest: `<n> t=<ms> held` or `<n> t=<ms> light=<bits> arms=<arms>`.
fn frame_head(rest: &str) -> Option<Frame> {
    let mut words = rest.split(' ');
    let n = words.next().filter(|n| !n.is_empty())?.to_string();
    let t = words.next()?.strip_prefix("t=")?.to_string();
    let mut frame = Frame {
        n,
        t,
        ..Frame::default()
    };
    match words.next()? {
        "held" => frame.held = true,
        light => {
            frame.light = light.strip_prefix("light=")?.to_string();
            frame.arms = arms(words.next()?.strip_prefix("arms=")?);
        }
    }
    words.next().is_none().then_some(frame)
}

/// `arms=`'s value: `-`, or `<arm>:<stream>+<stream>,…`.
fn arms(value: &str) -> Vec<(String, Vec<String>)> {
    if value == "-" {
        return Vec::new();
    }
    value
        .split(',')
        .map(|arm| {
            let (name, streams) = arm.split_once(':').unwrap_or((arm, ""));
            (
                name.to_string(),
                streams
                    .split('+')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        })
        .collect()
}

/// `<k>=<v>` words.
fn pairs(body: &str) -> impl Iterator<Item = (String, String)> + '_ {
    body.split_whitespace().map(|w| {
        let (k, v) = w.split_once('=').unwrap_or((w, ""));
        (k.to_string(), v.to_string())
    })
}

// ---- The comparison --------------------------------------------------------------------------

/// One word the two dumps print differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    /// The word: a stream's name, `px`, `deadline`, `sounds`, `state.<word>`, or the frame's
    /// shape (`frame`, `held`, `run`, `script`).
    pub what: String,
    pub head: String,
    pub base: String,
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} head={} base={}", self.what, self.head, self.base)
    }
}

/// One frame pair compared under §15.0.2.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameVerdict {
    /// Every arm either build drew, with the streams it touched (each arm once, in the order
    /// the head then the base drew them).
    pub arms: Vec<(String, Vec<String>)>,
    /// Differences in what is compared as streams (and the frame's shape).
    pub streams: Vec<Diff>,
    /// Differences in main's state.
    pub state: Vec<Diff>,
}

/// Compare one frame of the head's dump with the base's under §15.0.2.
#[must_use]
pub fn compare_frame(head: &Frame, base: &Frame) -> FrameVerdict {
    let mut v = FrameVerdict::default();
    for (name, streams) in head.arms.iter().chain(&base.arms) {
        match v.arms.iter_mut().find(|(n, _)| n == name) {
            Some((_, touched)) => {
                for s in streams {
                    if !touched.contains(s) {
                        touched.push(s.clone());
                    }
                }
            }
            None => v.arms.push((name.clone(), streams.clone())),
        }
    }
    if head.n != base.n || head.t != base.t {
        v.streams.push(Diff {
            what: "frame".into(),
            head: format!("F{} t={}", head.n, head.t),
            base: format!("F{} t={}", base.n, base.t),
        });
    }
    if head.held || base.held {
        // A held frame draws nothing, so it prints no streams, pixels or deadline: whether it
        // held and main's state under the hold are what it has to compare.
        if head.held != base.held {
            let said = |held: bool| if held { "held" } else { "drawn" }.to_string();
            v.streams.push(Diff {
                what: "held".into(),
                head: said(head.held),
                base: said(base.held),
            });
        } else {
            v.state = differ(&head.state, &base.state);
        }
        return v;
    }
    let touched = |stream: &str| v.arms.iter().any(|(_, s)| s.iter().any(|s| s == stream));
    let mut streams = differ(&head.streams, &base.streams);
    streams.retain(|d| !touched(&d.what));
    v.streams.extend(streams);
    if v.arms.is_empty() {
        v.streams.extend(differ(&head.drawn, &base.drawn));
    }
    v.state = differ(&head.state, &base.state);
    v
}

/// Every key whose value differs between `head` and `base` (a key one side lacks reads `-`),
/// in the head's order and then the base's.
fn differ(head: &[(String, String)], base: &[(String, String)]) -> Vec<Diff> {
    let value = |side: &[(String, String)], k: &str| {
        side.iter()
            .find(|(key, _)| key == k)
            .map_or_else(|| "-".to_string(), |(_, v)| v.clone())
    };
    let mut keys: Vec<&str> = head.iter().map(|(k, _)| k.as_str()).collect();
    for (k, _) in base {
        if !keys.contains(&k.as_str()) {
            keys.push(k);
        }
    }
    keys.into_iter()
        .filter_map(|k| {
            let (h, b) = (value(head, k), value(base, k));
            (h != b).then(|| Diff {
                what: k.to_string(),
                head: h,
                base: b,
            })
        })
        .collect()
}

/// The comparison of two dumps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub scripts: usize,
    pub frames: usize,
    /// Frames where something compared as a stream differs (or the frame's shape does).
    pub differing_compared: usize,
    /// Frames where some light arm drew.
    pub arm_frames: usize,
    /// Frames where main's state differs.
    pub state_differing: usize,
    /// The first difference of each script that has one.
    pub firsts: Vec<String>,
    /// Each run's frames where an arm drew, grouped by the arms.
    pub arms: Vec<String>,
}

impl Report {
    /// Whether the dumps differ anywhere compared.
    #[must_use]
    pub const fn differs(&self) -> bool {
        self.differing_compared > 0 || self.state_differing > 0
    }

    /// The totals line.
    #[must_use]
    pub fn totals(&self) -> String {
        format!(
            "scripts={} frames={} differing_compared={} arm_frames={} state_differing={}",
            self.scripts,
            self.frames,
            self.differing_compared,
            self.arm_frames,
            self.state_differing
        )
    }

    /// What the verb prints: each first difference, each arm listing, then the totals.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in self.firsts.iter().chain(&self.arms) {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&self.totals());
        out.push('\n');
        out
    }
}

/// Compare the head's dump with the base's: scripts, runs and frames in order. A script or a run
/// one side lacks or names differently, and a frame one side lacks, differ as the frame's shape.
#[must_use]
pub fn compare(head: &[Script], base: &[Script]) -> Report {
    let mut r = Report {
        scripts: head.len().max(base.len()),
        ..Report::default()
    };
    for i in 0..r.scripts {
        let (h, b) = (head.get(i), base.get(i));
        let name = h.or(b).map_or("", |s| s.name.as_str());
        let mut first: Option<String> = None;
        match (h, b) {
            (Some(h), Some(b)) if h.name == b.name => {
                compare_runs(&mut r, &mut first, name, &h.runs, &b.runs);
            }
            _ => {
                let frames = |s: Option<&Script>| {
                    s.map_or(0, |s| s.runs.iter().map(|r| r.frames.len()).sum())
                };
                shape(&mut r, frames(h).max(frames(b)));
                let said = |s: Option<&Script>| s.map_or_else(|| "-".into(), |s| s.name.clone());
                note(
                    &mut first,
                    name,
                    "P",
                    &[shape_diff("script", said(h), said(b))],
                );
            }
        }
        r.firsts.extend(first);
    }
    r
}

/// [`compare`] of one script's runs.
fn compare_runs(
    r: &mut Report,
    first: &mut Option<String>,
    name: &str,
    head: &[Run],
    base: &[Run],
) {
    for j in 0..head.len().max(base.len()) {
        let (h, b) = (head.get(j), base.get(j));
        let label = h.or(b).map_or("", |run| run.label.as_str());
        let (h, b) = match (h, b) {
            (Some(h), Some(b)) if h.label == b.label => (h, b),
            _ => {
                let frames = |run: Option<&Run>| run.map_or(0, |run| run.frames.len());
                shape(r, frames(h).max(frames(b)));
                let said =
                    |run: Option<&Run>| run.map_or_else(|| "-".into(), |run| run.label.clone());
                note(
                    first,
                    name,
                    &format!("S {label}"),
                    &[shape_diff("run", said(h), said(b))],
                );
                continue;
            }
        };
        let mut armed: Vec<(String, Vec<String>)> = Vec::new();
        for k in 0..h.frames.len().max(b.frames.len()) {
            r.frames += 1;
            let (hf, bf) = (h.frames.get(k), b.frames.get(k));
            let (n, t) = hf.or(bf).map_or(("", ""), |f| (f.n.as_str(), f.t.as_str()));
            let verdict = match (hf, bf) {
                (Some(hf), Some(bf)) => compare_frame(hf, bf),
                _ => {
                    let said = |f: Option<&Frame>| {
                        f.map_or_else(|| "-".into(), |f| format!("F{} t={}", f.n, f.t))
                    };
                    FrameVerdict {
                        streams: vec![shape_diff("frame", said(hf), said(bf))],
                        ..FrameVerdict::default()
                    }
                }
            };
            if !verdict.arms.is_empty() {
                r.arm_frames += 1;
                let said = arms_said(&verdict.arms);
                match armed.iter_mut().find(|(a, _)| *a == said) {
                    Some((_, frames)) => frames.push(n.to_string()),
                    None => armed.push((said, vec![n.to_string()])),
                }
            }
            r.differing_compared += usize::from(!verdict.streams.is_empty());
            r.state_differing += usize::from(!verdict.state.is_empty());
            let diffs: Vec<Diff> = verdict.streams.into_iter().chain(verdict.state).collect();
            note(first, name, &format!("S {label} F{n} t={t}"), &diffs);
        }
        for (said, frames) in armed {
            r.arms
                .push(format!("arms {name} S {label} {said}: {}", ranges(&frames)));
        }
    }
}

/// `n` frames (at least one) whose shape differs: counted, and counted as differing.
fn shape(r: &mut Report, n: usize) {
    let n = n.max(1);
    r.frames += n;
    r.differing_compared += n;
}

/// A difference in the dumps' shape: `what` the head and the base print.
fn shape_diff(what: &str, head: String, base: String) -> Diff {
    Diff {
        what: what.into(),
        head,
        base,
    }
}

/// The script's first difference, when it has none yet and `diffs` holds one: at `at`, each word.
fn note(first: &mut Option<String>, name: &str, at: &str, diffs: &[Diff]) {
    if first.is_none() && !diffs.is_empty() {
        let words: Vec<String> = diffs.iter().map(ToString::to_string).collect();
        *first = Some(format!("differs {name} {at}: {}", words.join("; ")));
    }
}

/// Arms as the dump prints them: `<arm>:<stream>+<stream>,…`.
fn arms_said(arms: &[(String, Vec<String>)]) -> String {
    arms.iter()
        .map(|(a, s)| format!("{a}:{}", s.join("+")))
        .collect::<Vec<_>>()
        .join(",")
}

/// Frame numbers as `F3-F9 F14`: each run of consecutive numbers once.
#[must_use]
pub fn ranges(frames: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut run: Option<(u64, u64)> = None;
    let flush = |out: &mut Vec<String>, run: (u64, u64)| {
        out.push(if run.0 == run.1 {
            format!("F{}", run.0)
        } else {
            format!("F{}-F{}", run.0, run.1)
        });
    };
    for f in frames {
        let Ok(n) = f.parse::<u64>() else {
            if let Some(r) = run.take() {
                flush(&mut out, r);
            }
            out.push(format!("F{f}"));
            continue;
        };
        run = match run {
            Some((a, b)) if n == b + 1 => Some((a, n)),
            Some(r) => {
                flush(&mut out, r);
                Some((n, n))
            }
            None => Some((n, n)),
        };
    }
    if let Some(r) = run {
        flush(&mut out, r);
    }
    out.join(" ")
}

/// The run's exit and, under a control, the line that says whether it held: without one, 0 when
/// the dumps agree and 1 when they differ; under `--control same`, 0 when they agree; under
/// `--control plant`, 0 when they differ.
#[must_use]
pub fn verdict(report: &Report, control: Option<Control>) -> (i32, Option<String>) {
    let differs = report.differs();
    match control {
        None => (i32::from(differs), None),
        Some(Control::Same) if !differs => (
            0,
            Some("control same: held, the base is HEAD and nothing differs".into()),
        ),
        Some(Control::Same) => (
            1,
            Some(
                "control same: FAILED, the base is HEAD itself, so each difference is the \
                 harness's own (a build path, a clock or an unseeded draw reaching the words)"
                    .into(),
            ),
        ),
        Some(Control::Plant) if differs => (
            0,
            Some(format!(
                "control plant: held, the planted BODY_FRAME_TOP is reported \
                 (differing_compared={} state_differing={})",
                report.differing_compared, report.state_differing
            )),
        ),
        Some(Control::Plant) => (
            1,
            Some(
                "control plant: FAILED, the base draws BODY_FRAME_TOP one lower and no frame \
                 differs (do the scripts draw a full ribbon body?)"
                    .into(),
            ),
        ),
    }
}

// ---- The run --------------------------------------------------------------------------------

/// `git -C <dir> <args>`'s standard output; the lab's environment (the owner's git config, for
/// the submodule's clone) with no prompt.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The commit `name` resolves to in `repo`.
fn commit(repo: &Path, name: &str) -> Result<String, String> {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ],
    )
    .map(|s| s.trim().to_string())
}

/// The head's scripts: each file named, and every `.trail` under each folder named (with none
/// named, under [`CORPUS`]), sorted within its folder; each as the dumps are given it, relative
/// to the head when it lies there.
fn scripts(named: &[String], head: &Path) -> Result<Vec<String>, Fail> {
    let corpus = head.join(CORPUS);
    let roots: Vec<PathBuf> = if named.is_empty() {
        if !corpus.is_dir() {
            return Err(Fail::usage(format!(
                "trail-diff: no scripts named, and the corpus ({}) is not there yet (P0.6): name \
                 .trail files or folders",
                corpus.display()
            )));
        }
        vec![corpus]
    } else {
        named
            .iter()
            .map(|s| PathBuf::from(from_caller(s)))
            .collect()
    };
    let mut out = Vec::new();
    for root in roots {
        if root.is_dir() {
            let mut found = Vec::new();
            trails(&root, &mut found);
            if found.is_empty() {
                return Err(Fail::usage(format!(
                    "trail-diff: {} holds no .trail script",
                    root.display()
                )));
            }
            found.sort();
            out.extend(found);
        } else if root.is_file() {
            out.push(root);
        } else {
            return Err(Fail::usage(format!(
                "trail-diff: {}: no such script or folder",
                root.display()
            )));
        }
    }
    Ok(out
        .into_iter()
        .map(|p| {
            let p = physical(&p);
            p.strip_prefix(head)
                .map_or_else(|_| p.display().to_string(), |r| r.display().to_string())
        })
        .collect())
}

/// Every `*.trail` file under `dir`, links not followed.
fn trails(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => trails(&p, out),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "trail") => out.push(p),
            _ => {}
        }
    }
}

/// The base's worktree under the lab state dir, removed on drop unless kept.
struct BaseTree {
    repo: PathBuf,
    path: PathBuf,
    keep: bool,
}

impl BaseTree {
    /// Stand the base at `sha` in `path`: a worktree this repository already lists there (one a
    /// `--keep` run left) is checked out at `sha` with its own changes dropped, else it is added
    /// afresh (`--force`, over a listing whose folder is gone, so no other worktree's listing is
    /// pruned); then its `vendor/astream` is brought to the base's pin.
    fn stand(repo: &Path, path: &Path, sha: &str, keep: bool) -> Result<BaseTree, Fail> {
        let fail = |e: String| not_run(format!("the base worktree: {e}"));
        let listed = git(repo, &["worktree", "list", "--porcelain"]).map_err(fail)?;
        let want = physical(path);
        let is_listed = listed
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .any(|l| physical(Path::new(l)) == want);
        let tree = || BaseTree {
            repo: repo.to_path_buf(),
            path: path.to_path_buf(),
            keep,
        };
        let tree = if is_listed && path.is_dir() {
            let tree = tree();
            git(path, &["checkout", "--quiet", "--detach", "--force", sha]).map_err(fail)?;
            git(path, &["clean", "-ffdq"]).map_err(fail)?;
            tree
        } else {
            if std::fs::symlink_metadata(path).is_ok() {
                // What a run cut short left in the lab's own folder: regenerated below.
                std::fs::remove_dir_all(path)
                    .map_err(|e| fail(format!("{}: {e}", path.display())))?;
            }
            let p = path.to_string_lossy();
            git(
                repo,
                &["worktree", "add", "--quiet", "--force", "--detach", &p, sha],
            )
            .map_err(fail)?;
            tree()
        };
        git(
            path,
            &[
                "submodule",
                "update",
                "--quiet",
                "--init",
                "--force",
                "vendor/astream",
            ],
        )
        .map_err(fail)?;
        Ok(tree)
    }
}

impl Drop for BaseTree {
    fn drop(&mut self) {
        if self.keep {
            println!(
                "trail-diff: the base worktree stays at {}",
                self.path.display()
            );
            return;
        }
        let p = self.path.to_string_lossy();
        if let Err(e) = git(&self.repo, &["worktree", "remove", "--force", &p]) {
            eprintln!("aterm-lab: trail-diff: {e}");
        }
    }
}

/// Build the dump in `tree` into `target`: the example's path.
fn build(tree: &Path, target: &Path, side: &str) -> Result<PathBuf, Fail> {
    let t = target.to_string_lossy();
    let status = crate::util::targo(&[
        "--unverified",
        "build",
        "-p",
        "aterm-gui",
        "--features",
        "bench-support",
        "--example",
        "trail_dump",
        "--profile",
        PROFILE,
        "--target-dir",
        &t,
    ])
    .current_dir(tree)
    .stdin(aterm_uds::spawnfd::Stdio::null())
    .status()
    .map_err(|e| not_run(format!("the {side} build: targo: {e}")))?;
    if !status.success() {
        return Err(not_run(format!(
            "the {side} build failed ({status}) in {}",
            tree.display()
        )));
    }
    let bin = target.join(PROFILE).join("examples").join("trail_dump");
    if !crate::sys::executable(&bin.to_string_lossy()) {
        return Err(not_run(format!(
            "the {side} build left no {}",
            bin.display()
        )));
    }
    Ok(bin)
}

/// Run each script on both builds, `jobs` at a time per build, each dump's output in `out`:
/// each build's output, script by script.
fn dump_all(
    bins: [&Path; 2],
    scripts: &[String],
    head: &Path,
    out: &Path,
    jobs: usize,
) -> Result<[String; 2], Fail> {
    let tasks: Vec<(usize, usize)> = (0..scripts.len()).flat_map(|i| [(0, i), (1, i)]).collect();
    let next = std::sync::Mutex::new(tasks.into_iter());
    let failed = std::sync::Mutex::new(Vec::<String>::new());
    let path = |side: usize, i: usize, ext: &str| {
        out.join(format!("{}-{i}.{ext}", ["head", "base"][side]))
    };
    std::thread::scope(|s| {
        for _ in 0..jobs * 2 {
            s.spawn(|| {
                loop {
                    let Some((side, i)) = next.lock().unwrap_or_else(|p| p.into_inner()).next()
                    else {
                        return;
                    };
                    let ran = (|| -> Result<(), String> {
                        let stdout = std::fs::File::create(path(side, i, "dump"))
                            .map_err(|e| e.to_string())?;
                        let stderr = std::fs::File::create(path(side, i, "err"))
                            .map_err(|e| e.to_string())?;
                        let status = aterm_uds::spawnfd::command(bins[side])
                            .arg(&scripts[i])
                            .current_dir(head)
                            .stdin(aterm_uds::spawnfd::Stdio::null())
                            .stdout(stdout)
                            .stderr(stderr)
                            .status()
                            .map_err(|e| e.to_string())?;
                        if status.success() {
                            Ok(())
                        } else {
                            let err =
                                std::fs::read_to_string(path(side, i, "err")).unwrap_or_default();
                            Err(format!("{status}: {}", err.trim()))
                        }
                    })();
                    if let Err(e) = ran {
                        failed
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(format!(
                                "the {} dump of {}: {e}",
                                ["head", "base"][side],
                                scripts[i]
                            ));
                    }
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap_or_else(|p| p.into_inner());
    if !failed.is_empty() {
        return Err(not_run(failed.join("; ")));
    }
    let mut dumps = [String::new(), String::new()];
    for (side, dump) in dumps.iter_mut().enumerate() {
        for i in 0..scripts.len() {
            let p = path(side, i, "dump");
            let text = std::fs::read_to_string(&p)
                .map_err(|e| not_run(format!("{}: {e}", p.display())))?;
            dump.push_str(&text);
        }
    }
    Ok(dumps)
}

/// Seconds, to one decimal.
fn secs(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

/// `trail-diff`.
/// # Errors
/// A usage error, or NOT RUN: a closure that differs, a base that cannot be stood, a build or a
/// dump that fails, a dump that does not parse.
pub fn cmd_trail_diff(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let a = parse_args(args)?;
    let started = Instant::now();
    let head = physical(&paths().repo);
    let scripts = scripts(&a.scripts, &head)?;
    let head_sha = commit(&head, "HEAD").map_err(|e| not_run(format!("the head: {e}")))?;
    let dirty = !git(&head, &["status", "--porcelain", "--untracked-files=no"])
        .map_err(not_run)?
        .trim()
        .is_empty();
    let (base_name, base_sha) = match (&a.control, &a.base) {
        (Some(_), _) => ("HEAD".to_string(), head_sha.clone()),
        (None, Base::Ref(r)) => (
            r.clone(),
            commit(&head, r).map_err(|e| Fail::usage(format!("trail-diff: --base {r}: {e}")))?,
        ),
        (None, Base::MergeBase) => (
            format!("{MERGE_BASE} with {MAIN}"),
            git(&head, &["merge-base", "HEAD", MAIN])
                .map(|s| s.trim().to_string())
                .map_err(|e| not_run(format!("--base {MERGE_BASE}: {e}")))?,
        ),
    };
    if a.control.is_some() && dirty {
        return Err(not_run(
            "a control's base is HEAD, and the head's working tree differs from HEAD: commit the \
             changes, or run the control from a clean checkout",
        ));
    }
    let changed = git(
        &head,
        &["diff", "--name-only", "--no-renames", "-z", &base_sha, "--"],
    )
    .map_err(not_run)?;
    let untracked =
        git(&head, &["ls-files", "--others", "--exclude-standard", "-z"]).map_err(not_run)?;
    let base_toml = git(&head, &["show", &format!("{base_sha}:Cargo.toml")]).map_err(not_run)?;
    let head_toml = std::fs::read_to_string(head.join("Cargo.toml"))
        .map_err(|e| not_run(format!("the head's Cargo.toml: {e}")))?;
    let members = closure_differs(&changed, &untracked, &base_toml, &head_toml);
    if !members.is_empty() {
        return Err(closure_refusal(&members));
    }
    let short = |s: &str| s.get(..10).unwrap_or(s).to_string();
    println!(
        "trail-diff: head {}{} base {} ({base_name}){}, {} script{}",
        short(&head_sha),
        if dirty { " and its working tree" } else { "" },
        short(&base_sha),
        match a.control {
            Some(Control::Same) => " under --control same",
            Some(Control::Plant) => " under --control plant",
            None => "",
        },
        scripts.len(),
        if scripts.len() == 1 { "" } else { "s" }
    );

    let dir = paths().state.join("trail-diff");
    std::fs::create_dir_all(&dir).map_err(|e| not_run(format!("{}: {e}", dir.display())))?;
    let _lock = lock(&dir.join("lock"), &head)?;
    // The last run's dumps, which this run's replace.
    let out = dir.join("out");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).map_err(|e| not_run(format!("{}: {e}", out.display())))?;
    let base_path = dir.join("base");
    let _tree = BaseTree::stand(&head, &base_path, &base_sha, a.keep)?;
    if a.control == Some(Control::Plant) {
        let file = base_path.join(PLANT_FILE);
        let text = std::fs::read_to_string(&file)
            .map_err(|e| not_run(format!("the plant: {}: {e}", file.display())))?;
        let planted = plant(&text).ok_or_else(|| {
            not_run(format!(
                "the plant: {} declares no `{PLANT_CONST}<number>;`",
                file.display()
            ))
        })?;
        std::fs::write(&file, planted)
            .map_err(|e| not_run(format!("the plant: {}: {e}", file.display())))?;
    }

    let t = Instant::now();
    let head_bin = build(&head, &dir.join("head.noindex"), "head")?;
    let head_built = t.elapsed();
    let t = Instant::now();
    let base_bin = build(&base_path, &dir.join("base.noindex"), "base")?;
    let base_built = t.elapsed();
    let jobs = a
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1)))
        .min(scripts.len());
    let t = Instant::now();
    let [head_dump, base_dump] = dump_all([&head_bin, &base_bin], &scripts, &head, &out, jobs)?;
    let dumped = t.elapsed();

    let parsed = |side: &str, text: &str| {
        parse_dump(text).map_err(|e| not_run(format!("the {side} dump does not parse: {e}")))
    };
    let report = compare(&parsed("head", &head_dump)?, &parsed("base", &base_dump)?);
    print!("{}", report.render());
    let (code, said) = verdict(&report, a.control);
    if let Some(said) = said {
        println!("{said}");
    }
    println!(
        "trail-diff: built the head in {} and the base in {}, dumped in {} ({jobs} at a time per \
         build), {} in all; the dumps are in {} (rerun a frame with <dump> --full --frame N \
         <script>: {}, {})",
        secs(head_built),
        secs(base_built),
        secs(dumped),
        secs(started.elapsed()),
        out.display(),
        head_bin.display(),
        base_bin.display()
    );
    Ok(code)
}

/// The run's lock in the lab state dir: one trail-diff at a time per checkout, since each one
/// stands the same base worktree and builds into the same target directories.
fn lock(path: &Path, head: &Path) -> Result<named_lock::NamedLock, Fail> {
    let me = named_lock::Holder::me("lab trail-diff", head);
    let taken = named_lock::take(
        path,
        &me,
        Wait::Block,
        &mut |holder, _| {
            println!(
                "trail-diff: waiting for pid {}, which holds {}",
                holder.map_or_else(|| "?".to_string(), |h| h.pid.to_string()),
                path.display()
            );
        },
        &|| false,
    );
    match taken {
        Ok(held) => Ok(held),
        Err(Refused::Stopped { signal, .. }) => Err(Fail {
            stopped: signal.inspect(|sig| crate::stop::note(*sig)),
            ..not_run("interrupted while waiting for another trail-diff of this checkout")
        }),
        Err(Refused::Held(_)) => Err(not_run(
            "another trail-diff of this checkout holds the lock",
        )),
        Err(Refused::Io(e)) => Err(not_run(format!("{}: {e}", path.display()))),
    }
}

#[cfg(test)]
#[path = "trail_diff_tests.rs"]
mod tests;
