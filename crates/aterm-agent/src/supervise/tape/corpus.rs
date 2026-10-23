// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A CORPUS OF HOSTED-LOOP TAPES (the replay plan's T10): runs of the window's own harness, recorded in the real-agent lab (`targo --unverified lab`, `crates/aterm-lab`) against the real Claude Code, kept in the checkout (`crates/aterm-agent/tests/testdata/tapes/`) and replayed by every build (`tests/tape_corpus.rs`).

use std::path::{Path, PathBuf};

use astream_effects::EffectRecord;

use super::acts::{ActClass, Candidate, act_class};
use super::codec::{self, End, Item};
use super::replay::{self, Options};
use super::report::{Report, Verdict};
use super::show::{self, Tape};
use super::{doors, text};
use crate::supervise::CtlReply;

/// The most text a corpus tape may be: a tape a person reads before it is committed, and a corpus every build replays.
pub const TEXT_MAX: usize = 512 * 1024;

/// A corpus tape's file name ends so: its text form.
pub const TEXT_SUFFIX: &str = ".atape.txt";

/// The fixed tokens a [`Scrub`] writes in place of the recording machine's names.
pub mod token {
    /// The lab's state directory (`$LAB_STATE`): every run's scratch home, state root and project under it.
    pub const STATE: &str = "/lab/state";
    /// The checkout the lab was built from.
    pub const CHECKOUT: &str = "/lab/checkout";
    /// The recording machine's home.
    pub const HOME: &str = "/lab/home";
    /// The recording machine's temporary directory.
    pub const TMP: &str = "/lab/tmp";
    /// The recording machine's user.
    pub const USER: &str = "lab-user";
    /// The recording machine's host.
    pub const HOST: &str = "lab-host";
}

/// Whether `path` names a tape file: `*.atape`, or its text form `*.atape.txt`.
#[must_use]
pub fn is_tape(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".atape") || n.ends_with(TEXT_SUFFIX))
}

/// The tapes `named` names, in order: each file as it is named, and each directory's tapes at any depth, in path order.
/// # Errors
/// A name that is neither a file nor a directory, or a directory that does not read whole: a check over part of a corpus would pass what it never read.
pub fn tapes_in(named: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for path in named {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !meta.is_dir() {
            out.push(path.clone());
            continue;
        }
        let mut found = Vec::new();
        let mut dirs = vec![path.clone()];
        while let Some(dir) = dirs.pop() {
            let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for entry in entries {
                let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
                let kind = entry
                    .file_type()
                    .map_err(|e| format!("{}: {e}", entry.path().display()))?;
                let at = entry.path();
                if kind.is_dir() {
                    dirs.push(at);
                } else if kind.is_file() && is_tape(&at) {
                    found.push(at);
                }
            }
        }
        found.sort();
        out.extend(found);
    }
    Ok(out)
}

/// The tape `bytes` (binary or text) replayed as a corpus replays it: in acts mode under `candidate` — the recorded policy itself for a corpus check, [`Candidate::default`] — every act held to the tape.
/// # Errors
/// A tape no replay can run, as [`replay::replay`] refuses one.
pub fn check(bytes: &[u8], candidate: &Candidate) -> Result<Report, String> {
    replay::replay(
        bytes,
        &Options {
            candidate: Some(candidate.clone()),
            ..Options::default()
        },
    )
}

/// One tape of a corpus check: where it is, and its replay's report or why it was not replayed.
#[derive(Debug, Clone)]
pub struct Checked {
    pub path: PathBuf,
    pub outcome: Result<Report, String>,
}

impl Checked {
    /// The tape's exit code: its verdict's, or 2 for a tape not replayed.
    #[must_use]
    pub fn code(&self) -> u8 {
        self.outcome.as_ref().map_or(2, Report::code)
    }
}

/// A check's exit code from each tape's: 1 when any tape's acts diverge (the answer a check is asked for), else 2 when a tape was not replayed, else 3 when one went STALE, else 4 when one reproduced its recorded panic, else 0 — every tape's acts taken as recorded.
#[must_use]
pub fn worst(codes: &[u8]) -> u8 {
    [1, 2, 3, 4]
        .into_iter()
        .find(|code| codes.contains(code))
        .unwrap_or(0)
}

/// A check as text: a line per tape — its verdict, its path and how many of its acts it took — with the whole report under a tape that did not reproduce, then the count of each verdict.
#[must_use]
pub fn render(rows: &[Checked]) -> String {
    let mut out = String::new();
    let mut tally: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for row in rows {
        let path = show::shown(&row.path.display().to_string());
        match &row.outcome {
            Ok(report) => {
                let word = report.verdict.word();
                *tally.entry(word).or_default() += 1;
                let taken = report.acts.as_ref().map_or_else(String::new, |a| {
                    format!(" (acts {} of {})", a.account.taken, a.account.acts)
                });
                out.push_str(&format!("{word} {path}{taken}\n"));
                if report.code() != 0 {
                    for line in report.render().lines() {
                        out.push_str("    ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
            Err(e) => {
                *tally.entry("NOT REPLAYED").or_default() += 1;
                out.push_str(&format!("NOT REPLAYED {path}: {}\n", show::shown(e)));
            }
        }
    }
    let counts: Vec<String> = tally.iter().map(|(w, n)| format!("{n} {w}")).collect();
    out.push_str(&format!("{} tape(s): {}\n", rows.len(), counts.join(", ")));
    out
}

/// One name a scrub takes: its bytes, its token, and whether it is a word
/// (a user, a host) or a path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    from: Vec<u8>,
    to: String,
    word: bool,
}

/// THE RECORDING MACHINE'S NAMES, EACH AS A FIXED TOKEN ([module docs](self)).
#[derive(Debug, Clone, Default)]
pub struct Scrub {
    rules: Vec<Rule>,
}

/// Whether a path's last name goes on with `b` ([`Scrub`]).
const fn path_goes_on(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// Whether a word goes on with `b` ([`Scrub`]): an ASCII letter, or a byte of
/// a character past ASCII.
const fn word_goes_on(b: u8) -> bool {
    b.is_ascii_alphabetic() || b >= 0x80
}

/// The macOS directories under `/private` that the root's links name.
const PRIVATE: [&str; 3] = ["tmp", "var", "etc"];

impl Scrub {
    /// A scrub that takes nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the directory `from` (and macOS's other spelling of it) as `to`.
    /// # Errors
    /// A path that is not absolute UTF-8, or one too near the root to be a machine's own (`/`, `/Users`, `/tmp`): taken everywhere, it would rewrite paths that are no one's.
    pub fn path(&mut self, from: &Path, to: &str) -> Result<&mut Self, String> {
        let text = from
            .to_str()
            .ok_or_else(|| format!("{}: not UTF-8", from.display()))?;
        let text = text.trim_end_matches('/');
        let names: Vec<&str> = text.split('/').skip(1).collect();
        if !from.is_absolute() || names.len() < 2 || names.iter().any(|n| n.is_empty()) {
            return Err(format!(
                "{text:?}: a scrub takes an absolute directory at least two names below the root"
            ));
        }
        self.add(text.as_bytes(), to, false);
        match names.as_slice() {
            ["private", first, rest @ ..] if PRIVATE.contains(first) && !rest.is_empty() => {
                let bare = format!("/{}", names[1..].join("/"));
                self.add(bare.as_bytes(), to, false);
            }
            [first, ..] if PRIVATE.contains(first) => {
                self.add(format!("/private{text}").as_bytes(), to, false);
            }
            _ => {}
        }
        Ok(self)
    }

    /// Take the word `from` (a user's or a host's name) as `to`.
    /// # Errors
    /// A word shorter than three bytes, or one with a space or a `/`: taken everywhere, it would rewrite the text around it.
    pub fn word(&mut self, from: &str, to: &str) -> Result<&mut Self, String> {
        if from.len() < 3 || from.bytes().any(|b| b.is_ascii_whitespace() || b == b'/') {
            return Err(format!(
                "{from:?}: a scrub takes a word of three bytes or more, with no space or `/`"
            ));
        }
        self.add(from.as_bytes(), to, true);
        Ok(self)
    }

    /// Take `from` as `to`, and — where the press guard spells it otherwise
    /// (a `.` as `\.`, a space as `\x20`: `policy::guard::row_guard`, which
    /// writes the `key if=<re>` a press is bound to the judged row by) — its
    /// guard spelling as the token's.
    fn add(&mut self, from: &[u8], to: &str, word: bool) {
        let mut push = |from: Vec<u8>, to: String| {
            if !self.rules.iter().any(|r| r.from == from) {
                self.rules.push(Rule { from, to, word });
            }
        };
        push(from.to_vec(), to.to_string());
        let text = String::from_utf8_lossy(from);
        let guarded = crate::supervise::policy::guard::literal(&text);
        if guarded != text {
            push(
                guarded.into_bytes(),
                crate::supervise::policy::guard::literal(to),
            );
        }
    }

    /// Each name this scrub takes, and whether it is a word.
    pub fn names(&self) -> impl Iterator<Item = (&[u8], bool)> {
        self.rules.iter().map(|r| (r.from.as_slice(), r.word))
    }

    /// `input` with every name taken ([`Scrub`]).
    #[must_use]
    pub fn bytes(&self, input: &[u8]) -> Vec<u8> {
        let mut rules: Vec<&Rule> = self.rules.iter().collect();
        rules.sort_by_key(|r| std::cmp::Reverse(r.from.len()));
        let mut out = Vec::with_capacity(input.len());
        let mut at = 0;
        'scan: while at < input.len() {
            for rule in &rules {
                if !input[at..].starts_with(&rule.from) {
                    continue;
                }
                let after = input.get(at + rule.from.len()).copied();
                let fits = if rule.word {
                    let before = at.checked_sub(1).map(|b| input[b]);
                    !before.is_some_and(word_goes_on) && !after.is_some_and(word_goes_on)
                } else {
                    !after.is_some_and(path_goes_on)
                };
                if fits {
                    out.extend_from_slice(rule.to.as_bytes());
                    at += rule.from.len();
                    continue 'scan;
                }
            }
            out.push(input[at]);
            at += 1;
        }
        out
    }

    /// `input` with every name taken.
    #[must_use]
    pub fn text(&self, input: &str) -> String {
        String::from_utf8_lossy(&self.bytes(input.as_bytes())).into_owned()
    }

    /// The tape `header` and `items` with every name taken: the header's text, and each item's ([module docs](self)).
    /// # Errors
    /// A control round trip whose request or reply does not read: its words cannot be rewritten without its lengths.
    pub fn tape(
        &self,
        header: Option<&str>,
        items: &[Item],
    ) -> Result<(Option<String>, Vec<Item>), String> {
        let items = items
            .iter()
            .enumerate()
            .map(|(at, item)| self.item(at, item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((header.map(|h| self.text(h)), items))
    }

    fn item(&self, at: usize, item: &Item) -> Result<Item, String> {
        Ok(match item {
            Item::Effect { record, stamp } => Item::Effect {
                record: self.record(at, record)?,
                stamp: *stamp,
            },
            Item::Note { kind, text } => Item::Note {
                kind: kind.clone(),
                text: self.text(text),
            },
            Item::Mark(mark) => Item::Mark(mark.clone()),
            Item::End(End::Returned { how }) => Item::End(End::Returned {
                how: self.text(how),
            }),
            Item::End(End::Unwound { message }) => Item::End(End::Unwound {
                message: self.text(message),
            }),
            Item::End(End::Suspended) => Item::End(End::Suspended),
        })
    }

    fn record(&self, at: usize, record: &EffectRecord) -> Result<EffectRecord, String> {
        Ok(match record {
            EffectRecord::Exchange {
                door,
                request,
                reply,
            } if door == doors::CTL || door == doors::TELLER => {
                let bad = |what: &str, e: codec::CodecError| {
                    format!("item {at} ({door}): a {what} that does not read: {e}")
                };
                let words: Vec<String> = codec::ctl_args(request)
                    .map_err(|e| bad("request", e))?
                    .iter()
                    .map(|w| self.text(w))
                    .collect();
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                let reply = match codec::ctl_reply_of(reply).map_err(|e| bad("reply", e))? {
                    Ok(CtlReply {
                        code,
                        stdout,
                        stderr,
                    }) => Ok(CtlReply {
                        code,
                        stdout: self.text(&stdout),
                        stderr: self.text(&stderr),
                    }),
                    Err(why) => Err(self.text(&why)),
                };
                EffectRecord::Exchange {
                    door: door.clone(),
                    request: codec::ctl_request(&words),
                    reply: codec::ctl_reply(&reply),
                }
            }
            EffectRecord::Exchange {
                door,
                request,
                reply,
            } => EffectRecord::Exchange {
                door: door.clone(),
                request: self.bytes(request),
                reply: self.bytes(reply),
            },
            EffectRecord::Emit { door, bytes } => EffectRecord::Emit {
                door: door.clone(),
                bytes: self.bytes(bytes),
            },
            EffectRecord::File { path, bytes } => EffectRecord::File {
                path: self.text(path),
                bytes: self.bytes(bytes),
            },
            EffectRecord::FileErr { path, kind } => EffectRecord::FileErr {
                path: self.text(path),
                kind: *kind,
            },
            other => other.clone(),
        })
    }
}

/// Where `needle` stands in `text`, ASCII case aside: the 1-based line of
/// each place.
fn found(text: &str, needle: &[u8], word: bool) -> Vec<usize> {
    let hay = text.as_bytes();
    let mut lines = Vec::new();
    if needle.is_empty() || needle.len() > hay.len() {
        return lines;
    }
    for at in 0..=hay.len() - needle.len() {
        if !hay[at..at + needle.len()].eq_ignore_ascii_case(needle) {
            continue;
        }
        if word {
            let before = at.checked_sub(1).map(|b| hay[b]);
            let after = hay.get(at + needle.len()).copied();
            if before.is_some_and(word_goes_on) || after.is_some_and(word_goes_on) {
                continue;
            }
        }
        lines.push(hay[..at].iter().filter(|&&b| b == b'\n').count() + 1);
    }
    lines
}

/// What the tape text `text` holds that a corpus may not ([module docs](self)): `/Users/`, the recording machine's `home`, any name `scrub` takes, each with the line it is first on and how often; and a text over [`TEXT_MAX`].
#[must_use]
pub fn lint(text: &str, scrub: &Scrub, home: Option<&Path>) -> Vec<String> {
    let mut problems = Vec::new();
    if text.len() > TEXT_MAX {
        problems.push(format!(
            "{} bytes of text, past the corpus's {} KiB",
            text.len(),
            TEXT_MAX / 1024
        ));
    }
    let mut needles: Vec<(Vec<u8>, bool)> = vec![(b"/Users/".to_vec(), false)];
    if let Some(home) = home.and_then(Path::to_str).map(|h| h.trim_end_matches('/'))
        && !home.is_empty()
    {
        needles.push((home.as_bytes().to_vec(), false));
    }
    needles.extend(scrub.names().map(|(n, w)| (n.to_vec(), w)));
    let mut seen = Vec::new();
    for (needle, word) in needles {
        if seen.contains(&needle) {
            continue;
        }
        let lines = found(text, &needle, word);
        if let Some(first) = lines.first() {
            problems.push(format!(
                "{:?} on line {first} ({} time(s))",
                String::from_utf8_lossy(&needle),
                lines.len()
            ));
        }
        seen.push(needle);
    }
    problems
}

/// The acts on a tape's `items`, in order: each act's door and request ([`act_class`]).
#[must_use]
pub fn acts(items: &[Item]) -> Vec<(String, Vec<u8>)> {
    items
        .iter()
        .filter_map(|item| match item.record() {
            Some(EffectRecord::Exchange { door, request, .. })
                if act_class(door, request) == ActClass::Act =>
            {
                Some((door.clone(), request.clone()))
            }
            _ => None,
        })
        .collect()
}

/// What a tape holds that a corpus is kept for ([module docs](self)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Census {
    /// Decisions on an approval box.
    pub approvals: usize,
    /// Decisions on a question box.
    pub questions: usize,
    /// Decisions at a turn's end.
    pub turn_ends: usize,
    /// Turn ends decided at a WALL: the judged screen shows one (Claude Code's reader of it, `aterm_phase::wall::wall`), or the rule that decided is a wall's own continuation (a retry, a resume, a compact, a login).
    pub walls: usize,
    /// The acts on it ([`acts`]).
    pub acts: usize,
}

impl std::ops::AddAssign for Census {
    fn add_assign(&mut self, other: Self) {
        self.approvals += other.approvals;
        self.questions += other.questions;
        self.turn_ends += other.turn_ends;
        self.walls += other.walls;
        self.acts += other.acts;
    }
}

impl std::fmt::Display for Census {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} approval(s), {} question(s), {} turn end(s), {} wall(s), {} act(s)",
            self.approvals, self.questions, self.turn_ends, self.walls, self.acts
        )
    }
}

/// What `tape` holds ([`Census`]).
#[must_use]
pub fn census(tape: &Tape) -> Census {
    let mut census = Census {
        acts: acts(&tape.items).len(),
        ..Census::default()
    };
    for decision in show::decisions(tape) {
        match decision.door.as_str() {
            doors::DECIDE_APPROVAL => census.approvals += 1,
            doors::DECIDE_QUESTION => census.questions += 1,
            doors::DECIDE_TURN_END => {
                census.turn_ends += 1;
                let shown = decision
                    .screen
                    .as_deref()
                    .is_some_and(|rows| aterm_phase::wall::wall(rows).is_some());
                let rule = decision
                    .rule
                    .as_deref()
                    .is_some_and(crate::supervise::policy::turn_end::is_wall_retry);
                if shown || rule {
                    census.walls += 1;
                }
            }
            _ => {}
        }
    }
    census
}

/// A tape made a corpus tape ([`promote`]).
#[derive(Debug, Clone)]
pub struct Promoted {
    /// Its text form, scrubbed: what the corpus keeps.
    pub text: String,
    /// What it holds.
    pub census: Census,
    /// Its effects, every one of them consumed by each replay.
    pub effects: usize,
}

/// The requests a replay's loop asked that are acts, in order, each word
/// passed through `map`.
fn acts_asked(report: &Report, map: impl Fn(&str) -> String) -> Vec<Vec<String>> {
    report
        .requests
        .iter()
        .filter(|words| {
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            act_class(doors::CTL, &codec::ctl_request(&words)) == ActClass::Act
        })
        .map(|words| words.iter().map(|w| map(w)).collect())
        .collect()
}

/// THE TAPE `bytes` MADE A CORPUS TAPE, or refused ([module docs](self)): whole and ended, and REPRODUCED by a strict replay on this build as it was recorded; scrubbed by `scrub` into its text form; that text passing [`lint`] (`home`: the recording machine's), replaying strictly REPRODUCED, replaying in acts mode under its own policy REPRODUCED with every act on it taken, and its replay asking the very acts the original's replay asked, each in the scrub's words.
/// # Errors
/// Why the tape is not a corpus tape — each failed condition named, with the replay's report where a replay said so.
pub fn promote(bytes: &[u8], scrub: &Scrub, home: Option<&Path>) -> Result<Promoted, String> {
    let tape = show::open(bytes, false)?;
    if let Some(tail) = show::tail_note(&tape) {
        return Err(format!("{tail}: a corpus keeps whole runs"));
    }
    if !tape.items.iter().any(|i| matches!(i, Item::End(_))) {
        return Err(
            "the tape has no end: its loop was cut off, and replays only as far as it goes"
                .to_string(),
        );
    }
    let indented = |report: &Report| {
        report
            .render()
            .lines()
            .map(|l| format!("\n    {l}"))
            .collect::<String>()
    };
    let original = replay::replay(bytes, &Options::default())?;
    if original.verdict != Verdict::Reproduced {
        return Err(format!(
            "the tape does not replay as recorded on this build:{}",
            indented(&original)
        ));
    }
    let (header, items) = scrub.tape(tape.header.as_deref(), &tape.items)?;
    let text = text::render(header.as_deref(), &items);
    let problems = lint(&text, scrub, home);
    if !problems.is_empty() {
        return Err(format!(
            "the scrubbed tape still holds what a corpus may not: {}",
            problems.join("; ")
        ));
    }
    let strict = replay::replay(text.as_bytes(), &Options::default())?;
    if strict.verdict != Verdict::Reproduced {
        return Err(format!(
            "the scrubbed tape does not replay as the original did:{}",
            indented(&strict)
        ));
    }
    let checked = check(text.as_bytes(), &Candidate::default())?;
    let taken = checked
        .acts
        .as_ref()
        .is_some_and(|a| a.account.taken == a.account.acts);
    if checked.verdict != Verdict::Reproduced || !taken {
        return Err(format!(
            "the scrubbed tape does not replay in acts mode with every act taken:{}",
            indented(&checked)
        ));
    }
    let recorded = acts_asked(&original, |w| scrub.text(w));
    let replayed = acts_asked(&strict, str::to_string);
    if recorded != replayed {
        return Err(format!(
            "the scrubbed tape's replay takes other acts than the original's: {replayed:?}, not \
             {recorded:?}"
        ));
    }
    let scrubbed = show::open(text.as_bytes(), false)?;
    Ok(Promoted {
        census: census(&scrubbed),
        effects: strict.effects,
        text,
    })
}

#[cfg(test)]
#[path = "corpus_tests.rs"]
mod tests;
