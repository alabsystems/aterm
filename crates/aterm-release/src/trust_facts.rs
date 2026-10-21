// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FACTS, NOT VERDICTS (Trust P0 — `$HOME/trust`
//! `docs/design/2026-09-29-incremental-deterministic-trust-results.md` §4.8 and
//! §5; no flag bypasses a check).
//!
//! The whole-tree Trust lane ([`crate::trust_lane`]) records what it
//! MEASURED — per library, the Summary, the exit status, what trustd refused,
//! what lapsed, what ran out of memory ([`Measured`]) — and every verdict is
//! JUDGED AT ASSEMBLY ([`judge`]) from those facts and the ratchet of the tree
//! being judged. A filed verdict would key on the floors too, so a floors-only
//! commit would need a whole new run. One judge: the lane prints its own
//! verdict with it ([`conclude`]), `ship trust run --remeasure` reads the stored
//! work it leaves undecided ([`pending_for`]), and `targo --unverified ship
//! trust judge` prints its whole report.
//!
//! THE KEY ([`facts_key`]). A facts record is filed under a key over
//! everything that determines what the lane measured, except the files
//! `--update-ratchet` rewrites, which no verification reads:
//!
//! * the tree with the entries of the ratchet, the memory record and the
//!   cost-history seed ([`COSTS`], which only orders a run) removed
//!   ([`tree_sans_floors`]) — the code, the lane, its queued forks, the
//!   gate-list derivation and this judge are all in it. The
//!   memory record ([`MEMORY`]) IS an input to the run: it
//!   decides which libraries a host refuses up front. So the facts carry, per
//!   library, the memory row the run used ([`Crate::memory`]) and the decision
//!   it made, and the judge reads those rows, never the judged commit's: a
//!   memory row can only turn a library into REFUSED(memory) or CAP-BOUND,
//!   neither of which is ever a pass, so no row can verify what the facts did
//!   not measure. `--update-ratchet` rewrites both files, and the commit that
//!   records them still reuses the run's facts;
//! * the prover: atpkg's `tree_root` over the whole toolchain directory the
//!   pinned driver runs from ([`prover_identity`]) — every binary and every
//!   sysroot rlib, not one trustc commit-hash;
//! * the lane: the whole-tree mode.
//!
//! THE ENVIRONMENT IS THE LANE'S, NOT THE CALLER'S. The
//! lane runs every compile — the judge, the graph, each check — in an
//! environment it BUILDS from a list (the script's "THE LANE'S ENVIRONMENT"):
//! its pins (`CARGO_INCREMENTAL`, `TRUST_NO_MIGRATE_WARN`), which are in the
//! tree the key covers, and a few variables it passes from the host (`HOME`,
//! `TMPDIR`, …), whose values no key covers. Nothing else the caller exported
//! reaches a compile: not `TRUSTFLAGS` (targo-trust's per-run `-Ztrust-*`
//! overrides), not a `TARGO_*` twin of a cargo setting, not a value an
//! `option_env!` compiles into a verified library. What is left is CHECKED
//! from what the compiles themselves recorded ([`conclude`]): no dep-info may
//! name the ratchet, and no `# env-dep:` line may carry a value the key does
//! not determine — a passed variable read at all, or a value neither cargo, a
//! build script nor a lane pin supplied.
//!
//! A record from another key is never read: the assembler computes the key of
//! the commit it judges and opens that file, and a record whose own lines name
//! another key is refused. A floors-only commit has the same key, so its
//! verdict needs no new run; a change to any other file, or another prover, is
//! another key. What a floors-only commit may do to the floors is the ratchet's
//! up-only rule, held here against the floors of the commit the facts were
//! measured on ([`floors_lowered`]): reused facts never judge a floor lowered,
//! or re-labelled to another prover, that the rule would refuse.
//!
//! NO FLOOR EXCUSES (the owner's ruling: no report-only path). A
//! library below its floor is a finding or it is undecided — never a pass:
//!
//! * a floor ANOTHER prover recorded is held like any other: a new prover
//!   meets the floors it inherits, or the row is lowered onto it with a reason
//!   in its fifth column (`ship trust judge --update-ratchet --reason`), which
//!   the up-only rule then admits. `--update-ratchet` raises floors and
//!   corrects totals; it never lowers a floor without that reason;
//! * a shortfall the run's lapsed wall-clock budgets (`timed_out=`) could
//!   cover is UNDECIDED — `NOT-VERIFIED(clock)`, COULD NOT RUN, no pass — as a
//!   shortfall trustd's refusals could cover is. It is resolved by measuring
//!   again, not by an excuse: `ship trust run --remeasure` runs only
//!   the libraries the stored facts leave undecided, on the same key, and
//!   [`remeasured`] folds their new facts into the record ([`conclude`]). The
//!   proof checks ay's caller cancelled ([`CHECK_CANCELLED`]) are credited the
//!   same way ONCE per library and key — in a filed record's first
//!   measurement, never in a re-measure or a run that files nothing — because
//!   a passing run prints them too, so only a measurement made again can tell
//!   their clock from the code ([`clock_credit`]).
//!
//! A CHECK THAT DID NOT FINISH IS NOT VERIFIED. targo-trust prints a Summary
//! of whatever its compiler reported so far: an aterm-census check whose trustc
//! was SIGTERMed after 91 CPU-minutes left a parseable `Summary: 207 proved …
//! (2037 total)` and exit 101, as a finished advisory run can, and the stage
//! printed `ok`.
//! So a check is FINISHED only when its log carries targo-trust's own word that
//! the compiler covered the whole crate — `Coverage: <n>/<n> eligible function
//! bodies verified (complete)`, which targo-trust prints only from the
//! compiler's end-of-walk `coverage_summary` row ([`COVERAGE_COMPLETE`]) — and
//! nothing in its record says it was cut short: a targo-trust `transport:*` row
//! other than `missing-json`, a compiler cargo saw end by a signal, or the
//! check's own exit by one (128+N). Any of those is [`Measured::ended`]; the
//! lane's per-crate wall budget ending the check is [`Measured::wall_budget`].
//! Either is a red row — `NOT-VERIFIED(incomplete)` or `OVER-BUDGET(<n>s)` —
//! that fails the run: never scored, in neither total, never a floor written,
//! re-based or corrected from it.
//!
//! A PATH TARGO REFUSED IS THE HOST'S. `targo trust` will not execute through a
//! path another principal could swap: a group- or world-writable component
//! (a sticky bit exempts nothing), one owned by neither root nor the caller, a
//! symlink on the way that another owns, one another owns that an ACL still
//! lets the caller write, or a path trustd will not serve from (a foreign-owned
//! or cross-user writable ancestor, or a runtime directory or executable that is
//! not what it demands). The check then verifies
//! nothing, and on 2026-10-16 a whole 22-library run under umask 0002 failed
//! with nothing but `NOT-VERIFIED(rc=2)` rows to show for it. The log names the
//! path in targo's own words ([`Measured::path_refused`]); each row keeps its
//! status, and the report adds one `REFUSED PATH:` line per path, naming every
//! library it failed and what to do. The refusal says nothing about the code,
//! so a run in which targo refused a path files no record ([`filing`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use aterm_digest::Sha256;

use crate::ledger::{Error, GitRunner, Result, git_ok, rev_parse};
use crate::release::shell_word;

/// The first line of every facts record. A file without it is no record.
/// Bumped whenever a record gains a fact an older one could not state (the
/// memory rows and host, cancelled proof checks, why a check did not finish),
/// so an earlier record is never read.
pub const FACTS_MAGIC: &str = "aterm-trust facts 7";

/// The last line of every facts record: a file that stops before it was cut
/// short and is no record.
pub const FACTS_END: &str = "end";

/// The file-name prefix of a facts record in the receipt store:
/// `<store>/trust-facts-<key>`.
pub const FACTS_PREFIX: &str = "trust-facts-";

/// The first line of the plan a lane run writes in its log directory.
pub const PLAN_MAGIC: &str = "aterm-trust plan 1";

/// The floors file, relative to the repository root — one of the paths the key
/// leaves out.
pub const RATCHET: &str = "crates/aterm-release/trust/floors.tsv";

/// The lane's committed cost-history SEED, relative to the repository root: what a
/// clone with no history of its own orders a run by (`trust_lane::read_costs`), and
/// what `--update-ratchet` rewrites beside [`RATCHET`] and [`MEMORY`]. It only orders
/// a run, so the key leaves it out too.
pub const COSTS: &str = "crates/aterm-release/trust/costs.tsv";

/// The key's schema line. A change to what the key covers bumps it, and every
/// record filed before misses: the files `--update-ratchet` rewrites are left out
/// by path ([`RATCHET`], [`MEMORY`], [`COSTS`]).
const KEY_SCHEMA: &str = "aterm-trust facts key 5";

/// The lane mode a filed record measured: the whole tree, the scope list's
/// `never` rows its only exclusions. The only mode that files, and the mode
/// required by stored-facts verdicts.
pub const LANE_WHOLE_TREE: &str = "whole-tree";

/// Why nothing below reads a clock: a record is a function of its key, and
/// the key has no time in it. The date a record carries is informational.
fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// A value on one line of a record: no tab, carriage return or newline.
pub(crate) fn one_line(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

/// Lowercase hex of a digest.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

// ---------------------------------------------------------------------------
// WHAT ONE CHECK MEASURED, read from its log
// ---------------------------------------------------------------------------

/// A PROOF CHECK ITS CALLER CANCELLED: ay's words when the
/// caller's stop signal ends a strict proof check ("interrupt, deadline, or
/// memory limit"; Trust's `trust-router` reads it as a wall-clock stop). The
/// line names no function and no obligation, and it is ROUTINE: aterm-codec at
/// 6673f6a31 (seal 321aaeda7, the lane's profile, source unchanged since
/// 0.100.0) printed 91-121 of them in runs that proved its full 383/551 at load
/// 13-30, and 125-144 in the five that proved 310/553 at load 41-84 (the
/// release lane's among them). Those 73 proofs were all lost in one function,
/// `base64::encode_padded` (73 of 81 proved, then 0 of 83 — every row the
/// full verifier's at 0 ms, its interval and kernel proofs gone), and the log
/// says nothing else about it: no `timed_out=`, no budget row. So a shortfall
/// these lines could cover may be the clock's, and one run cannot say — but
/// neither can any number of runs that count them, since a run that proves
/// everything prints as many: credited without end, they would leave a real
/// drop in aterm-alloc, aterm-json or aterm-http (17, 64 and 47 proved
/// against 29, 72 and 49 such lines in 0.101.0's lane) undecided forever.
/// They are therefore credited once ([`clock_credit`]): a filed record's first
/// measurement may be undecided by them, and the `--remeasure` that resolves it
/// is judged on `timed_out=` alone. Trust counting a deadline's give-up as
/// `timed_out` is the exact fix; this is the trace the log leaves until then.
pub const CHECK_CANCELLED: &str = "proof validation cancelled by the caller";

/// What one `targo trust check` left in its log, and nothing that depends on a
/// floor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    /// The exit status the lane recorded (`<log>.rc`), `?` when there is none.
    pub rc: String,
    /// Verification did not run: the log names `verification did not run` or
    /// the `transport:missing-json` row targo prints for it.
    pub transport: bool,
    /// The last line whose first non-blank word is `Summary:`, empty when
    /// there is none (`grep -E '^\s*Summary:' | tail -1`).
    pub summary: String,
    /// Obligations trustd refused admission: the `[UNKNOWN]` rows that say
    /// `memory admission failed`, else every line that does.
    pub refused: u64,
    /// What those refusals say, by kind and count ([`admission_cause`]).
    pub refused_why: String,
    /// Obligations whose wall-clock budget lapsed: every `timed_out=<n>`, summed.
    pub lapsed: u64,
    /// Proof checks ay's caller cancelled: the lines naming [`CHECK_CANCELLED`].
    /// Clock evidence only once per library and key ([`clock_credit`]).
    pub cancelled: u64,
    /// `[UNKNOWN]` result rows naming "exceeded the per-function verification
    /// budget" — a function that ran out of the verifier's memory ceiling.
    pub budget_rows: u64,
    /// Bodies the memory census says were "skipped at the memory ceiling",
    /// summed over the census lines ([`census_skipped`]); `None` when no line
    /// names a count. The census and its ceiling are the 321aaeda7 seal's:
    /// trust main has no in-run memory ceiling (5aa81ac1683) and prints no such
    /// line, so a seal from main leaves this ABSENT — never a zero, which would
    /// say no body was skipped.
    pub skipped: Option<u64>,
    /// The ceiling the log names, as it names it: the last census line's
    /// (`3.0 GiB`), else the last `-Ztrust-verify-memory-ceiling-bytes` it
    /// carries (`3221225472 B`); empty when it names none. A seal from trust
    /// main has neither, so its logs always leave this empty: an absent
    /// ceiling, never one that held.
    pub ceiling: String,
    /// The verifier's peak, the largest any census line names (`3.5 GiB`), as
    /// it names it; empty when the log has no census line. Absent, too, on a
    /// seal from trust main: its opt-in `-Ztrust-memory-notes` census (which the
    /// lane does not ask for) names no peak in these words.
    pub peak: String,
    /// A CHECK TARGO WOULD NOT START: its log has no Summary and
    /// names targo's refusal to start without a trustd it can prove its own
    /// build's ([`TRUSTD_REFUSED`]), said by what the log adds — `dirty` (a
    /// crash epoch left DIRTY), `owned` (another trustd held the socket),
    /// `unready` (the trustd it started did not answer in time) or `other`.
    /// Empty when the check started.
    pub launch: String,
    /// What the socket held when the lane probed it after that refusal (the
    /// `<log>.trustd` line): `none`, `ours`, `foreign` or `silent`, empty when
    /// no probe was recorded …
    pub held: String,
    /// … and what the probe said of it.
    pub held_what: String,
    /// THE BUDGET (the lane's): the per-crate wall budget, in seconds, at which
    /// the lane ended this check (its `<log>.budget` mark), `?` for a mark that
    /// names none; empty when the lane did not end it.
    pub wall_budget: String,
    /// WHY THE CHECK DID NOT FINISH, as its own record says it, `; `-separated:
    /// a targo-trust `transport:*` row other than `missing-json` (which is
    /// [`Measured::transport`]), a compiler cargo saw end by a signal, the
    /// check's own exit by one (128+N), or — for a check that reported a
    /// Summary — no [`COVERAGE_COMPLETE`] line. Empty when it finished.
    pub ended: String,
    /// A PATH TARGO REFUSED (the module docs): each path the log names targo or
    /// trustd refusing to execute through ([`path_refusals`]), once, sorted.
    /// Never in a record: a run whose check names one files nothing ([`filing`]).
    pub path_refused: Vec<PathRefusal>,
}

/// A path targo refused to execute through (A PATH TARGO REFUSED IS THE HOST'S,
/// the module docs), as its message names it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PathRefusal {
    /// The path the message names: the refused component, a launch symlink or
    /// the directory holding it, or trustd's ancestor, runtime directory or
    /// executable.
    pub path: String,
    pub why: Refused,
}

/// Why targo refused a path ([`PathRefusal`]), as far as its message says.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refused {
    /// Group- or world-writable: its owner, when the message names one (targo
    /// holds a component's mode only after proving its owner root or the
    /// caller), and its mode. `link` is the launch symlink a directory holds,
    /// when that is what targo checked (its words add "without the sticky bit").
    Writable {
        owner: Option<u32>,
        mode: Option<u32>,
        link: Option<String>,
    },
    /// Owned by `owner`, which is neither root nor the invoking identity `me` (a
    /// component, a launch symlink, or the directory holding one).
    Foreign { owner: u32, me: u32 },
    /// Another principal owns it, and the caller `me` can still write it: an ACL.
    Acl { me: u32 },
    /// trustd's: an ancestor of its `authority` (`trustd executable`, `trustd
    /// socket`, `trustd runtime`) that is foreign-owned or cross-user writable;
    /// the message does not say which.
    Ancestor { authority: String },
    /// trustd's: a path that is not what trustd demands of it, in trustd's words
    /// (`trustd runtime root must be an euid-owned 0700 directory`).
    Demand { demand: String },
}

/// What to do about a refused path that is not the caller's to change: keep what
/// targo checks off it.
const REFUSED_MOVE: &str = "keep the lane's target root (--target-root) and the toolchain under directories owned by root or you and writable by their owner alone";

impl PathRefusal {
    /// What is wrong with it, for the caller `me`, as the lane's pre-flight says it
    /// (`mode 775: group- or world-writable`).
    fn said(&self, me: u32) -> String {
        match &self.why {
            Refused::Writable { owner, mode, link } => {
                let mode =
                    mode.map_or_else(|| "mode unnamed".to_string(), |m| format!("mode {m:o}"));
                let owner = owner
                    .filter(|o| *o != me)
                    .map_or_else(String::new, |o| format!(", owned by uid={o}"));
                match link {
                    Some(l) => format!(
                        "{mode}: group- or world-writable without the sticky bit{owner}, holding the launch symlink {l}"
                    ),
                    None => format!("{mode}: group- or world-writable{owner}"),
                }
            }
            Refused::Foreign { owner, me } => {
                format!("owned by uid={owner}, neither root nor you (uid={me})")
            }
            Refused::Acl { me } => {
                format!("another owns it, and an ACL lets you (uid={me}) write it")
            }
            Refused::Ancestor { authority } => {
                format!("foreign-owned or cross-user writable, by the {authority} check")
            }
            Refused::Demand { demand } => format!("trustd demands: {demand}"),
        }
    }

    /// What the caller `me` does about it: `chmod go-w` a path targo found
    /// writable beyond its owner when the owner is `me` (never a shared sticky
    /// directory, `/tmp`); a path that is another's, or that a foreign owner or an
    /// ACL puts out of the caller's reach, must hold nothing targo checks
    /// ([`REFUSED_MOVE`]). trustd's runtime directory has a fixed path (`/tmp/
    /// trustd-runtime-locks-<uid>`), which cannot move: the caller's own is
    /// `chmod`ed to what trustd demands, and another's is root's to remove (the
    /// next check makes it afresh, 0700).
    fn remedy(&self, me: u32) -> String {
        let p = &self.path;
        let chmod = format!("chmod go-w {}", shell_word(p));
        match &self.why {
            Refused::Demand { demand } if !demand.starts_with("trustd executable") => {
                let chmod = if demand.contains("0700") {
                    format!("chmod 700 {}", shell_word(p))
                } else {
                    chmod
                };
                format!(
                    "if it is yours, run: {chmod}; if not, root must remove it, and the next check makes it afresh"
                )
            }
            Refused::Writable { mode: Some(m), .. } if m & 0o1000 != 0 => format!(
                "{p} is a shared sticky directory, which targo refuses all the same, so it must hold nothing targo checks: {REFUSED_MOVE}"
            ),
            Refused::Writable { owner: Some(o), .. } if *o == me => format!("run: {chmod}"),
            Refused::Writable { owner: Some(o), .. } => {
                format!("{p} is uid={o}'s, not yours to change, so it must move: {REFUSED_MOVE}")
            }
            Refused::Writable { owner: None, .. }
            | Refused::Ancestor { .. }
            | Refused::Demand { .. } => {
                format!("if it is yours, run: {chmod}; if not, it must move: {REFUSED_MOVE}")
            }
            Refused::Foreign { .. } => {
                format!("{p} is foreign-owned, so it must move: {REFUSED_MOVE}")
            }
            Refused::Acl { .. } => {
                format!("{p} is writable through an ACL, so it must move: {REFUSED_MOVE}")
            }
        }
    }
}

/// targo's words before a path whose chain it refused (`cargo/util/
/// process_authority.rs`): `<authority> path component `<path>` …`.
const REFUSED_COMPONENT: &str = "path component `";

/// targo's words for a symlink on a path it executes through: `[the directory
/// `<dir>` holding ]<authority> launch path symlink `<link>` …`, the launcher's
/// (`bin/cargo/main.rs`, authority `protected Trust`) and each hop
/// `process_authority.rs` resolves (`verified tool launcher`, `verified
/// runtime-library directory`, …).
const REFUSED_LINK: &str = " launch path symlink `";

/// trustd's words (trust-router `coordinator.rs`): `<authority> ancestor is
/// foreign-owned or cross-user writable: <dir>`.
const REFUSED_ANCESTOR: &str = " ancestor is foreign-owned or cross-user writable: ";

/// trustd's words for a path that is not what it demands (trust-router
/// `coordinator.rs`): `<subject> must be <demand>: <path>`, each subject one it
/// checks before it serves a check.
const TRUSTD_DEMANDS: [&str; 3] = [
    "socket parent must be ",
    "trustd runtime root must be ",
    "trustd executable must be ",
];

/// The path at the start of `rest` that a trustd message ends with: up to where
/// targo-trust's wrapper goes on (`; refusing …`), or a JSON string's escaped
/// newline or close.
fn trustd_path(rest: &str) -> &str {
    let end = ["; ", "\\n", "\""]
        .iter()
        .filter_map(|t| rest.find(t))
        .min()
        .unwrap_or(rest.len());
    rest[..end].trim_end()
}

/// The text up to the next backtick, and what follows that backtick.
fn backticked(s: &str) -> Option<(&str, &str)> {
    let e = s.find('`')?;
    (e > 0).then(|| (&s[..e], &s[e + 1..]))
}

/// `0o<octal>` at the start of `s`, as targo prints a mode (`{:#o}`).
fn octal_mode(s: &str) -> Option<u32> {
    let r = s.strip_prefix("0o")?;
    let n = r.bytes().take_while(|b| (b'0'..=b'7').contains(b)).count();
    u32::from_str_radix(&r[..n], 8).ok()
}

/// ` is owned by uid=<n>, which is neither root nor the invoking identity
/// (uid=<me>)` at the start of `rest`, or a symlink hop's ` is owned by uid=<n>,
/// which is not permitted to own a redirection into the verified toolchain
/// (invoking uid=<me>)`. The sealed-launcher demand for a root owner (`… is owned
/// by uid=<n>, but TRUST_REQUIRE_SEALED_LAUNCHER demands …`, and the hop's words
/// for the caller's own link under it) is a policy the caller opted into, not this.
fn foreign_owner(rest: &str) -> Option<Refused> {
    const INVOKERS: [&str; 2] = [
        ", which is neither root nor the invoking identity (uid=",
        ", which is not permitted to own a redirection into the verified toolchain (invoking uid=",
    ];
    let r = rest.strip_prefix(" is owned by uid=")?;
    let owner = digits_at(r, 0);
    let after = &r[owner.len()..];
    let me = INVOKERS.iter().find_map(|w| after.strip_prefix(w))?;
    let (owner, me) = (owner.parse().ok()?, digits_at(me, 0).parse().ok()?);
    (owner != me).then_some(Refused::Foreign { owner, me })
}

/// `line` with its ANSI escape sequences (`ESC [ … <final>`) taken out.
fn without_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.clone().next() == Some('[') {
            chars.next();
            for d in chars.by_ref() {
                if ('@'..='~').contains(&d) {
                    break;
                }
            }
        }
    }
    out
}

/// Every path one log line says targo refused to execute through, in its own
/// words, wherever the line carries them (after cargo's `error: `, under a
/// `Caused by:`, inside a JSON string, between ANSI colour codes):
///
/// * `cargo/util/process_authority.rs`: `<authority> path component `<path>` is
///   group- or world-writable (uid=<owner>, mode=0o<mode>)`, `… is owned by
///   uid=<owner>, which is neither root nor the invoking identity (uid=<me>)`,
///   `… remains writable by Targo's effective identity (uid=<me>); ACL-backed …`;
/// * `bin/cargo/main.rs`: `the directory `<dir>` holding protected Trust launch
///   path symlink `<link>` is group- or world-writable without the sticky bit
///   (mode=0o<mode>)`, or owned by another, or the symlink itself owned by another;
/// * `process_authority.rs` again, for each symlink hop it resolves: `<authority>
///   launch path symlink `<link>` is owned by uid=<owner>, which is not permitted
///   to own a redirection into the verified toolchain (invoking uid=<me>)` (the
///   directory holding it is a path component, above);
/// * trust-router `coordinator.rs`, inside targo-trust's `could not establish the
///   selected same-sysroot trustd memory authority …` or `… private per-user
///   trustd runtime directory: …`: `<authority> ancestor is foreign-owned or
///   cross-user writable: <dir>`, and `<subject> must be <demand>: <path>`
///   ([`TRUSTD_DEMANDS`]).
///
/// Not read: the sealed-launcher demand for a root owner ([`foreign_owner`]),
/// and a path targo failed to inspect, which is no refusal.
fn path_refusals(line: &str) -> Vec<PathRefusal> {
    if !line.contains("writable") && !line.contains("owned by uid=") && !line.contains("must be ") {
        return Vec::new();
    }
    let line = without_ansi(line);
    let line = line.as_str();
    let mut out = Vec::new();
    for i in find_all(line, REFUSED_COMPONENT) {
        let Some((path, rest)) = backticked(&line[i + REFUSED_COMPONENT.len()..]) else {
            continue;
        };
        let why = if let Some(r) = rest.strip_prefix(" is group- or world-writable (uid=") {
            let owner = digits_at(r, 0);
            Some(Refused::Writable {
                owner: owner.parse().ok(),
                mode: r[owner.len()..]
                    .strip_prefix(", mode=")
                    .and_then(octal_mode),
                link: None,
            })
        } else if let Some(r) =
            rest.strip_prefix(" remains writable by Targo's effective identity (uid=")
        {
            digits_at(r, 0).parse().ok().map(|me| Refused::Acl { me })
        } else {
            foreign_owner(rest)
        };
        if let Some(why) = why {
            out.push(PathRefusal {
                path: path.to_string(),
                why,
            });
        }
    }
    for i in find_all(line, REFUSED_LINK) {
        let Some((link, rest)) = backticked(&line[i + REFUSED_LINK.len()..]) else {
            continue;
        };
        // The directory holding the link, when that is what targo checked: `the
        // directory `<dir>` holding <authority>`, the authority words between.
        let before = &line[..i];
        let holder = before
            .rfind("` holding ")
            .filter(|h| !before[h + "` holding ".len()..].contains('`'))
            .and_then(|h| {
                let d = &before[..h];
                d.rfind("the directory `")
                    .map(|s| &d[s + "the directory `".len()..])
            })
            .filter(|d| !d.is_empty());
        let writable =
            rest.strip_prefix(" is group- or world-writable without the sticky bit (mode=");
        let why = match (holder, writable) {
            (Some(_), Some(r)) => Some(Refused::Writable {
                owner: None,
                mode: octal_mode(r),
                link: Some(link.to_string()),
            }),
            _ => foreign_owner(rest),
        };
        if let Some(why) = why {
            out.push(PathRefusal {
                path: holder.unwrap_or(link).to_string(),
                why,
            });
        }
    }
    for i in find_all(line, REFUSED_ANCESTOR) {
        let path = trustd_path(&line[i + REFUSED_ANCESTOR.len()..]);
        let before = &line[..i];
        let authority = before
            .rfind("trustd")
            .map(|t| &before[t..])
            .filter(|a| a.split(' ').count() <= 2)
            .unwrap_or("trustd");
        if !path.is_empty() {
            out.push(PathRefusal {
                path: path.to_string(),
                why: Refused::Ancestor {
                    authority: authority.to_string(),
                },
            });
        }
    }
    for subject in TRUSTD_DEMANDS {
        for i in find_all(line, subject) {
            let said = &line[i..];
            let Some(colon) = said.find(": ") else {
                continue;
            };
            let path = trustd_path(&said[colon + 2..]);
            if !path.is_empty() {
                out.push(PathRefusal {
                    path: path.to_string(),
                    why: Refused::Demand {
                        demand: said[..colon].to_string(),
                    },
                });
            }
        }
    }
    out
}

/// targo-trust's words, on its `Coverage:` line, when the compiler's end-of-walk
/// `coverage_summary` row came in and every eligible body reached an outcome
/// (targo-trust `report.rs` `coverage_line`, on seal 321aaeda7 and on 6606d3bdf
/// alike): the one positive sign that a check covered its whole crate. The row
/// is sent once, after the eager whole-crate walk, so a compiler ended partway
/// never sends it, and targo-trust then prints `Coverage: unknown (compiler
/// emitted no coverage_summary transport row; …)` (measured 2026-09-29: the
/// killed aterm-census log). A body skipped at the verifier's memory ceiling is
/// still credited as processed (trustc credits a body before it is verified), so
/// a CAP-BOUND run keeps this line. A later targo-trust (trust main, CB4
/// 712ca0fff4) words a complete test-harness compile, some of whose bodies were
/// excluded rather than verified, `Coverage: <n>/<n> eligible function bodies
/// accounted for (<v> verified, <x> excluded: <tag>) (complete)`
/// ([`COVERAGE_ACCOUNTED`]): just as whole, so it is read as this line is.
pub const COVERAGE_COMPLETE: &str = "eligible function bodies verified (complete)";

/// The start of the words a later targo-trust gives a complete coverage line
/// whose crate had bodies excluded, not verified ([`COVERAGE_COMPLETE`]); the
/// line ends `) (complete)`.
pub const COVERAGE_ACCOUNTED: &str = "eligible function bodies accounted for (";

/// targo's words when it will not start a check beside a trustd it cannot
/// prove its own build's (seal 321aaeda7; the lane's `TRUSTD_REFUSED`).
pub const TRUSTD_REFUSED: &str =
    "could not establish the selected same-sysroot trustd memory authority";

/// targo's words for a trustd crash epoch left DIRTY (the lane's `TRUSTD_DIRTY`).
pub const TRUSTD_DIRTY: &str = "crash epoch is not CLEAN";

/// Every start index of `needle` in `hay`.
fn find_all(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = hay.get(from..).and_then(|s| s.find(needle)) {
        out.push(from + i);
        from += i + needle.len().max(1);
    }
    out
}

/// The maximal run of ASCII digits starting at `i`.
fn digits_at(s: &str, i: usize) -> &str {
    let rest = s.get(i..).unwrap_or("");
    let n = rest.bytes().take_while(u8::is_ascii_digit).count();
    &rest[..n]
}

/// The start of the maximal run of ASCII digits ending at `end`.
fn digits_start_before(s: &str, end: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = end;
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    i
}

/// `sed -nE 's/.*Summary: ([0-9]+) proved.*/\1/p'` and
/// `sed -nE 's/.*\(([0-9]+) total\).*/\1/p'` over one Summary line: the
/// rightmost `Summary: <n> proved` and the rightmost `(<n> total)`. `None`
/// for either that is absent or does not fit a count.
#[must_use]
pub fn parse_summary(line: &str) -> (Option<u64>, Option<u64>) {
    let proved = find_all(line, "Summary: ").into_iter().rev().find_map(|i| {
        let d = digits_at(line, i + "Summary: ".len());
        let after = i + "Summary: ".len() + d.len();
        (!d.is_empty() && line.get(after..).is_some_and(|r| r.starts_with(" proved")))
            .then(|| d.parse().ok())
            .flatten()
    });
    let total = find_all(line, "(").into_iter().rev().find_map(|i| {
        let d = digits_at(line, i + 1);
        let after = i + 1 + d.len();
        (!d.is_empty() && line.get(after..).is_some_and(|r| r.starts_with(" total)")))
            .then(|| d.parse().ok())
            .flatten()
    });
    (proved, total)
}

/// `.*memory census:.*[^0-9]([0-9]+) bodies skipped at the memory ceiling.*`
/// over one line: the rightmost count with a census marker before it. Only the
/// 321aaeda7 seal prints one; trust main's `-Ztrust-memory-notes` census counts
/// compacted slots, not skipped bodies, so a line of its gives `None`.
fn census_skipped(line: &str) -> Option<u64> {
    let census = line.find("memory census:")? + "memory census:".len();
    find_all(line, " bodies skipped at the memory ceiling")
        .into_iter()
        .rev()
        .find_map(|j| {
            let start = digits_start_before(line, j);
            // `[^0-9]` needs a character before the digits, after the marker.
            (start < j && start > census)
                .then(|| line[start..j].parse().ok())
                .flatten()
        })
}

/// `.*memory census:.* <key> ([0-9.]+ [KMGT]?i?B).*` over one line (`key`
/// ` ceiling ` or ` peak `): the rightmost such figure with a census marker
/// before it.
fn census_figure(line: &str, key: &str) -> Option<String> {
    let census = line.find("memory census:")? + "memory census:".len();
    find_all(line, key).into_iter().rev().find_map(|k| {
        if k < census {
            return None;
        }
        let at = k + key.len();
        let rest = &line[at..];
        let n = rest
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'.')
            .count();
        let unit = rest.get(n..)?.strip_prefix(' ')?;
        let mut u = 0;
        let ub = unit.as_bytes();
        if ub.first().is_some_and(|b| b"KMGT".contains(b)) {
            u += 1;
        }
        if ub.get(u) == Some(&b'i') {
            u += 1;
        }
        (n > 0 && ub.get(u) == Some(&b'B')).then(|| format!("{} {}", &rest[..n], &unit[..=u]))
    })
}

/// Each `[transport:<kind>] <message>` row a line carries — targo-trust's own
/// word that the compiler's transport was not whole (targo-trust
/// `pipeline/transport.rs`: `missing-json`, `missing-crate-summary`,
/// `summary-accounting`, `crate-summary-accounting`, `ordering`,
/// `unsupported-message`, `malformed`) — as the kind and its message, less the
/// ` (targo-trust)` tail.
fn transport_rows(line: &str) -> Vec<(String, String)> {
    const OPEN: &str = "[transport:";
    find_all(line, OPEN)
        .into_iter()
        .filter_map(|i| {
            let rest = &line[i + OPEN.len()..];
            let n = rest
                .bytes()
                .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
                .count();
            if n == 0 || rest.as_bytes().get(n) != Some(&b']') {
                return None;
            }
            let msg = &rest[n + 1..];
            let msg = msg.find(OPEN).map_or(msg, |j| &msg[..j]).trim();
            let msg = msg.strip_suffix("(targo-trust)").map_or(msg, str::trim_end);
            Some((rest[..n].to_string(), msg.to_string()))
        })
        .collect()
}

/// cargo's word that a compiler it ran ended by a signal — `process didn't exit
/// successfully: `<program> …` (signal: 15, SIGTERM: termination signal)` — as
/// the program's file name, the signal's number and its name (empty when the
/// line gives none). A compiler that exited with a status is not this.
fn compiler_signal(line: &str) -> Option<(String, u32, String)> {
    const SAID: &str = "exit successfully: `";
    const SIGNAL: &str = "(signal: ";
    let rest = &line[line.find(SAID)? + SAID.len()..];
    let program = rest.split([' ', '`']).next().unwrap_or("");
    let program = program.rsplit('/').next().unwrap_or(program);
    let s = rest.rfind(SIGNAL)? + SIGNAL.len();
    let d = digits_at(rest, s);
    let n = d.parse().ok()?;
    let name = rest[s + d.len()..]
        .strip_prefix(", ")
        .and_then(|r| r.split([':', ')']).next())
        .unwrap_or("")
        .trim();
    Some((program.to_string(), n, name.to_string()))
}

/// The portable name of signal `n`, empty for one whose number differs between
/// platforms.
fn signal_name(n: u32) -> &'static str {
    match n {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => "",
    }
}

/// `" (NAME)"` for a non-empty signal name, else nothing.
fn signal_said(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else {
        format!(" ({name})")
    }
}

/// A span of seconds as the lane says it: `1 h 30 min`, `6 h`, `45 s`.
fn span(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let mut out = Vec::new();
    if h > 0 {
        out.push(format!("{h} h"));
    }
    if m > 0 {
        out.push(format!("{m} min"));
    }
    if s > 0 || out.is_empty() {
        out.push(format!("{s} s"));
    }
    out.join(" ")
}

/// Whether a `Coverage:` line is targo-trust's word that the whole crate was
/// covered: `Coverage: <n>/<n> eligible function bodies verified (complete)`,
/// or the later `… accounted for (…) (complete)` ([`COVERAGE_ACCOUNTED`]), the
/// two counts equal.
fn coverage_complete(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix("Coverage: ") else {
        return false;
    };
    let Some((counts, words)) = rest.split_once(' ') else {
        return false;
    };
    let whole = words == COVERAGE_COMPLETE
        || (words.starts_with(COVERAGE_ACCOUNTED) && words.ends_with(") (complete)"));
    whole
        && matches!(counts.split_once('/'), Some((p, e))
            if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p == e)
}

/// What trustd's refusals say, by kind and count, in trustd's own words
/// (trust-router's coordinator): it could not be reached, it did not answer
/// before its deadline, or it answered and declined. `kind: n` pairs in that
/// order, then any other refusal.
#[must_use]
pub fn admission_cause(refusals: &[String]) -> String {
    let mut n = [0u64; 4];
    for r in refusals {
        let k = if r.contains("could not connect") || r.contains("could not send") {
            0
        } else if r.contains("before the deadline") {
            1
        } else if r.contains("declined the reservation")
            || r.contains("ERR busy")
            || r.contains("budget")
        {
            2
        } else {
            3
        };
        n[k] += 1;
    }
    [
        "trustd unreachable",
        "trustd did not answer in time",
        "trustd declined or was busy",
        "another refusal",
    ]
    .iter()
    .zip(n)
    .filter(|(_, c)| *c > 0)
    .map(|(kind, c)| format!("{kind}: {c}"))
    .collect::<Vec<_>>()
    .join(", ")
}

impl Measured {
    /// Read one check's log, line by line (a heavy library's log is large), and
    /// the exit status its `<log>.rc` recorded.
    #[must_use]
    pub fn of_log(log: impl BufRead, rc: &str) -> Measured {
        const ADMISSION: &str = "memory admission failed";
        const BUDGET: &str = "exceeded the per-function verification budget";
        let mut m = Measured {
            rc: rc.to_string(),
            ..Measured::default()
        };
        let (mut listed, mut named): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        let mut ceiling_bytes: Option<u64> = None;
        let mut peak_gib: Option<f64> = None;
        // What a check targo would not start said (the lane's
        // `launch_refusal_cause`, in its order).
        let (mut launch_refused, mut dirty, mut owned, mut unready) = (false, false, false, false);
        // Whether it finished ("A CHECK THAT DID NOT FINISH"): targo-trust's
        // transport rows by kind (the first message of each), cargo's first
        // compiler-ended-by-a-signal line, and the last Coverage line.
        let mut cut: BTreeMap<String, String> = BTreeMap::new();
        let mut signalled: Option<(String, u32, String)> = None;
        let mut coverage: Option<String> = None;
        // A PATH TARGO REFUSED, each once.
        let mut refused_paths: BTreeSet<PathRefusal> = BTreeSet::new();
        let mut buf = Vec::new();
        let mut log = log;
        loop {
            buf.clear();
            match log.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.contains("verification did not run") || line.contains("transport:missing-json")
            {
                m.transport = true;
            }
            launch_refused |= line.contains(TRUSTD_REFUSED);
            dirty |= line.contains(TRUSTD_DIRTY);
            owned |= line.contains("another trustd owns");
            unready |= line.contains("did not establish exact-identity readiness");
            let lead = line.trim_start();
            if lead.starts_with("Summary:") {
                m.summary = one_line(line);
            }
            if lead.starts_with("Coverage:") {
                coverage = Some(one_line(lead));
            }
            for (kind, msg) in transport_rows(line) {
                if kind != "missing-json" {
                    cut.entry(kind).or_insert(msg);
                }
            }
            if signalled.is_none() {
                signalled = compiler_signal(line);
            }
            refused_paths.extend(path_refusals(line));
            if let Some(a) = line.find(ADMISSION) {
                // `grep -o 'memory admission failed.*'`: from the first mention on.
                let said = line[a..].to_string();
                // `\[UNKNOWN\].*memory admission failed`: a mention after the
                // first `[UNKNOWN]`.
                let last = line.rfind(ADMISSION).unwrap_or(a);
                if line
                    .find("[UNKNOWN]")
                    .is_some_and(|u| u + "[UNKNOWN]".len() <= last)
                {
                    listed.push(said.clone());
                }
                named.push(said);
            }
            for i in find_all(line, "timed_out=") {
                let d = digits_at(line, i + "timed_out=".len());
                m.lapsed = m.lapsed.saturating_add(d.parse().unwrap_or(0));
            }
            if line.contains(CHECK_CANCELLED) {
                m.cancelled = m.cancelled.saturating_add(1);
            }
            if lead
                .strip_prefix("[UNKNOWN]")
                .is_some_and(|rest| rest.contains(BUDGET))
            {
                m.budget_rows += 1;
            }
            if let Some(s) = census_skipped(line) {
                m.skipped = Some(m.skipped.unwrap_or(0).saturating_add(s));
            }
            if let Some(c) = census_figure(line, " ceiling ") {
                m.ceiling = c;
            }
            if let Some(p) = census_figure(line, " peak ") {
                // The largest peak, compared as the lane compares figures.
                if peak_gib.is_none_or(|was| gib_of(&p).is_some_and(|now| now > was)) {
                    peak_gib = gib_of(&p);
                    m.peak = p;
                }
            }
            // The 321aaeda7 seal's flag; trust main has none (5aa81ac1683), so a
            // log of a seal from main names no ceiling here either.
            for i in find_all(line, "trust-verify-memory-ceiling-bytes") {
                let at = i + "trust-verify-memory-ceiling-bytes".len();
                if matches!(line.as_bytes().get(at), Some(b'=' | b' ')) {
                    let d = digits_at(line, at + 1);
                    if let Ok(b) = d.parse() {
                        ceiling_bytes = Some(b);
                    }
                }
            }
        }
        let refusals = if listed.is_empty() { named } else { listed };
        m.refused = refusals.len() as u64;
        m.refused_why = admission_cause(&refusals);
        if m.ceiling.is_empty()
            && let Some(b) = ceiling_bytes
        {
            m.ceiling = format!("{b} B");
        }
        if m.summary.is_empty() && launch_refused {
            m.launch = if dirty {
                "dirty"
            } else if owned {
                "owned"
            } else if unready {
                "unready"
            } else {
                "other"
            }
            .into();
        }
        // A CHECK THAT DID NOT FINISH: what its own record says, in its words.
        let mut ended: Vec<String> = cut
            .iter()
            .map(|(kind, msg)| {
                let msg = if msg.is_empty() {
                    "a row of that kind"
                } else {
                    msg.as_str()
                };
                format!("targo-trust: {msg} [transport:{kind}]")
            })
            .collect();
        if let Some((program, n, name)) = &signalled {
            ended.push(format!(
                "cargo: {program} ended by signal {n}{}",
                signal_said(name)
            ));
        }
        if let Some(n) = rc
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|c| (129..=192).contains(c))
            .map(|c| c - 128)
        {
            ended.push(format!(
                "the check itself ended by signal {n}{}",
                signal_said(signal_name(n))
            ));
        }
        if !m.summary.is_empty() {
            match &coverage {
                Some(c) if coverage_complete(c) => {}
                Some(c) => ended.push(format!("targo-trust's {c}")),
                None => ended.push(
                    "targo-trust printed no Coverage line, so nothing says the compiler covered the whole crate"
                        .into(),
                ),
            }
        }
        m.ended = one_line(&ended.join("; "));
        m.path_refused = refused_paths.into_iter().collect();
        m
    }

    fn to_fields(&self) -> String {
        [
            one_line(&self.rc),
            u8::from(self.transport).to_string(),
            self.refused.to_string(),
            self.lapsed.to_string(),
            self.cancelled.to_string(),
            self.budget_rows.to_string(),
            self.skipped.map_or_else(|| "-".into(), |n| n.to_string()),
            one_line(&self.ceiling),
            one_line(&self.peak),
            one_line(&self.launch),
            one_line(&self.held),
            one_line(&self.held_what),
            one_line(&self.refused_why),
            one_line(&self.wall_budget),
            one_line(&self.ended),
            one_line(&self.summary),
        ]
        .join("\t")
    }

    fn from_fields(f: &[&str]) -> Option<Measured> {
        let [
            rc,
            transport,
            refused,
            lapsed,
            cancelled,
            budget_rows,
            skipped,
            ceiling,
            peak,
            launch,
            held,
            held_what,
            refused_why,
            wall_budget,
            ended,
            summary,
        ] = f
        else {
            return None;
        };
        if !matches!(*launch, "" | "dirty" | "owned" | "unready" | "other") {
            return None;
        }
        if !(wall_budget.is_empty()
            || *wall_budget == "?"
            || wall_budget.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        Some(Measured {
            rc: (*rc).to_string(),
            transport: match *transport {
                "0" => false,
                "1" => true,
                _ => return None,
            },
            refused: refused.parse().ok()?,
            lapsed: lapsed.parse().ok()?,
            cancelled: cancelled.parse().ok()?,
            budget_rows: budget_rows.parse().ok()?,
            // `-` is a log with no census count ([`Measured::skipped`]).
            skipped: match *skipped {
                "-" => None,
                n => Some(n.parse().ok()?),
            },
            ceiling: (*ceiling).to_string(),
            peak: (*peak).to_string(),
            launch: (*launch).to_string(),
            held: (*held).to_string(),
            held_what: (*held_what).to_string(),
            refused_why: (*refused_why).to_string(),
            wall_budget: (*wall_budget).to_string(),
            ended: (*ended).to_string(),
            summary: (*summary).to_string(),
            // Never filed ([`filing`]), so never read back.
            path_refused: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// MEMORY: WHAT A HOST CAN VERIFY (the lane's header, "MEMORY")
// ---------------------------------------------------------------------------
//
// `targo trust` gives each verified library a memory ceiling of one eighth of
// the host's RAM and takes no other value, so a floor recorded under a larger
// ceiling can read as a regression on a smaller host (the owner's rule: a
// machine too small to verify something gets a warning and a refusal, never a
// failure). The memory record
// ([`MEMORY`]) holds, per library, the ceiling its floor
// was SET at and the verifier's measured NEED, one row per RAM it was measured
// on (MemRow, "THE NEED IS THE HOST'S"); this host's own observations
// (`<git common dir>/aterm-release/trust-memory-host.tsv`, never committed) add
// refusals learned here. Figures are compared in GiB: four decimals from a log,
// one decimal as printed.

const GIB: f64 = 1_073_741_824.0;

/// The memory record, relative to the repository root.
pub const MEMORY: &str = "crates/aterm-release/trust/memory.tsv";

/// The header of this host's memory observations, as the lane writes it.
const HOST_OBS_HEADER: &str = "# trust lane host memory observations: <name@version> <held|cap-bound> <run ceiling GiB> <verifier peak GiB> <proved> <floor> <prover> <when>, one row per library whose last check here ran out of the verifier's memory ceiling. While its version, floor and prover stand, a cap-bound row is refused up front on this host and a held one is gated; --try-refused runs a refused one again. Local to this machine, never committed.";

/// Four decimals, as the lane's `to_gib … 4` printed a figure it compared.
fn four(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or(x)
}

/// One decimal, as trustc's census and the memory record print a figure.
fn one(x: f64) -> String {
    format!("{x:.1}")
}

/// A figure a log prints (`3.5 GiB`, `983.0 MiB`, `3221225472 B`) in GiB, to
/// four decimals; `None` when it is not a number and a unit.
fn gib_of(figure: &str) -> Option<f64> {
    let (v, unit) = figure.split_once(' ')?;
    let v: f64 = v.parse().ok()?;
    let f = match unit.as_bytes().first() {
        Some(b'K') => 1.0 / 1_048_576.0,
        Some(b'M') => 1.0 / 1024.0,
        Some(b'G') => 1.0,
        Some(b'T') => 1024.0,
        _ => 1.0 / GIB,
    };
    Some(four(v * f))
}

/// A count of GiB as the memory record and the lane's lines write it (`16.0`,
/// `1.4`): `[0-9]+(\.[0-9]+)?`, `None` for `-` or anything else.
fn gib(s: &str) -> Option<f64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, "0"));
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    (digits(int) && digits(frac))
        .then(|| s.parse().ok())
        .flatten()
}

/// `a <= b` for two GiB counts; false when either is not one.
fn gib_le(a: &str, b: &str) -> bool {
    matches!((gib(a), gib(b)), (Some(a), Some(b)) if a <= b)
}

/// The larger (or smaller) of two GiB counts, one decimal.
fn gib_max(a: &str, b: &str) -> String {
    let (x, y) = (gib(a).unwrap_or(0.0), gib(b).unwrap_or(0.0));
    one(if x >= y { x } else { y })
}
fn gib_min(a: &str, b: &str) -> String {
    let (x, y) = (gib(a).unwrap_or(0.0), gib(b).unwrap_or(0.0));
    one(if x <= y { x } else { y })
}

/// The whole GiB of RAM whose eighth reaches a ceiling of `x` GiB.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ram_for(x: f64) -> u64 {
    let r = 8.0 * x;
    let i = r.trunc();
    (if i < r - 1e-9 { i + 1.0 } else { i }) as u64
}

/// The whole GiB of RAM whose eighth is MORE than `x` GiB — a need known only
/// as a lower bound.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ram_above(x: f64) -> u64 {
    (8.0 * x + 1e-9).trunc() as u64 + 1
}

/// The machine a run was on, as the lane read it: its memory in bytes (0 when
/// it could not be read) and whether that is Linux's MemTotal, a few percent
/// under the RAM the machine is sold with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Host {
    pub mem: u64,
    pub memtotal: bool,
}

impl Host {
    /// The RAM, whole when it is a whole number of GiB, else one decimal.
    #[allow(clippy::cast_precision_loss)]
    fn ram_gib(&self) -> String {
        let g = self.mem as f64 / GIB;
        if (g - g.trunc()).abs() < f64::EPSILON {
            format!("{g:.0}")
        } else {
            format!("{g:.1}")
        }
    }

    /// targo trust's rule: RAM / 8 in whole bytes, one decimal of GiB; `None`
    /// when the memory could not be read.
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn ceiling(&self) -> Option<String> {
        (self.mem > 0).then(|| one((self.mem / 8) as f64 / GIB))
    }

    /// `<RAM> GiB RAM, verifier ceiling <C> GiB`, or `memory unreadable`.
    fn desc(&self) -> String {
        match self.ceiling() {
            Some(c) => format!("{} GiB RAM, verifier ceiling {c} GiB", self.ram_gib()),
            None => "memory unreadable".into(),
        }
    }

    /// The host's ceiling, said the way targo takes it.
    fn share(&self) -> String {
        if self.mem > 0 {
            format!("{} GiB RAM / 8", self.ram_gib())
        } else {
            "targo trust: RAM / 8".into()
        }
    }

    /// The RAM as a memory row names the host its need was measured on (`24`,
    /// `125.7`); `None` when it could not be read.
    fn ram_class(&self) -> Option<String> {
        (self.mem > 0).then(|| self.ram_gib())
    }

    /// What verifies a library: `sure`, the RAM that surely does, and `may`,
    /// a RAM that may — said only when no RAM surely does, and only of a
    /// machine larger than this one.
    fn ram_phrase(&self, sure: Option<u64>, may: Option<u64>) -> String {
        let of = if self.memtotal {
            "of RAM as the OS reports it (MemTotal)"
        } else {
            "of RAM"
        };
        let may = may.filter(|m| self.mem == 0 || !gib_le(&m.to_string(), &self.ram_gib()));
        match (sure, may) {
            (Some(s), _) => format!("{s} GiB {of} verifies it"),
            (None, Some(m)) => format!(
                "its floor records no ceiling, so no RAM is known to verify it ({m} GiB may)"
            ),
            (None, None) => "its floor records no ceiling, so no RAM is known to verify it".into(),
        }
    }
}

/// One row of the memory record, as a run read it: the ceiling the library's
/// floor was set at; the RAM of the host its need was measured on and the
/// verifier ceiling of the run that measured it; the verifier's need, that
/// need's bound (`=` measured with room, `>` measured by a run that ran out, so
/// more) and when. `-` each when the record has no row; a row that records no
/// need has `-` for all four of its measurement's fields.
///
/// THE NEED IS THE HOST'S. The census's peak is a RESIDENT size,
/// and memory pressure lowers it: aterm-cli's 2026-09-29 tree peaked at 2.1 GiB
/// with room on the 8 GiB A18 Pro Mac and at 3.0 GiB on m101 (24 GiB), where
/// aterm-verify also peaked at 3.2 GiB against the 8 GiB Mac's 1.6, and
/// aterm-census at 6.4 against 1.3. A need is therefore recorded with the RAM
/// it was measured on, a library may have one row per RAM, and a host reads the
/// row that bounds its own need ([`row_for`], [`Reading`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemRow {
    pub f: String,
    /// The RAM, GiB, of the host the need was measured on — the machine's own,
    /// never a faked figure: the memory pressure its resident peak reflects.
    pub ram: String,
    /// The verifier ceiling, GiB, of the run that measured the need: RAM / 8 as
    /// targo read it (a faked RAM included, said in the row's `measured`).
    pub at: String,
    pub need: String,
    pub bound: String,
    pub when: String,
}

impl Default for MemRow {
    fn default() -> MemRow {
        MemRow {
            f: "-".into(),
            ram: "-".into(),
            at: "-".into(),
            need: "-".into(),
            bound: "-".into(),
            when: "-".into(),
        }
    }
}

impl MemRow {
    fn fields(&self) -> String {
        [
            &self.f,
            &self.ram,
            &self.at,
            &self.need,
            &self.bound,
            &self.when,
        ]
        .map(|v| one_line(v))
        .join("\t")
    }

    fn from_fields(f: &[&str]) -> Option<MemRow> {
        let [f, ram, at, need, bound, when] = f else {
            return None;
        };
        Some(MemRow {
            f: (*f).into(),
            ram: (*ram).into(),
            at: (*at).into(),
            need: (*need).into(),
            bound: (*bound).into(),
            when: (*when).into(),
        })
    }
}

/// Two RAM figures as the memory record writes them (`24`, `125.7`): the same
/// machine size, to the record's one decimal.
fn same_ram(a: &str, b: &str) -> bool {
    matches!((gib(a), gib(b)), (Some(a), Some(b)) if (a - b).abs() < 0.05)
}

/// How a host reads a need measured on some host's RAM (MemRow, "THE NEED IS
/// THE HOST'S"). Memory pressure only lowers the resident peak the census
/// reads, so a need measured where MORE RAM was free is at least this host's,
/// and one measured where LESS was free is at most it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// Measured on this host's RAM: the need itself.
    Here,
    /// Measured on more RAM: an upper bound on this host's need.
    Upper,
    /// Measured on less RAM: a lower bound on this host's need.
    Lower,
    /// No need, or this host's RAM is unknown.
    Unknown,
}

impl Reading {
    #[allow(clippy::cast_precision_loss)]
    fn of(row: &MemRow, host: &Host) -> Reading {
        let here = host.mem as f64 / GIB;
        match gib(&row.ram) {
            Some(_) if host.mem > 0 && same_ram(&row.ram, &host.ram_gib()) => Reading::Here,
            Some(r) if host.mem > 0 && r > here => Reading::Upper,
            Some(_) if host.mem > 0 => Reading::Lower,
            _ => Reading::Unknown,
        }
    }
}

/// THE ROW A HOST READS of a library's rows: the one measured on its own RAM;
/// else the one measured on the most RAM below it, a lower bound — what it
/// refuses surely does not fit, and it never says the library fits; else the
/// one measured on the least RAM above it, an upper bound — read as every host
/// read the record before rows had a RAM; else the row with no need. On a
/// host whose RAM is unknown (which refuses nothing up front), the row measured
/// on the most RAM. `None` for a library the record has no row for.
///
/// So a host whose RAM has no row of its own decides as a single-row record
/// does: below the row's RAM by the same rules, above it gating what it gated
/// and refusing what it refused — but never calling a library FITS from a need
/// measured under more pressure than its own ([`fits`]).
fn row_for<'a>(rows: &'a [MemRow], host: &Host) -> Option<&'a MemRow> {
    let ram = |r: &&MemRow| gib(&r.ram).unwrap_or(0.0);
    let by_ram = |a: &&MemRow, b: &&MemRow| ram(a).total_cmp(&ram(b));
    let read = |w: Reading| {
        rows.iter()
            .filter(move |r| r.ram != "-" && Reading::of(r, host) == w)
    };
    read(Reading::Here)
        .next()
        .or_else(|| read(Reading::Lower).max_by(by_ram))
        .or_else(|| read(Reading::Upper).min_by(by_ram))
        .or_else(|| read(Reading::Unknown).max_by(by_ram))
        .or_else(|| rows.first())
}

/// Does the record say the library FITS a verifier ceiling of `c` GiB on this
/// host — rule 2 of [`memory_refusal`], and after a run what makes a drop that
/// ran out the code's rather than the host's? Only a need measured with room
/// (`=`), read as [`Reading`] says:
/// * measured on this host's RAM: when printed BELOW `c`, or measured with room
///   at a ceiling no larger than `c`. trustc's census prints both figures to one
///   decimal, so a need printed AT the ceiling may be just over it, and on seal
///   321aaeda7 a verifier whose peak reaches its ceiling turns every later
///   function UNKNOWN: m101 (24 GiB), 2026-10-01, aterm-cli's need measured with
///   room at a 4.0 GiB ceiling printed 3.0, and at its real 3.0 GiB ceiling it
///   proved 0 of 443 against a floor of 75 (92 of 558 with room);
/// * measured on more RAM (or this host's is unknown): no larger than `c`, a
///   tie included, as every host read the record before rows had a RAM — a
///   need from a host with more free memory is at least this host's, and
///   aterm-regex's 1.0, measured on 24 GiB, is gated at an 8 GiB host's 1.0 GiB
///   ceiling as it always was;
/// * measured on less RAM: never. A lower bound cannot say a library fits — the
///   8 GiB Mac's 1.3 GiB for aterm-census was 6.4 on 24 GiB.
fn fits(row: &MemRow, host: &Host, c: &str) -> bool {
    row.bound == "="
        && match Reading::of(row, host) {
            Reading::Here => {
                matches!((gib(&row.need), gib(c)), (Some(n), Some(x)) if n < x)
                    || gib_le(&row.at, c)
            }
            Reading::Upper | Reading::Unknown => gib_le(&row.need, c),
            Reading::Lower => false,
        }
}

/// The fields of a tab-separated line as bash's `IFS=$'\t' read` gives them:
/// runs of tabs are one separator (a tab is IFS whitespace), and the last of
/// `n` names takes the rest of the line.
fn tab_fields(line: &str, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = line.trim_matches('\t');
    while out.len() + 1 < n && !rest.is_empty() {
        let (f, r) = rest.split_once('\t').unwrap_or((rest, ""));
        out.push(f.to_string());
        rest = r.trim_start_matches('\t');
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// THE MEMORY RECORD, read and held to its shape: `<name> <floor ceiling>
/// <host RAM> <run ceiling> <need> <bound> <measured>`. A figure that is not
/// GiB or `-`, a bound that is not `=`, `>` or `-`, a need without its bound or
/// its host RAM (or either without a need), a run ceiling with no need, two
/// rows of a library measured on one RAM, a row with no need beside another,
/// or two floor ceilings for one library is an error naming the row — a typo
/// must not silently refuse or admit a library. Each library's rows, in the
/// record's order.
///
/// # Errors
/// The row that does not parse, in the lane's words.
pub fn parse_memory(
    text: &str,
    name: &str,
) -> std::result::Result<BTreeMap<String, Vec<MemRow>>, String> {
    let gib_or_dash = |s: &str| s == "-" || gib(s).is_some();
    let mut out: BTreeMap<String, Vec<MemRow>> = BTreeMap::new();
    for line in text.lines() {
        let f = tab_fields(line, 7);
        let Some(lib) = f.first().filter(|n| !n.starts_with('#')) else {
            continue;
        };
        let col = |i: usize| f.get(i).map_or("", String::as_str);
        let (fc, ram, at, need, bound) = (col(1), col(2), col(3), col(4), col(5));
        let bad = if !gib_or_dash(fc) {
            format!("the floor ceiling '{fc}'; it must be GiB (e.g. 16.0) or '-'")
        } else if !gib_or_dash(ram) {
            format!("the host RAM '{ram}'; it must be GiB (e.g. 24) or '-'")
        } else if !gib_or_dash(at) {
            format!("the run ceiling '{at}'; it must be GiB (e.g. 3.0) or '-'")
        } else if !gib_or_dash(need) {
            format!("the need '{need}'; it must be GiB (e.g. 1.4) or '-'")
        } else if !matches!(bound, "=" | ">" | "-") {
            format!(
                "the bound '{bound}'; it must be '=' (measured with room), '>' (it ran out, so more) or '-'"
            )
        } else if (need == "-") != (bound == "-") {
            format!(
                "the need '{need}' with the bound '{bound}'; a need takes a bound, and a bound a need"
            )
        } else if (need == "-") != (ram == "-") {
            format!(
                "the need '{need}' with the host RAM '{ram}'; a need names the RAM it was measured on, and a host RAM a need"
            )
        } else if need == "-" && at != "-" {
            format!("the run ceiling '{at}' with no need")
        } else {
            String::new()
        };
        if !bad.is_empty() {
            return Err(format!("{name} gives '{lib}' {bad}"));
        }
        let when = col(6);
        let row = MemRow {
            f: fc.into(),
            ram: ram.into(),
            at: at.into(),
            need: need.into(),
            bound: bound.into(),
            when: if when.is_empty() {
                "-".into()
            } else {
                when.into()
            },
        };
        let rows = out.entry(lib.clone()).or_default();
        let clash = if let Some(first) = rows.first().filter(|r| r.f != row.f) {
            format!(
                "two floor ceilings, '{}' and '{fc}'; every row of a library names the ceiling its one floor was set at",
                first.f
            )
        } else if !rows.is_empty() && (ram == "-" || rows.iter().any(|r| r.ram == "-")) {
            "a row with no need beside another; a library records no need, or one per RAM it was measured on".to_string()
        } else if rows.iter().any(|r| same_ram(&r.ram, ram)) {
            format!("two rows measured on {ram} GiB of RAM; a library has one row per RAM")
        } else {
            String::new()
        };
        if !clash.is_empty() {
            return Err(format!("{name} gives '{lib}' {clash}"));
        }
        rows.push(row);
    }
    Ok(out)
}

/// One row of what this host learned (the lane's host observations): a
/// library's last check here that ran out of the verifier's ceiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostObs {
    /// `name@version`.
    pub spec: String,
    /// `held` (it met its floor anyway) or `cap-bound` (it fell below it).
    pub outcome: String,
    /// The run's ceiling, GiB, as its log names it; `-` for a `held` run whose log
    /// names none, a row [`parse_host_obs`] drops.
    pub ceil: String,
    /// The verifier's peak, GiB, `-` when the log had none.
    pub peak: String,
    pub proved: String,
    pub floor: String,
    /// The `--version` line of the prover that ran it.
    pub prover: String,
    pub when: String,
}

impl HostObs {
    /// What a refusal reads of it.
    fn seen(&self) -> Seen {
        Seen {
            outcome: self.outcome.clone(),
            ceil: self.ceil.clone(),
            peak: self.peak.clone(),
            when: self.when.clone(),
        }
    }
}

/// What a memory refusal read of this host's observation of the library, kept
/// in the facts so the refusal is said from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub outcome: String,
    pub ceil: String,
    pub peak: String,
    pub when: String,
}

impl Seen {
    fn fields(&self) -> String {
        [&self.outcome, &self.ceil, &self.peak, &self.when]
            .map(|v| one_line(v))
            .join("\t")
    }

    fn from_fields(f: &[&str]) -> Option<Option<Seen>> {
        let [outcome, ceil, peak, when] = f else {
            return None;
        };
        Some((*outcome != "-").then(|| Seen {
            outcome: (*outcome).into(),
            ceil: (*ceil).into(),
            peak: (*peak).into(),
            when: (*when).into(),
        }))
    }
}

/// The rows a run may read: an outcome of `held` or `cap-bound`, a ceiling that
/// is GiB and a floor that is a count; any other row is ignored (it can only
/// add a refusal, never a pass).
#[must_use]
pub fn parse_host_obs(text: &str) -> Vec<HostObs> {
    text.lines()
        .filter_map(|line| {
            let f = tab_fields(line, 8);
            let col = |i: usize| f.get(i).cloned().unwrap_or_default();
            let spec = col(0);
            (!spec.is_empty() && !spec.starts_with('#')).then_some(())?;
            let o = HostObs {
                spec,
                outcome: col(1),
                ceil: col(2),
                peak: col(3),
                proved: col(4),
                floor: col(5),
                prover: col(6),
                when: col(7),
            };
            (matches!(o.outcome.as_str(), "held" | "cap-bound")
                && gib(&o.ceil).is_some()
                && !o.floor.is_empty()
                && o.floor.bytes().all(|b| b.is_ascii_digit()))
            .then_some(o)
        })
        .collect()
}

/// Why this host refuses a library up front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The MEMORY line, facts first.
    pub line: String,
    /// Its need, `~N` (measured with room) or `>N` (a lower bound).
    pub token: String,
    /// The RAM that surely verifies it, when its floor records a ceiling or its
    /// need was measured with room.
    pub sure: Option<u64>,
}

/// REFUSE UP FRONT (the lane's `memory_refusal`): does this host refuse the
/// library whose floor exists (`has_floor`), whose memory row — the one this
/// host reads ([`row_for`]) — is `row` and whose observation here — under this
/// floor and prover — is `obs`? With C this host's ceiling and F the floor's,
/// the first rule that matches decides:
///   0. C unknown, or no floor (NO-FLOOR is the ratchet's finding): gate it;
///   1. F <= C, or a `held` observation here at a ceiling <= C: gate it;
///   2. the row says it FITS C ([`fits`]): gate it;
///   3. a `cap-bound` observation here at a ceiling >= C: refuse;
///   4. a need `=` measured on this host's RAM (which, not fitting, reaches C),
///      a need `=` above C, or a lower bound (`>`) at or above C: refuse;
///   5. anything else: gate it, and the rule after the run decides.
///
/// A refusal names the RAM that verifies it, the least of: the floor's; the RAM
/// whose ceiling its need was measured with room at; and a RAM whose ceiling is
/// above the need, when that is no more than the RAM the need was measured on.
/// Above that RAM, a ceiling above the need only MAY verify it: with less
/// memory pressure the need may be larger (aterm-cli: 3.0 with room on 24 GiB,
/// so 32 GiB verifies it and 25 may).
pub(crate) fn memory_refusal(
    host: &Host,
    name: &str,
    has_floor: bool,
    row: &MemRow,
    obs: Option<&Seen>,
) -> Option<Refusal> {
    let c = host.ceiling()?;
    if !has_floor || (row.f != "-" && gib_le(&row.f, &c)) {
        return None;
    }
    if obs.is_some_and(|o| o.outcome == "held" && gib_le(&o.ceil, &c)) {
        return None;
    }
    if fits(row, host, &c) {
        return None;
    }
    let sure = gib(&row.f).map(ram_for);
    if let Some(o) = obs.filter(|o| o.outcome == "cap-bound" && gib_le(&c, &o.ceil)) {
        // Where it ran out bounds its need from below: more than the larger of
        // that run's peak and its ceiling.
        let peak = if o.peak == "-" { "0" } else { o.peak.as_str() };
        let low = gib_max(peak, &o.ceil);
        let may =
            (row.f == "-" || !gib_le(&row.f, &low)).then(|| ram_above(gib(&low).unwrap_or(0.0)));
        return Some(Refusal {
            line: format!(
                "{name} not verified here: needs more than {low} GiB, this host gives {c} GiB ({}); it ran out here on {}; {}",
                host.share(),
                o.when.get(..10).unwrap_or(&o.when),
                host.ram_phrase(sure, may)
            ),
            token: format!(">{low}"),
            sure,
        });
    }
    let reading = Reading::of(row, host);
    if row.bound == "=" && (reading == Reading::Here || !gib_le(&row.need, &c)) {
        // What verifies it: the floor's RAM; the RAM whose ceiling the need was
        // measured with room at; or a RAM whose ceiling is above the need and
        // which is no more than the RAM it was measured on (more memory
        // pressure, so no larger a need) — the least of them. A RAM above the
        // need but more than the one that measured it only may: there the need
        // may be larger.
        let room = gib(&row.at).map(ram_for);
        let above = gib(&row.need).map(ram_above);
        let under = above.filter(|r| gib_le(&r.to_string(), &row.ram));
        let sure = [sure, room, under].into_iter().flatten().min();
        let may = above;
        // Where it was measured is said only of another host.
        let measured = if reading == Reading::Here {
            String::new()
        } else {
            format!(", measured on {} GiB of RAM", row.ram)
        };
        // A need equal to the ceiling is refused too: a verifier whose peak reaches
        // its ceiling turns every later function UNKNOWN.
        let tight = if gib_le(&row.need, &c) {
            ", no room above the need"
        } else {
            ""
        };
        let short = format!("this host gives {c} GiB ({}){tight}", host.share());
        return Some(Refusal {
            line: format!(
                "{name} not verified here: needs ~{} GiB{measured}, {short}; {}",
                row.need,
                host.ram_phrase(sure, may)
            ),
            token: format!("~{}", row.need),
            sure,
        });
    }
    if row.bound == ">" && gib_le(&c, &row.need) {
        let may = (row.f == "-" || !gib_le(&row.f, &row.need))
            .then(|| ram_above(gib(&row.need).unwrap_or(0.0)));
        return Some(Refusal {
            line: format!(
                "{name} not verified here: needs more than {} GiB, this host gives {c} GiB ({}); {}",
                row.need,
                host.share(),
                host.ram_phrase(sure, may)
            ),
            token: format!(">{}", row.need),
            sure,
        });
    }
    None
}

/// THE MEMORY CENSUS of one check (the lane's `census_of`): its peak and
/// ceiling (GiB, four decimals, and as printed), bodies skipped at the ceiling
/// and budget rows, and whether it surely ran out (EXHAUSTED: bodies skipped,
/// or budget rows with the peak ABOVE the ceiling, compared unrounded) or sat
/// at the ceiling (ATCEIL: budget rows with the two printed equal, which never
/// excuses a drop and never lets a need be called measured with room). Budget
/// rows alone prove nothing about memory: that budget also trips on time and
/// steps.
#[derive(Clone, Debug, Default)]
struct Census {
    peak: Option<String>,
    ceil: Option<String>,
    skipped: Option<u64>,
    budget: u64,
    exhausted: bool,
    atceil: bool,
}

impl Census {
    fn of(m: &Measured) -> Census {
        let (peak, ceil) = (gib_of(&m.peak), gib_of(&m.ceiling));
        let mut c = Census {
            peak: peak.map(one),
            ceil: ceil.map(one),
            skipped: m.skipped,
            budget: m.budget_rows,
            ..Census::default()
        };
        if m.skipped.is_some_and(|n| n > 0) {
            c.exhausted = true;
        } else if let (true, Some(p), Some(x)) = (m.budget_rows > 0, peak, ceil)
            && x <= p
        {
            if p <= x {
                c.atceil = true;
            } else {
                c.exhausted = true;
            }
        }
        c
    }
}

/// KEEP THE MEMORY RECORD CURRENT (the lane's `mem_learn`, under
/// `--update-ratchet`): what a run on `host` learned, written to the row of its
/// own RAM — `row`, the one it read, when that is measured there, else a new
/// one; a row measured on another RAM is read, never rewritten. A run with room
/// writes its census peak, `=`; a run that ran out, or whose peak sat at its
/// ceiling with budget rows, writes `>` the larger of its peak and its ceiling —
/// but only over a row that knows less. Either names the run's ceiling. The
/// floor ceiling `f` is the caller's, and every row of the library takes it
/// ([`memory_rewritten`]); a host whose RAM is unknown files no need. `None`
/// when nothing would change.
fn mem_learn(row: &MemRow, f: &str, c: &Census, today: &str, host: &Host) -> Option<MemRow> {
    let ram = host.ram_class().filter(|_| c.peak.is_some());
    let base = match &ram {
        Some(r) if Reading::of(row, host) != Reading::Here => MemRow {
            ram: r.clone(),
            ..MemRow::default()
        },
        _ => row.clone(),
    };
    let mut new = MemRow {
        f: f.to_string(),
        ..base.clone()
    };
    if let (Some(peak), Some(_)) = (&c.peak, &ram) {
        let at = c.ceil.clone().unwrap_or_else(|| "-".into());
        if !c.exhausted && !c.atceil {
            (new.at, new.need, new.bound, new.when) =
                (at, peak.clone(), "=".into(), today.to_string());
        } else {
            let cand = c
                .ceil
                .as_deref()
                .map_or_else(|| peak.clone(), |x| gib_max(peak, x));
            if base.need == "-" || !gib_le(&cand, &base.need) {
                (new.at, new.need, new.bound, new.when) = (at, cand, ">".into(), today.to_string());
            }
        }
    }
    let changed = new.f != row.f
        || !same_slot(&new, row)
        || new.at != row.at
        || new.need != row.need
        || new.bound != row.bound;
    changed.then_some(new)
}

/// Do two rows of one library hold the same place in the record: both with no
/// need, or both measured on the same RAM?
fn same_slot(a: &MemRow, b: &MemRow) -> bool {
    (a.ram == "-" && b.ram == "-") || same_ram(&a.ram, &b.ram)
}

/// The memory record's text with `rows` written in: its comment and blank
/// lines first, then every row, sorted by library and RAM. A row in `rows`
/// replaces its library's row in the same place ([`same_slot`]) — and, when it
/// records a need, the library's row with none — and every row of its library
/// takes its floor ceiling.
#[must_use]
pub fn memory_rewritten(text: &str, rows: &[(String, MemRow)]) -> String {
    let mut out: Vec<String> = text
        .lines()
        .filter(|l| l.is_empty() || l.starts_with('#'))
        .map(str::to_string)
        .collect();
    let line_of = |n: &str, r: &MemRow| format!("{}\t{}", one_line(n), r.fields());
    // Each data line, with its row when it parses as one.
    let mut data: Vec<(String, Option<MemRow>, String)> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let f = tab_fields(l, 7);
            let name = f.first().cloned().unwrap_or_default();
            let fields: Vec<&str> = f.iter().skip(1).map(String::as_str).collect();
            (name, MemRow::from_fields(&fields), l.to_string())
        })
        .collect();
    for (n, r) in rows {
        data.retain(|(dn, dr, _)| {
            dn != n
                || !dr
                    .as_ref()
                    .is_some_and(|d| same_slot(d, r) || (r.ram != "-" && d.ram == "-"))
        });
        data.push((n.clone(), Some(r.clone()), line_of(n, r)));
        for (dn, dr, line) in &mut data {
            if let Some(d) = dr.as_mut().filter(|d| *dn == *n && d.f != r.f) {
                d.f.clone_from(&r.f);
                *line = line_of(dn, d);
            }
        }
    }
    let key = |(n, r, l): &(String, Option<MemRow>, String)| {
        let ram = r.as_ref().and_then(|r| gib(&r.ram)).unwrap_or(-1.0);
        (n.clone(), ram, l.clone())
    };
    data.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0)
            .then(ka.1.total_cmp(&kb.1))
            .then(ka.2.cmp(&kb.2))
    });
    out.extend(data.into_iter().map(|(_, _, l)| l));
    let mut s = out.join("\n");
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------
// THE FACTS OF ONE RUN
// ---------------------------------------------------------------------------

/// What the lane decided about one selected library before anything ran —
/// from the graph, the scope list and the fork roster, all in the tree — and,
/// for one it gated, what the check measured.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "`Gate` IS what the run measured (past the lint's 200-byte line since \
              `Measured::path_refused`, 2026-10-17); a run holds one Decision per selected \
              library, tens of them, so boxing it would put an allocation and a deref on \
              every match over a run's facts, in the lane, the judge and the comparison, \
              to save a few kilobytes"
)]
pub enum Decision {
    Gate(Measured),
    /// A bin-only member: nothing the library lane verifies.
    NoLib,
    /// A fork the roster names with a status other than `admitted`.
    Queued(String),
    /// Another selected package shares its name (see [`Facts::ambiguous`]).
    Ambiguous,
    /// This host could not reproduce its floor, so it was never run (MEMORY):
    /// what the refusal read of this host's own observation of it, when one
    /// decided.
    Memory(Option<Seen>),
}

impl Decision {
    /// How a plan's `crate` line says it.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Decision::Gate(_) => "gate".into(),
            Decision::NoLib => "no-lib".into(),
            Decision::Queued(s) => format!("queued:{s}"),
            Decision::Ambiguous => "ambiguous".into(),
            Decision::Memory(_) => "memory".into(),
        }
    }

    fn from_word(word: &str) -> Option<Decision> {
        Some(match word {
            "gate" => Decision::Gate(Measured::default()),
            "no-lib" => Decision::NoLib,
            "ambiguous" => Decision::Ambiguous,
            "memory" => Decision::Memory(None),
            other => Decision::Queued(other.strip_prefix("queued:")?.to_string()),
        })
    }
}

/// One selected library, in the lane's selection order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub decision: Decision,
    /// Its row of the memory record as the run read it — the one this host
    /// reads ([`row_for`]), `-` each when the record has none: what decided a
    /// memory refusal, and what the judge holds a drop that ran out of memory
    /// to.
    pub memory: MemRow,
}

impl Crate {
    /// A library with no memory row.
    #[must_use]
    pub fn new(name: &str, decision: Decision) -> Crate {
        Crate {
            name: name.into(),
            decision,
            memory: MemRow::default(),
        }
    }
}

/// Where a FILED record came from: its key and each input the key covers,
/// and what the run was on. Absent for a run that filed nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    pub key: String,
    pub tree_sans_floors: String,
    pub prover: String,
    pub lane: String,
    /// Informational: where the prover was, what its trustc says it is, the
    /// commit and tree the run was on, the host and its load, the slot count,
    /// when. None of it is in the key.
    pub prover_root: String,
    pub trustc: String,
    pub head: String,
    pub tree: String,
    pub host: String,
    pub load: String,
    pub jobs: String,
    pub date: String,
    /// Each later run that measured some of its libraries AGAIN on the same key
    /// ([`remeasured`]): `<date> <commit> <library>…`, oldest first.
    pub remeasured: Vec<String>,
}

/// Everything one run of the lane measured, and nothing it judged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The pinned driver's physical path.
    pub driver: String,
    /// Its `--version` line, empty when it gave none.
    pub driver_id: String,
    /// The socket the run's trustd keeper held a grant on.
    pub trustd: String,
    /// How many grants the keeper took, empty when it took none.
    pub grants: String,
    /// What the keeper found holding the socket instead of this build's trustd
    /// (the lane's `.trustd-seen`), empty when it found nothing else.
    pub trustd_seen: String,
    /// The libraries a narrowed run named (`-p`), empty for the whole tree.
    pub only: String,
    /// Roster forks the resolved graph does not carry.
    pub vanished: Vec<String>,
    /// Two selected packages sharing a name: the name and both specs.
    pub ambiguous: Vec<(String, String, String)>,
    pub crates: Vec<Crate>,
    /// The machine the run was on (MEMORY).
    pub host: Host,
    /// When the run measured, `YYYY-MM-DDTHH:MM:SSZ` — what the memory record
    /// and this host's observations say a figure was measured on.
    pub measured_at: String,
    pub provenance: Option<Provenance>,
}

impl Facts {
    /// The prover as a ratchet row names it: the driver's `--version` line,
    /// `unknown` when it gave none, on one line.
    #[must_use]
    pub fn driver_row(&self) -> String {
        let id = if self.driver_id.is_empty() {
            "unknown"
        } else {
            self.driver_id.as_str()
        };
        one_line(id)
    }

    /// The record as filed: magic, provenance, the run, every library, `end`.
    #[must_use]
    pub fn to_record(&self) -> String {
        let mut out = format!("{FACTS_MAGIC}\n");
        if let Some(p) = &self.provenance {
            for (k, v) in [
                ("key", &p.key),
                ("tree-sans-floors", &p.tree_sans_floors),
                ("prover", &p.prover),
                ("lane", &p.lane),
                ("prover-root", &p.prover_root),
                ("trustc", &p.trustc),
                ("head", &p.head),
                ("tree", &p.tree),
                ("host", &p.host),
                ("load", &p.load),
                ("jobs", &p.jobs),
                ("date", &p.date),
            ] {
                out.push_str(&format!("{k} {}\n", one_line(v)));
            }
            for r in &p.remeasured {
                out.push_str(&format!("remeasured {}\n", one_line(r)));
            }
        }
        for (k, v) in [
            ("driver", &self.driver),
            ("driver-id", &self.driver_id),
            ("trustd", &self.trustd),
            ("grants", &self.grants),
            ("trustd-seen", &self.trustd_seen),
            ("only", &self.only),
        ] {
            out.push_str(&format!("{k} {}\n", one_line(v)));
        }
        out.push_str(&format!("host-mem {}\n", self.host.mem));
        out.push_str(&format!(
            "host-mem-os {}\n",
            if self.host.memtotal { "MemTotal" } else { "-" }
        ));
        out.push_str(&format!("measured-at {}\n", one_line(&self.measured_at)));
        for v in &self.vanished {
            out.push_str(&format!("vanished {}\n", one_line(v)));
        }
        for (n, a, b) in &self.ambiguous {
            out.push_str(&format!(
                "ambiguous {}\t{}\t{}\n",
                one_line(n),
                one_line(a),
                one_line(b)
            ));
        }
        for c in &self.crates {
            out.push_str(&format!(
                "crate {}\t{}\t{}",
                one_line(&c.name),
                one_line(&c.decision.word()),
                c.memory.fields()
            ));
            match &c.decision {
                Decision::Gate(m) => {
                    out.push('\t');
                    out.push_str(&m.to_fields());
                }
                Decision::Memory(seen) => {
                    out.push('\t');
                    out.push_str(
                        &seen
                            .as_ref()
                            .map_or_else(|| ["-"; 4].join("\t"), Seen::fields),
                    );
                }
                _ => {}
            }
            out.push('\n');
        }
        out.push_str(FACTS_END);
        out.push('\n');
        out
    }

    /// Parse a record written by [`Facts::to_record`]. A file that is not one
    /// whole record — no magic line, a line this reader does not know, a crate
    /// line it cannot read, no `end` — is `None`, never a partial record.
    #[must_use]
    pub fn parse(text: &str) -> Option<Facts> {
        let mut lines = text.lines();
        if lines.next() != Some(FACTS_MAGIC) {
            return None;
        }
        let mut f = Facts::default();
        let mut p = Provenance::default();
        let mut has_provenance = false;
        let mut ended = false;
        for line in lines {
            if ended {
                return None;
            }
            if line == FACTS_END {
                ended = true;
                continue;
            }
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            let prov = |slot: &mut String| {
                *slot = v.to_string();
                true
            };
            has_provenance |= match k {
                "key" => prov(&mut p.key),
                "tree-sans-floors" => prov(&mut p.tree_sans_floors),
                "prover" => prov(&mut p.prover),
                "lane" => prov(&mut p.lane),
                "prover-root" => prov(&mut p.prover_root),
                "trustc" => prov(&mut p.trustc),
                "head" => prov(&mut p.head),
                "tree" => prov(&mut p.tree),
                "host" => prov(&mut p.host),
                "load" => prov(&mut p.load),
                "jobs" => prov(&mut p.jobs),
                "date" => prov(&mut p.date),
                "remeasured" => {
                    p.remeasured.push(v.to_string());
                    true
                }
                _ => false,
            };
            match k {
                "key" | "tree-sans-floors" | "prover" | "lane" | "prover-root" | "trustc"
                | "head" | "tree" | "host" | "load" | "jobs" | "date" | "remeasured" => {}
                "driver" => f.driver = v.to_string(),
                "driver-id" => f.driver_id = v.to_string(),
                "trustd" => f.trustd = v.to_string(),
                "grants" => f.grants = v.to_string(),
                "trustd-seen" => f.trustd_seen = v.to_string(),
                "only" => f.only = v.to_string(),
                "host-mem" => f.host.mem = v.parse().ok()?,
                "host-mem-os" => {
                    f.host.memtotal = match v {
                        "MemTotal" => true,
                        "-" => false,
                        _ => return None,
                    };
                }
                "measured-at" => f.measured_at = v.to_string(),
                "vanished" => f.vanished.push(v.to_string()),
                "ambiguous" => {
                    let [n, a, b] = v.split('\t').collect::<Vec<_>>()[..] else {
                        return None;
                    };
                    f.ambiguous.push((n.into(), a.into(), b.into()));
                }
                "crate" => {
                    let fields: Vec<&str> = v.split('\t').collect();
                    let (name, word) = (fields.first()?, fields.get(1)?);
                    let memory = MemRow::from_fields(fields.get(2..8)?)?;
                    let rest = &fields[8..];
                    let decision = match Decision::from_word(word)? {
                        Decision::Gate(_) => Decision::Gate(Measured::from_fields(rest)?),
                        Decision::Memory(_) => Decision::Memory(Seen::from_fields(rest)?),
                        d if rest.is_empty() => d,
                        _ => return None,
                    };
                    f.crates.push(Crate {
                        name: (*name).to_string(),
                        decision,
                        memory,
                    });
                }
                _ => return None,
            }
        }
        if !ended {
            return None;
        }
        f.provenance = has_provenance.then_some(p);
        Some(f)
    }
}

// ---------------------------------------------------------------------------
// THE JUDGE: facts + a ratchet -> a verdict
// ---------------------------------------------------------------------------

/// One ratchet row, as the lane reads it: `<crate>\t<proved>\t<total>\t<prover>[\t<why>]`.
struct Floor {
    proved: String,
    total: String,
    driver: String,
    why: String,
}

/// The first row naming `name` (`awk -F'\t' '$1==c{…; exit}'`); `None` when
/// there is no ratchet or no row.
fn floor_of(ratchet: Option<&str>, name: &str) -> Option<Floor> {
    let row = ratchet?
        .lines()
        .find(|l| l.split('\t').next() == Some(name))?;
    let f: Vec<&str> = row.split('\t').collect();
    let col = |i: usize| {
        f.get(i)
            .map_or("", |s| s.trim_end_matches('\t'))
            .to_string()
    };
    Some(Floor {
        proved: col(1),
        total: col(2),
        driver: col(3),
        why: col(4),
    })
}

/// The report's line for a path targo refused ([`PathRefusal`]), naming each
/// library whose check it refused, what is wrong with it and what the caller `me`
/// does about it.
fn refused_path_note(r: &PathRefusal, names: &[&str], me: u32) -> String {
    format!(
        "REFUSED PATH: {}: targo refused {} ({}), which says nothing about the code: {}",
        names.join(" "),
        r.path,
        r.said(me),
        r.remedy(me)
    )
}

/// What [`judge`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// The rows, totals, notes and the verdict sentence, as the lane prints
    /// them (a blank line first).
    pub report: String,
    /// The last line the lane prints after its FACTS line: `VERDICT: FAIL — …`
    /// (the failed rows),
    /// `VERDICT: COULD NOT RUN — …`, `VERDICT: NARROWED BY MEMORY — …`, or
    /// empty for a pass.
    pub tail: String,
    /// `PASS`, `FAIL`, `COULD-NOT-RUN` or `NARROWED`.
    pub word: &'static str,
    /// The lane's exit status: 0, 1, 2 or 4 (NARROWED BY MEMORY).
    pub exit: i32,
    /// `<proved>/<total>` over the libraries that were scored.
    pub total: String,
    /// What the run did not verify (the `GATED:` line's list), `none` when nothing.
    pub not_verified: String,
    /// Each row that failed the run, `<crate> <STATUS> <proved>/<total>`.
    pub failed: Vec<String>,
    /// The libraries nothing was decided about — trustd kept their check from
    /// starting or refused work their shortfall is within, or lapsed wall-clock
    /// budgets could cover their shortfall: what `--remeasure` runs again.
    pub unverified: Vec<String>,
    /// NARROWED BY MEMORY: each library this host could not verify — refused up
    /// front or CAP-BOUND — with its need (`~N` or `>N`) and the RAM that
    /// surely verifies it, when its floor records a ceiling.
    pub memory: Vec<(String, String, Option<u64>)>,
    /// The machine the run was on, said (`<RAM> GiB RAM, verifier ceiling <C>
    /// GiB`).
    pub host: String,
    /// The ratchet `update` would write, when it changes a row.
    pub ratchet: Option<String>,
    /// The memory record rows `update` would write (`mem_learn`), by library.
    pub memory_rows: Vec<(String, MemRow)>,
    /// What this host learned, by library: `Some((outcome, run ceiling, peak,
    /// proved, floor))` to remember, `None` to forget (the lane's host
    /// observations; a library absent here keeps its row).
    pub host_obs: Vec<(String, Option<[String; 5]>)>,
}

/// What to do while the trustd a run met holds the socket (the lane's
/// `trustd_remedy`): `verdict` is the probe's word, or `dirty`.
/// THE CLOCK'S CREDIT for one library's shortfall: its `timed_out=` lapses
/// always, and the proof checks ay's caller cancelled ([`CHECK_CANCELLED`])
/// only in a filed record's FIRST measurement of it. Those lines come in passing
/// runs too, so counting them every time would leave a real drop they could
/// cover undecided on every run; credited once, the `--remeasure` that
/// completes the record decides it — met, or a regression the lapses it stamps
/// cannot cover. A run that files nothing (a `-p` run) has no re-measure to
/// resolve it, so it is judged on `timed_out=` alone.
fn clock_credit(facts: &Facts, name: &str, m: &Measured) -> u64 {
    let first = facts.provenance.as_ref().is_some_and(|p| {
        !p.remeasured
            .iter()
            .any(|r| r.split_whitespace().skip(2).any(|n| n == name))
    });
    m.lapsed.saturating_add(if first { m.cancelled } else { 0 })
}

/// How to measure AGAIN the libraries a run left undecided, in the words of a
/// lane that can: a run whose facts were filed (they carry a provenance) is
/// completed on its own key by `--remeasure`; a narrowed run, or one that could
/// not file, has no record for `--remeasure` to complete, so its own scope runs
/// again.
fn measure_again(facts: &Facts, names: &[&str]) -> String {
    if facts.provenance.is_some() {
        return "targo --unverified ship trust run --remeasure".into();
    }
    format!(
        "targo --unverified ship trust run -p {}",
        names.join(" -p ")
    )
}

/// Options for the host's process-name probe, or no recipe on an unknown host.
/// macOS excludes ancestors unless `-a` is given; procps on Linux includes
/// them unless its separate `-A` option is given. Neither probe uses `-f`:
/// these patterns match process names, not argv[0] or full arguments.
fn trustd_probe_options() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("-alx")
    } else if cfg!(target_os = "linux") {
        Some("-lx")
    } else {
        None
    }
}

fn trustd_remedy(verdict: &str, sock: &str, exe: &str) -> String {
    let never = "never signal it: a killed trustd records DIRTY and blocks every check until `--recover-after-crash` attests no solver survives";
    match verdict {
        "foreign" => {
            format!("re-run once it retires (it exits 300 s after its last grant); {never}")
        }
        "silent" => format!("re-run when the machine is quieter; {never}"),
        "dirty" => match trustd_probe_options() {
            Some(options) => format!(
                "trustd's crash epoch at {sock} is DIRTY, and targo starts no trustd until someone attests that no solver of the prior daemon survives: from a shell that sees every process on this host (pgrep sees only its own PID namespace, so never from inside a PID-namespaced sandbox), `pgrep {options} 'trustc|rustc|ay|trust-wp.*|targo.*'` checks the standard Trust process names. Only exit 1 means none of those names matched; an error is not a clear probe. Account for any other or renamed solvers this toolchain runs too. Once no prior solver survives, run `{} --recover-after-crash --confirm-no-solvers --socket {}` and re-run; while a process remains, wait for it to exit, and never kill another's build to get there. It is an attestation, never a way to take a live socket",
                shell_word(exe),
                shell_word(sock)
            ),
            None => format!(
                "trustd's crash epoch at {sock} is DIRTY; this host has no vetted process-listing recipe, so no recovery command is offered. Establish a complete host view and that no solver of the prior daemon survives before attesting; never signal trustd or kill another's build"
            ),
        },
        _ => "re-run once trustd admits it".into(),
    }
}

/// How `--update-ratchet` may move the floors ([`judge`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update<'a> {
    /// Write floors: a first floor, a high-water mark or a total corrected
    /// with its floor unchanged, and a re-base of another prover's floor that
    /// this run meets. Never a lowering.
    pub on: bool,
    /// The reason a floor ANOTHER prover recorded is lowered onto this one —
    /// written in the row's fifth column, where the up-only rule reads it
    /// (`ship trust judge --update-ratchet --reason`). Without it no floor goes
    /// down.
    pub reason: Option<&'a str>,
}

impl Update<'_> {
    /// No update: judge only.
    pub const NONE: Update<'static> = Update {
        on: false,
        reason: None,
    };
    /// `--update-ratchet`, which lowers nothing.
    pub const RAISE: Update<'static> = Update {
        on: true,
        reason: None,
    };
}

/// JUDGE a run's facts against a ratchet — the lane's verdict, as it printed
/// it before the judge moved here, less the excuses the owner ruled out (NO
/// FLOOR EXCUSES in the module docs), plus a row the shell could not say: a
/// floor that is not a count fails as `FLOOR-UNPARSED` (the shell died on it,
/// `set -u`, with no verdict).
///
/// `ratchet` is the floors file's text (`None` when there is none);
/// `ratchet_name` and `memory_name` are how the notes name the floors file and
/// the memory record. Each library is judged against the memory row the run
/// used ([`Crate::memory`]). `update` is `--update-ratchet` ([`Update`]).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn judge(
    facts: &Facts,
    ratchet: Option<&str>,
    ratchet_name: &str,
    memory_name: &str,
    update: Update<'_>,
) -> Verdict {
    let driver_row = facts.driver_row();
    let host = facts.host;
    let today = facts.measured_at.get(..10).unwrap_or(&facts.measured_at);
    let sock = facts.trustd.as_str();
    let trustd_exe = Path::new(&facts.driver).parent().map_or_else(
        || "trustd".to_string(),
        |d| d.join("trustd").display().to_string(),
    );
    let mut rows: Vec<[String; 4]> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut mem_notes: Vec<String> = Vec::new();
    let mut fail = false;
    let mut failed: Vec<String> = Vec::new();
    let dash = || "-".to_string();
    for v in &facts.vanished {
        rows.push([v.clone(), "FORK-VANISHED".into(), dash(), dash()]);
        failed.push(format!("{v} FORK-VANISHED"));
        fail = true;
    }
    for (n, a, b) in &facts.ambiguous {
        rows.push([
            n.clone(),
            format!("AMBIGUOUS-SPEC({a} vs {b})"),
            dash(),
            dash(),
        ]);
        failed.push(format!("{n} AMBIGUOUS-SPEC"));
        fail = true;
    }
    let narrowed = !facts.only.is_empty();
    let (mut queued, mut gated) = (0usize, 0usize);
    let (mut queued_what, mut skipped_what) = (String::new(), String::new());
    // MEMORY: what this host could not verify, refused up front (here) or
    // CAP-BOUND after its run (below) — named, each with its need and the RAM
    // that verifies it, and the largest such RAM.
    let (mut mem_what, mut memory) = (String::new(), Vec::new());
    let (mut mem_ram, mut mem_ram_unknown) = (None::<u64>, false);
    let mut note_ram = |sure: Option<u64>| match sure {
        None => mem_ram_unknown = true,
        Some(r) => mem_ram = Some(mem_ram.map_or(r, |m: u64| m.max(r))),
    };
    for c in &facts.crates {
        match &c.decision {
            Decision::Queued(_) => {
                queued += 1;
                queued_what.push_str(&format!(" {}", c.name));
                skipped_what.push_str(&format!(" {} (queued fork)", c.name));
            }
            Decision::Memory(seen) => {
                mem_what.push_str(&format!(" {}", c.name));
                skipped_what.push_str(&format!(" {} (memory)", c.name));
                let r = memory_refusal(&host, &c.name, true, &c.memory, seen.as_ref())
                    .unwrap_or_else(|| Refusal {
                        line: format!(
                            "{} not verified here: this host gives {} ({})",
                            c.name,
                            host.ceiling()
                                .map_or_else(|| "?".into(), |c| format!("{c} GiB")),
                            host.share()
                        ),
                        token: "?".into(),
                        sure: None,
                    });
                mem_notes.push(format!("MEMORY: {}", r.line));
                memory.push((c.name.clone(), r.token, r.sure));
                note_ram(r.sure);
            }
            Decision::Gate(_) => gated += 1,
            Decision::NoLib | Decision::Ambiguous => {}
        }
    }
    let (mut total_proved, mut total_all, mut capbound_n) = (0u64, 0u64, 0usize);
    // Checks that did not finish (OVER-BUDGET, NOT-VERIFIED(incomplete)): each
    // ran and decided nothing, so none is counted as gated.
    let mut unfinished_n = 0usize;
    // UNDECIDED, by what kept it so: trustd (a check it kept from starting, or
    // work it refused within the shortfall) or the wall clock.
    let (mut by_trustd, mut by_clock) = (Vec::<String>::new(), Vec::<String>::new());
    let (mut trustd_dirty, mut trustd_block) = (false, None::<(String, String)>);
    let mut ratchet_now: Option<String> = ratchet.map(str::to_string);
    let mut rewrote = false;
    let mut memory_rows: Vec<(String, MemRow)> = Vec::new();
    let mut host_obs: Vec<(String, Option<[String; 5]>)> = Vec::new();
    // A PATH TARGO REFUSED, with every library whose check it refused: one bad
    // directory fails every check, and is said once.
    let mut refused_paths: BTreeMap<&PathRefusal, Vec<&str>> = BTreeMap::new();
    for c in &facts.crates {
        let name = c.name.as_str();
        let m = match &c.decision {
            Decision::Ambiguous => continue,
            Decision::NoLib => {
                rows.push([name.into(), "NO-LIB".into(), dash(), dash()]);
                continue;
            }
            Decision::Queued(s) => {
                rows.push([name.into(), format!("QUEUED({s})"), dash(), dash()]);
                continue;
            }
            // Refused up front: its MEMORY line says why and what verifies it.
            Decision::Memory(_) => {
                rows.push([name.into(), "REFUSED(memory)".into(), dash(), dash()]);
                continue;
            }
            Decision::Gate(m) => m,
        };
        // A PATH TARGO REFUSED is the host's, whatever row its check earns below
        // (NOT-VERIFIED, NOT-VERIFIED(trustd), GATE-BROKE): the row keeps its
        // status, and its `REFUSED PATH:` line says why and what to do.
        for r in &m.path_refused {
            refused_paths.entry(r).or_default().push(name);
        }
        // OVER BUDGET (the lane's THE BUDGET): the lane ended the check at its
        // per-crate wall budget, so whatever its log holds is a check that did
        // not finish. Read before anything the log says: a red row naming the
        // crate and the budget, in neither total, never a floor.
        if !m.wall_budget.is_empty() {
            let secs = m.wall_budget.parse::<u64>().ok();
            let s = format!("OVER-BUDGET({}s)", m.wall_budget);
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            unfinished_n += 1;
            skipped_what.push_str(&format!(" {name} (over its wall budget)"));
            notes.push(format!(
                "OVER BUDGET: {name}: still verifying when its per-crate wall budget of {} ran out, so the lane ended its check there and nothing was decided about it — a verification that does not finish is not verified; give it longer with --crate-budget <seconds>, or find what holds it (its log, up to where it was ended)",
                secs.map_or_else(
                    || "(unnamed)".to_string(),
                    |s| if s < 60 {
                        format!("{s} s")
                    } else {
                        format!("{s} s ({})", span(s))
                    }
                )
            ));
            fail = true;
            continue;
        }
        // A CHECK TARGO WOULD NOT START compiled nothing: the machine's trustd
        // kept it from starting, which decides nothing about the crate —
        // NOT-VERIFIED(trustd), never GATE-BROKE. The verdict names what kept
        // it: a DIRTY crash epoch, the trustd that held the socket, or — when
        // that was this build's, answering the lane in time — this build's
        // trustd answering targo too late for it.
        if !m.launch.is_empty() {
            if m.launch == "dirty" {
                trustd_dirty = true;
            } else {
                match m.held.as_str() {
                    "foreign" | "silent" => {
                        trustd_block = Some((m.held.clone(), m.held_what.clone()));
                    }
                    "ours" => {
                        trustd_block = Some((
                            "silent".into(),
                            format!(
                                "{}, and still targo did not see it answer within the half second it gives",
                                m.held_what
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            rows.push([name.into(), "NOT-VERIFIED(trustd)".into(), dash(), dash()]);
            by_trustd.push(name.to_string());
            skipped_what.push_str(&format!(" {name} (trustd did not admit it)"));
            let cause = match m.launch.as_str() {
                "dirty" => "trustd's crash epoch is DIRTY (its log carries the recovery command)",
                "owned" => {
                    "another trustd held the socket and did not prove itself this build's within targo's half second"
                }
                "unready" => "the trustd it started did not answer in time",
                _ => "no trustd of its build could serve it (its log says why)",
            };
            let held = match m.held.as_str() {
                "none" => "nothing",
                "ours" => "this build's trustd",
                "foreign" => "a trustd of another build",
                "silent" => "a trustd that did not answer in time",
                _ => "what no probe could tell",
            };
            notes.push(format!(
                "NOT VERIFIED: {name}: targo would not start its check — {cause}; the socket then held {held}"
            ));
            continue;
        }
        // VERIFICATION THAT DID NOT RUN is not a verdict about the crate.
        if m.transport {
            let s = format!("NOT-VERIFIED(rc={})", m.rc);
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            fail = true;
            continue;
        }
        // NOR IS ONE THAT DID NOT FINISH ("A CHECK THAT DID NOT FINISH", the
        // module docs), whatever its Summary counts: never scored, never held to
        // a floor or written as one, in neither total, and it fails — a
        // verification that broke, like GATE-BROKE.
        if !m.ended.is_empty() {
            let counted = match parse_summary(&m.summary) {
                (Some(p), Some(t)) => {
                    format!(
                        "its Summary, {p} proved of {t}, counts only what ran before it stopped"
                    )
                }
                _ if m.summary.is_empty() => "it left no Summary".to_string(),
                _ => "its Summary does not parse".to_string(),
            };
            let s = "NOT-VERIFIED(incomplete)".to_string();
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            unfinished_n += 1;
            skipped_what.push_str(&format!(" {name} (did not finish)"));
            notes.push(format!(
                "NOT VERIFIED: {name}: its verification did not finish ({}) — {counted}, so nothing was decided about it; re-run it",
                m.ended
            ));
            fail = true;
            continue;
        }
        if m.summary.is_empty() {
            let s = format!("GATE-BROKE(rc={})", m.rc);
            failed.push(format!("{name} {s}"));
            rows.push([name.into(), s, dash(), dash()]);
            fail = true;
            continue;
        }
        let (Some(proved), Some(all)) = parse_summary(&m.summary) else {
            failed.push(format!("{name} SUMMARY-UNPARSED"));
            rows.push([name.into(), "SUMMARY-UNPARSED".into(), dash(), dash()]);
            fail = true;
            continue;
        };
        let row = floor_of(ratchet_now.as_deref(), name);
        // A FLOOR THAT IS NOT A COUNT decides nothing (the shell died on it).
        let floor: Option<u64> = match &row {
            None => None,
            Some(r) if r.proved.is_empty() => None,
            Some(r) => match r.proved.parse() {
                Ok(n) => Some(n),
                Err(_) => {
                    failed.push(format!("{name} FLOOR-UNPARSED"));
                    rows.push([
                        name.into(),
                        "FLOOR-UNPARSED".into(),
                        proved.to_string(),
                        all.to_string(),
                    ]);
                    notes.push(format!(
                        "BAD FLOOR: {name}: its row in {ratchet_name} gives the floor `{}`, which is not a count — fix the row (the verify stage's up-only check names it too)",
                        r.proved
                    ));
                    fail = true;
                    continue;
                }
            },
        };
        let floor_driver = row.as_ref().map_or("", |r| r.driver.as_str());
        let floor_total = row.as_ref().map_or("", |r| r.total.as_str());
        let floor_why = row.as_ref().map_or("", |r| r.why.as_str());
        let own_floor = floor_driver.is_empty() || floor_driver == driver_row;
        let lapsed = clock_credit(facts, name, m);
        let mem = &c.memory;
        let cen = Census::of(m);
        // CAP-BOUND (MEMORY): a drop under the floor's own prover that the lapses
        // do not cover, from a run that ran out of its ceiling, when the floor was
        // set at a larger ceiling than this run's — unless the memory row says the
        // library FITS this run's ceiling on this host ([`fits`]): then it needed
        // more than its row says, and the drop is the code's. Never a pass: the
        // run is NARROWED, which no cut accepts.
        let capbound = floor.is_some_and(|f| proved.saturating_add(lapsed) < f)
            && own_floor
            && cen.exhausted
            && mem.f != "-"
            && cen
                .ceil
                .as_deref()
                .is_some_and(|x| !gib_le(&mem.f, x) && !fits(mem, &host, x));
        // AN OBLIGATION THE VERIFIER NEVER DISPATCHED (trustd refused it): each
        // refusal takes at most one proof away. A drop they, with the lapses,
        // could cover decided nothing; a first floor is not recorded from such a
        // run; a larger drop is scored, whoever recorded the floor.
        if m.refused > 0 {
            let (refused, why) = (m.refused, &m.refused_why);
            match floor {
                Some(f) if proved >= f => notes.push(format!(
                    "ADMISSION REFUSED: {name}: {refused} obligation(s) refused memory admission ({why}), and still {proved} proved against its floor of {f} — the floor held; what was refused was not decided"
                )),
                None if !update.on => notes.push(format!(
                    "ADMISSION REFUSED: {name}: {refused} obligation(s) refused memory admission ({why}) — record its floor on a run trustd admits"
                )),
                Some(f) if proved.saturating_add(refused).saturating_add(lapsed) < f => {
                    let tail = if capbound {
                        ", in a run that ran out of its memory ceiling — CAP-BOUND"
                    } else {
                        " — the drop is the code's"
                    };
                    notes.push(format!(
                        "ADMISSION REFUSED: {name}: {refused} obligation(s) refused memory admission ({why}) and {lapsed} lapsed cannot cover {proved} proved against its floor of {f}{tail}"
                    ));
                }
                _ => {
                    let against = floor
                        .map_or_else(|| "no floor".to_string(), |f| format!("its floor of {f}"));
                    rows.push([name.into(), "NOT-VERIFIED(trustd)".into(), dash(), dash()]);
                    by_trustd.push(name.to_string());
                    skipped_what.push_str(&format!(" {name} (trustd did not admit it)"));
                    notes.push(format!(
                        "NOT VERIFIED: {name}: {refused} obligation(s) refused memory admission ({why}) left {proved} proved against {against} — the shortfall is the machine's, so nothing was decided; measure it again once trustd admits its work: {}",
                        measure_again(facts, &[name])
                    ));
                    continue;
                }
            }
        }
        // CAP-BOUND: the drop is this host's ceiling, not the code. In neither
        // total and no failure, and no pass: NARROWED. No floor is written or
        // lowered, `--update-ratchet` included (which records only the need it
        // learned); remembered, so the next run here refuses it up front.
        if let (true, Some(f), Some(x)) = (capbound, floor, cen.ceil.as_deref()) {
            rows.push([name.into(), format!("CAP-BOUND(<{f})"), dash(), dash()]);
            capbound_n += 1;
            mem_what.push_str(&format!(" {name}"));
            skipped_what.push_str(&format!(" {name} (cap-bound)"));
            // Its need is more than the larger of its peak and the ceiling it ran
            // out of, which is the run's own, said with the RAM whose eighth it is.
            let low = gib_max(cen.peak.as_deref().unwrap_or("0"), x);
            let sure = gib(&mem.f).map(ram_for);
            let may = (!gib_le(&mem.f, &low)).then(|| ram_above(gib(&low).unwrap_or(0.0)));
            mem_notes.push(format!(
                "MEMORY: {name} cap-bound: ran out at its {x} GiB ceiling ({} GiB RAM / 8): {proved} proved vs floor {f}, set at {} GiB; floor kept, refused up front next run; {}",
                ram_for(gib(x).unwrap_or(0.0)),
                mem.f,
                host.ram_phrase(sure, may)
            ));
            memory.push((name.to_string(), format!(">{low}"), sure));
            note_ram(sure);
            host_obs.push((
                name.into(),
                Some([
                    "cap-bound".into(),
                    x.to_string(),
                    cen.peak.clone().unwrap_or_else(dash),
                    proved.to_string(),
                    f.to_string(),
                ]),
            ));
            if update.on
                && let Some(r) = mem_learn(mem, &mem.f, &cen, today, &host)
            {
                memory_rows.push((name.into(), r));
            }
            continue;
        }
        // A run that ran out and still met its floor is remembered as `held`, so
        // the next run here gates it rather than refusing it; any other run
        // forgets the library (one that did not run out learned nothing to keep,
        // and one that fell short must never turn into a refusal next time). Its
        // ceiling is the one its log names, never this host's RAM / 8: a log that
        // names none (a seal from trust main has no in-run ceiling) is filed `-`,
        // which no later run reads as a ceiling it held at.
        host_obs.push((
            name.into(),
            match (cen.exhausted, floor) {
                (true, Some(f)) if proved >= f => Some([
                    "held".into(),
                    cen.ceil.clone().unwrap_or_else(dash),
                    cen.peak.clone().unwrap_or_else(dash),
                    proved.to_string(),
                    f.to_string(),
                ]),
                _ => None,
            },
        ));
        // A BUDGET THAT LAPSED decides nothing (no floor excuses). The
        // per-function budget is wall-clock (`Function budget: 120000ms` in the
        // report header), and a lapse stamps its whole batch `timed_out=N`
        // (measured 2026-09-27 at load ~100). A drop the run's own lapses could
        // cover is the machine's or the code's, and this run cannot say which: it
        // is UNDECIDED — in neither total, COULD NOT RUN, never a pass — until the
        // library is measured again on the same key (`--remeasure`). A drop
        // larger than every lapse in the run is a regression, below. A proof
        // check ay's caller cancelled is credited once ([`clock_credit`]).
        if let Some(f) = floor.filter(|f| proved < *f && proved.saturating_add(lapsed) >= *f) {
            rows.push([
                name.into(),
                format!("NOT-VERIFIED(clock,<{f},{lapsed})"),
                dash(),
                dash(),
            ]);
            by_clock.push(name.to_string());
            skipped_what.push_str(&format!(" {name} (budgets lapsed)"));
            let cancelled = lapsed - m.lapsed;
            let why = match (m.lapsed, cancelled) {
                (0, c) => format!("{c} proof check(s) were cancelled"),
                (n, 0) => format!("{n} obligation(s) ran out of time"),
                (n, c) => format!(
                    "{n} obligation(s) ran out of time and {c} proof check(s) were cancelled"
                ),
            };
            notes.push(format!(
                "BUDGET LAPSED: {name}: {proved} proved, floor {f}; {why} — undecided; measure it again when the machine is quieter: {}",
                measure_again(facts, &[name])
            ));
            continue;
        }
        total_proved = total_proved.saturating_add(proved);
        total_all = total_all.saturating_add(all);
        let mut status = if proved == all {
            "CLEAN".to_string()
        } else {
            "open".to_string()
        };
        let mut bad = false;
        match floor {
            None if update.on => status = "FLOOR-RECORDED".into(),
            None => {
                status = "NO-FLOOR".into();
                bad = true;
                notes.push(format!(
                    "NO FLOOR: {name}: gated with no floor in {ratchet_name} — record it: targo --unverified ship trust run -p {name} --update-ratchet, and commit the row"
                ));
            }
            Some(f) if proved < f => {
                status = format!("RATCHET-REGRESSION(<{f})");
                bad = true;
                // ANOTHER PROVER SET THE FLOOR, and it is held (no floor excuses).
                // A new prover meets the floors it inherits; when it
                // proves less, the row goes down onto it only with a reason in its
                // fifth column, which the up-only rule admits.
                if !own_floor {
                    notes.push(format!(
                        "DRIVER CHANGED: {name}: its floor of {f} was recorded under {floor_driver}; this run's prover is {driver_row}, and proved {proved}: fix what it no longer proves, or lower the row onto it with a reason (`targo --unverified ship trust judge --update-ratchet --reason '<why>'`)"
                    ));
                }
                // A CEILING THAT RAN OUT is said beside the failure, with why it
                // does not excuse it.
                let skipped = m.skipped.unwrap_or(0);
                let exhausted = m.budget_rows.saturating_add(skipped);
                if exhausted > 0 {
                    let skipped_note = if skipped > 0 {
                        format!(
                            "; {skipped} of them were bodies skipped at the ceiling, never verified"
                        )
                    } else {
                        String::new()
                    };
                    let ceiling = cen.ceil.as_deref().map_or_else(
                        || "not named in its log".to_string(),
                        |x| format!("{x} GiB"),
                    );
                    let run_c = cen.ceil.as_deref();
                    let under =
                        !cen.exhausted && !cen.atceil && cen.peak.is_some() && cen.ceil.is_some();
                    let mem_why = if mem.f == "-" {
                        format!(
                            "{memory_name} records no ceiling for its floor, so the drop cannot be called the host's; it still fails; record it on a host that holds the floor: targo --unverified ship trust run -p {name} --update-ratchet"
                        )
                    } else if !own_floor {
                        "its floor is another prover's, and a new prover's shortfall is not called the host's; it still fails".to_string()
                    } else if run_c.is_some_and(|x| gib_le(&mem.f, x)) {
                        format!(
                            "its floor was set at {} GiB, no more than this run's, so the drop is the code's",
                            mem.f
                        )
                    } else if let Some(x) = run_c.filter(|x| cen.exhausted && fits(mem, &host, x)) {
                        let on = if Reading::of(mem, &host) == Reading::Here {
                            "this host's RAM".to_string()
                        } else {
                            format!("{} GiB of RAM", mem.ram)
                        };
                        format!(
                            "{memory_name} records its need as ~{} GiB, measured with room on {on}, which fits this run's {x} GiB: it needed more than its record says, so the drop is the code's",
                            mem.need
                        )
                    } else if cen.exhausted {
                        format!(
                            "its floor was set at {} GiB, and this run's log names no ceiling to compare it with, so the drop cannot be called the host's; it still fails",
                            mem.f
                        )
                    } else if under {
                        "the drop is not the host's".to_string()
                    } else {
                        format!(
                            "its floor was set at {} GiB, and this run's log cannot show whether it ran out of memory, so the drop cannot be called the host's; it still fails",
                            mem.f
                        )
                    };
                    // Memory is named only when the census shows it ran out: a
                    // budget row alone also trips on time and steps. A peak the
                    // log names under the ceiling it names rules memory out.
                    let what = if cen.exhausted {
                        format!(
                            "{exhausted} function(s) ran out of the verifier's memory ceiling ({ceiling}{skipped_note})"
                        )
                    } else if under {
                        format!(
                            "{exhausted} function(s) hit the verifier's budget (time or steps; peak {} under its {} ceiling)",
                            m.peak, m.ceiling
                        )
                    } else {
                        format!(
                            "{exhausted} function(s) hit the verifier's budget (ceiling {ceiling})"
                        )
                    };
                    notes.push(format!(
                        "MEMORY CEILING: {name}: {proved} proved against a floor of {f}; {what} — {mem_why}"
                    ));
                }
            }
            Some(_) => {}
        }
        // A FLOOR LOWERED BY HAND IS HELD TO ITS TOTAL.
        if !floor_why.is_empty() && !floor_total.is_empty() && floor_driver == driver_row {
            match floor_total.parse::<u64>() {
                Ok(t) if all > t => {
                    if status == "open" || status == "CLEAN" {
                        status = format!("TOTAL-ABOVE-ROW(>{t})");
                    }
                    bad = true;
                    let set = floor.map_or_else(String::new, |f| format!("floor {f}, "));
                    // `--update-ratchet` lowers no floor, so below it only a
                    // hand edit moves the row.
                    let todo = if floor.is_some_and(|f| proved < f) {
                        format!(
                            " and proves {proved}, below the floor — fix the regression, or edit the row by hand (floor, total {all}, and the reason in its fifth column)"
                        )
                    } else {
                        format!(
                            " — re-measure it: targo --unverified ship trust run -p {name} --update-ratchet, and commit the row"
                        )
                    };
                    notes.push(format!(
                        "HAND-LOWERED: {name}: its row sets {set}total {t}; this run measures {all} obligations{todo}"
                    ));
                }
                Ok(_) => {}
                Err(_) => {
                    status = "FLOOR-UNPARSED".into();
                    bad = true;
                    notes.push(format!(
                        "BAD FLOOR: {name}: its row in {ratchet_name} gives the total `{floor_total}`, which is not a count — fix the row"
                    ));
                }
            }
        }
        if bad {
            fail = true;
            failed.push(format!("{name} {status} {proved}/{all}"));
        }
        rows.push([name.into(), status, proved.to_string(), all.to_string()]);
        // UP, NEVER DOWN. `--update-ratchet` writes a first floor, a high-water
        // mark (a total corrected with its floor unchanged included), and a
        // re-base of another prover's floor this run meets. A floor this run
        // falls below stays — whoever recorded it — unless the floor is another
        // prover's and a REASON is given, written in the row's fifth column. THE
        // RE-BASE GUARD (MEMORY): a lowering from a run under a ceiling smaller
        // than the one the floor was set at would write a small host's count as
        // the floor every larger host then passes without trying — so it is
        // withheld, and said, unless the run's census shows it had room. The
        // memory record is kept current beside it: the floor ceiling becomes this
        // run's when it writes a new floor value, the smaller of the two when it
        // rewrites the same value, and stays when it leaves the floor alone.
        // THIS RUN'S CEILING is the one its log names ([`Measured::ceiling`]),
        // never this host's RAM / 8: a seal from trust main has no in-run ceiling
        // and its logs name none, so a floor value such a run writes was set at no
        // ceiling (`-`, absent), and its lowering of a floor set at one is withheld,
        // since nothing shows that run had the room the floor was set with.
        if update.on {
            let mut nf = mem.f.clone();
            let run_c = cen.ceil.clone();
            let lowers = floor.is_some_and(|f| proved < f);
            let reason = update.reason.filter(|_| lowers && !own_floor);
            if lowers && reason.is_none() {
                if !own_floor {
                    notes.push(format!(
                        "NOT LOWERED: {name}: {proved} proved stays below the floor of {} another prover recorded — --update-ratchet lowers no floor; it goes down onto this prover only with its reason: `targo --unverified ship trust judge --update-ratchet --reason '<why>'`",
                        floor.unwrap_or(0)
                    ));
                }
            } else {
                let withhold = match (floor, run_c.as_deref()) {
                    (Some(f), _) if proved >= f || mem.f == "-" => None,
                    (None, _) => None,
                    (Some(_), None) => Some(format!(
                        "this run's log names no verifier ceiling (a seal from trust main has none), and its floor was set at {} GiB; re-base it from a run whose log names its ceiling",
                        mem.f
                    )),
                    (Some(_), Some(rc))
                        if !gib_le(&mem.f, rc)
                            && (cen.peak.is_none()
                                || cen.skipped.is_some_and(|n| n > 0)
                                || cen.budget > 0
                                || cen.exhausted) =>
                    {
                        let how = if cen.exhausted {
                            "this run ran out of it"
                        } else {
                            "nothing in its census shows it had room"
                        };
                        Some(format!(
                            "this host's {rc} GiB ceiling is below the {} GiB its floor was set at, and {how}; re-base it on a host with {} GiB",
                            mem.f,
                            ram_for(gib(&mem.f).unwrap_or(0.0))
                        ))
                    }
                    (Some(_), Some(_)) => None,
                };
                if let Some(w) = withhold {
                    notes.push(format!("RE-BASE WITHHELD: {name}: {w}"));
                } else {
                    let mut lines: Vec<String> = ratchet_now
                        .as_deref()
                        .unwrap_or("")
                        .lines()
                        .filter(|l| l.split('\t').next() != Some(name))
                        .map(str::to_string)
                        .collect();
                    lines.push(match reason {
                        Some(why) => {
                            format!("{name}\t{proved}\t{all}\t{driver_row}\t{}", one_line(why))
                        }
                        None => format!("{name}\t{proved}\t{all}\t{driver_row}"),
                    });
                    lines.sort();
                    let mut text = lines.join("\n");
                    text.push('\n');
                    ratchet_now = Some(text);
                    rewrote = true;
                    let same = floor == Some(proved) && own_floor;
                    nf = match run_c {
                        Some(rc) if same && mem.f != "-" => gib_min(&mem.f, &rc),
                        Some(rc) => rc,
                        // The same value rewritten stands at the ceiling its
                        // row names; a value from a run whose log names no
                        // ceiling was set at none.
                        None if same => mem.f.clone(),
                        None => dash(),
                    };
                }
            }
            if let Some(r) = mem_learn(mem, &nf, &cen, today, &host) {
                memory_rows.push((name.into(), r));
            }
        }
    }

    let unverified: Vec<String> = by_trustd.iter().chain(&by_clock).cloned().collect();
    // REFUSED PATH, said ahead of every other note: what failed the checks it
    // names, and the one thing that lets them run.
    let me = aterm_uds::peer::our_uid();
    let refused_notes: Vec<String> = refused_paths
        .iter()
        .map(|(r, names)| refused_path_note(r, names, me))
        .collect();
    let memory_n = mem_what.split_whitespace().count();
    let gated_n = gated.saturating_sub(capbound_n + unverified.len() + unfinished_n);
    let mut out = String::from("\n");
    let row_line = |r: &[String; 4]| format!("{:<28} {:<26} {:>8} {:>8}\n", r[0], r[1], r[2], r[3]);
    out.push_str(&row_line(&[
        "CRATE".into(),
        "STATUS".into(),
        "PROVED".into(),
        "TOTAL".into(),
    ]));
    for r in &rows {
        out.push_str(&row_line(r));
    }
    out.push('\n');
    out.push_str(&format!("TOTAL: {total_proved}/{total_all} proved\n"));
    let id = if facts.driver_id.is_empty() {
        "no --version answer"
    } else {
        facts.driver_id.as_str()
    };
    out.push_str(&format!("DRIVER: {} ({id})\n", facts.driver));
    // THE HOST'S VERIFIER CEILING (MEMORY): what it refused up front by.
    match host.ceiling() {
        Some(c) => out.push_str(&format!(
            "HOST: {} GiB RAM, verifier ceiling {c} GiB per library (targo trust: RAM / 8)\n",
            host.ram_gib()
        )),
        None => out.push_str("HOST: memory unreadable, nothing refused up front\n"),
    }
    // WHETHER TRUSTD WAS HELD: a run that gated a crate with no grant held ran
    // with trustd free to go idle under it.
    if gated > 0 {
        match facts.grants.as_str() {
            "1" => out.push_str(&format!("TRUSTD: held by this run's keeper ({sock})\n")),
            "" if !facts.trustd_seen.is_empty() => {
                let what = facts
                    .trustd_seen
                    .split_once('\t')
                    .map_or(facts.trustd_seen.as_str(), |(_, w)| w);
                out.push_str(&format!(
                    "TRUSTD: never held — the keeper holds only this build's trustd, and {what}\n"
                ));
            }
            "" => out.push_str(&format!(
                "TRUSTD: never held — no trustd granted the keeper's request at {sock}\n"
            )),
            n => out.push_str(&format!(
                "TRUSTD: held by this run's keeper, {n} grants — trustd went away under a live grant and came back ({sock})\n"
            )),
        }
    }
    for n in refused_notes.iter().chain(&mem_notes).chain(&notes) {
        out.push_str(n);
        out.push('\n');
    }
    let skipped_shown = if skipped_what.is_empty() {
        " none"
    } else {
        skipped_what.as_str()
    };
    // A CAP-BOUND library, and one nothing was decided about, ran and decided
    // nothing about its floor: neither is counted.
    out.push_str(&format!(
        "GATED: {gated_n} of {} selected; not verified:{skipped_shown}\n",
        facts.crates.len()
    ));
    if queued > 0 {
        out.push_str(&format!(
            "SCOPE: {queued} vendored fork(s) QUEUED, not gated, nothing claimed:{queued_what}\n"
        ));
    }
    let roster_missing = facts.vanished.len();
    if total_all > 0 && total_proved == total_all {
        if !narrowed && queued == 0 && !fail && unverified.is_empty() && memory_n == 0 {
            out.push_str("VERDICT: 100% MACHINE-PROVED (every workspace library + every vendored fork, advisory lane: every obligation proved)\n");
        } else if narrowed || queued > 0 || roster_missing > 0 || fail || !unverified.is_empty() {
            // Memory alone narrows nothing a re-run here could lift: the NARROWED
            // verdict names it and the RAM that verifies it.
            out.push_str("VERDICT: 100% PROVED OF WHAT RAN — NOT the workspace claim.\n");
            out.push_str("         Everything gated above is clean; the campaign claim is the\n");
            out.push_str("         WHOLE-TREE run, and this run was narrower:\n");
            if narrowed {
                out.push_str(&format!(
                    "     - -p restricted the run to: {}\n",
                    facts.only
                ));
            }
            if queued > 0 {
                out.push_str(&format!(
                    "     - {queued} vendored fork(s) QUEUED (not converted), nothing is claimed about them:{queued_what}\n"
                ));
                out.push_str("       Queued: aterm-release trust_lane.rs QUEUED_FORKS\n");
            }
            if roster_missing > 0 {
                out.push_str(&format!(
                    "     - {roster_missing} librar(ies) a floor or QUEUED_FORKS names are NOT in the resolved graph (FORK-VANISHED): a floor row goes with its library\n"
                ));
            }
            if fail {
                out.push_str("     - at least one crate failed (the NO-FLOOR / TOTAL-ABOVE-ROW / GATE-BROKE / NOT-VERIFIED / OVER-BUDGET /\n");
                out.push_str("       SUMMARY-UNPARSED / AMBIGUOUS-SPEC / FORK-* rows above); one with no usable Summary, or one that\n");
                out.push_str("       did not finish, is in NEITHER total\n");
            }
            if !by_trustd.is_empty() {
                out.push_str(&format!(
                    "     - trustd did not admit the work, so nothing was decided about: {} (in NEITHER total)\n",
                    by_trustd.join(" ")
                ));
            }
            if !by_clock.is_empty() {
                out.push_str(&format!(
                    "     - lapsed budgets could cover the shortfall, so nothing was decided about: {} (in NEITHER total)\n",
                    by_clock.join(" ")
                ));
            }
            out.push_str("         Re-run the whole tree, with every exclusion lifted, before quoting a whole-tree number.\n");
        }
    }
    let (word, exit, mut tail) = if fail {
        ("FAIL", 1, format!("VERDICT: FAIL — {}", failed.join("; ")))
    } else if !unverified.is_empty() {
        // The one line the verify stage puts in its row: what kept each library
        // undecided, and what to do.
        let mut said = Vec::new();
        if !by_trustd.is_empty() {
            let why = if trustd_dirty {
                trustd_remedy("dirty", sock, &trustd_exe)
            } else if let Some((verdict, what)) = &trustd_block {
                format!("{what}: {}", trustd_remedy(verdict, sock, &trustd_exe))
            } else {
                trustd_remedy("", sock, &trustd_exe)
            };
            said.push(format!(
                "trustd did not admit the work, so nothing was decided about: {}; {why}",
                by_trustd.join(" ")
            ));
        }
        if !by_clock.is_empty() {
            said.push(format!(
                "lapsed wall-clock budgets could cover the shortfall, so nothing was decided about: {}; measure them again when the machine is quieter: {}",
                by_clock.join(" "),
                measure_again(facts, &by_clock.iter().map(String::as_str).collect::<Vec<_>>())
            ));
        }
        (
            "COULD-NOT-RUN",
            2,
            format!("VERDICT: COULD NOT RUN — {}", said.join("; and ")),
        )
    } else if memory_n > 0 {
        // NARROWED BY MEMORY: nothing failed and nothing was left undecided, but
        // this host could not verify every library it was asked about. Not a
        // pass about those libraries, and not a finding about the code.
        let ram_all = match (mem_ram, mem_ram_unknown) {
            (None, _) if memory_n == 1 => {
                "no RAM is known to verify it (its floor records no ceiling)".to_string()
            }
            (None, _) => "no RAM is known to verify them (no floor of theirs records its ceiling)"
                .to_string(),
            (Some(r), true) => {
                format!("{r} GiB of RAM verifies those whose floor records its ceiling")
            }
            (Some(r), false) if memory_n == 1 => format!("{r} GiB of RAM verifies it"),
            (Some(r), false) => format!("{r} GiB of RAM verifies them all"),
        };
        // "Nothing that fit failed" only when something fit.
        let fit = if gated_n > 0 {
            "; nothing that fit failed"
        } else {
            ""
        };
        (
            "NARROWED",
            4,
            format!(
                "VERDICT: NARROWED BY MEMORY — not verified on this host ({}):{mem_what}{fit}; {ram_all}",
                host.desc()
            ),
        )
    } else {
        ("PASS", 0, String::new())
    };
    // The verdict's own line names what to do about a refused path too: a re-run
    // alone meets the same refusal.
    if !tail.is_empty() && !refused_paths.is_empty() {
        let said: Vec<String> = refused_paths
            .keys()
            .map(|r| {
                format!(
                    "targo refused {} ({}): {}",
                    r.path,
                    r.said(me),
                    r.remedy(me)
                )
            })
            .collect();
        tail.push_str(&format!(" — REFUSED PATH: {}", said.join("; ")));
    }
    let none_if_empty = |s: &str| {
        let s = s.trim();
        if s.is_empty() {
            "none".to_string()
        } else {
            s.to_string()
        }
    };
    Verdict {
        report: out,
        tail,
        word,
        exit,
        total: format!("{total_proved}/{total_all}"),
        not_verified: none_if_empty(&skipped_what),
        failed,
        unverified,
        memory,
        host: host.desc(),
        ratchet: rewrote.then_some(ratchet_now).flatten(),
        memory_rows,
        host_obs,
    }
}

// ---------------------------------------------------------------------------
// THE UP-ONLY FLOOR, for facts reused on another commit
// ---------------------------------------------------------------------------

/// THE UP-ONLY FLOOR ON REUSE. Facts measured on commit A
/// judge commit B when B differs from A only in [`RATCHET`]
/// — so the floors are the ONE input that differs, and B's floors are held
/// here to the ratchet's up-only rule: every row of `measured` (A's floors) whose
/// floor `judged` (B's) lowers — or whose prover column it rewrites — named. `driver` is the
/// `--version` line of the prover that MEASURED the facts; `gated(name)` says
/// the facts verified that library.
///
/// A floor goes DOWN in exactly two ways, each saying WHY in the row's fifth
/// column (a reason the measured commit's row did not already give): a RE-BASE
/// onto `driver`, the prover the facts were measured with, of a floor another
/// prover recorded (what `ship trust judge --update-ratchet --reason` writes —
/// a new prover meets the floors it inherits, or says why it does not); or a
/// hand edit under the same prover whose proved count fell by no more than its
/// total. A re-base that does not lower is free. A row's prover column changes
/// only onto `driver`: the facts measured that prover and no other, and the
/// judge holds some checks (CAP-BOUND, the hand-lowered total) to a floor's own
/// prover alone. For the same reason a floor that APPEARS after the measured
/// commit for a library the facts verify names `driver`: a ratchet edit may add
/// a new row under any prover (a branch recorded before a seal changed),
/// but here the facts' prover is known exactly. A row removed while the facts
/// still verify its library lowers it to nothing. A floor or total that is not
/// a count is named too. With no `driver` (the facts' prover gave no
/// `--version` line) no re-base, re-label or new row is admitted.
#[must_use]
pub fn floors_lowered(
    measured: &str,
    judged: &str,
    gated: &dyn Fn(&str) -> bool,
    driver: Option<&str>,
) -> Vec<String> {
    struct Row<'a> {
        name: &'a str,
        proved: &'a str,
        total: &'a str,
        prover: &'a str,
        why: &'a str,
    }
    fn rows(text: &str) -> Vec<Row<'_>> {
        text.lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let mut f = l.split('\t');
                let name = f.next()?;
                Some(Row {
                    name,
                    proved: f.next().unwrap_or(""),
                    total: f.next().unwrap_or(""),
                    prover: f.next().unwrap_or(""),
                    why: f.next().unwrap_or("").trim(),
                })
            })
            .collect()
    }
    let driver_shown = driver.unwrap_or("its prover gave no `--version` line");
    let (was_rows, now) = (rows(measured), rows(judged));
    let mut out = Vec::new();
    for r in &now {
        if r.proved.parse::<u64>().is_err() {
            out.push(format!("{}: floor `{}` is not a count", r.name, r.proved));
        }
        if r.total.parse::<u64>().is_err() {
            out.push(format!("{}: total `{}` is not a count", r.name, r.total));
        }
        if gated(r.name) && !was_rows.iter().any(|w| w.name == r.name) && driver != Some(r.prover) {
            out.push(format!(
                "{}: a new floor {} under `{}`, which is not the prover that measured the facts \
                 ({driver_shown})",
                r.name, r.proved, r.prover
            ));
        }
    }
    for was in &was_rows {
        let Ok(was_n) = was.proved.parse::<u64>() else {
            continue;
        };
        let Some(is) = now.iter().find(|r| r.name == was.name) else {
            if gated(was.name) {
                out.push(format!(
                    "{}: floor {was_n} removed while the facts still verify it",
                    was.name
                ));
            }
            continue;
        };
        let Ok(is_n) = is.proved.parse::<u64>() else {
            continue;
        };
        if is.prover != was.prover {
            if driver != Some(is.prover) {
                out.push(format!(
                    "{}: floor {was_n} -> {is_n} re-based onto `{}`, which is not the prover \
                     that measured the facts ({driver_shown})",
                    was.name, is.prover
                ));
            } else if is_n < was_n && (is.why.is_empty() || is.why == was.why) {
                // A new prover meets the floors it inherits, or its row says why
                // not: a re-base goes down only with a new reason (no total rule —
                // another prover's obligations are another set).
                out.push(format!(
                    "{}: floor {was_n} -> {is_n} re-based onto the measuring prover with no new \
                     reason in the row's fifth column: a new prover meets the floors it \
                     inherits, or its row says why it does not",
                    was.name
                ));
            }
            continue;
        }
        if is_n >= was_n {
            continue;
        }
        if is.why.is_empty() || is.why == was.why {
            out.push(format!(
                "{}: floor {was_n} -> {is_n} under the same prover ({}) and no new reason in the \
                 row's fifth column",
                was.name, was.prover
            ));
            continue;
        }
        // The reason must be true of the row: the proved obligations that
        // went, went with their code, so the total fell at least as far. A
        // total that is not a count shows nothing.
        let gone = match (was.total.parse::<u64>(), is.total.parse::<u64>()) {
            (Ok(w), Ok(i)) => Some(w.saturating_sub(i)),
            _ => None,
        };
        if gone.is_none_or(|g| was_n - is_n > g) {
            out.push(format!(
                "{}: floor {was_n} -> {is_n} under the same prover ({}), but the total went {} -> \
                 {}: a reason covers only proved obligations that left with their code",
                was.name, was.prover, was.total, is.total
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// THE KEY
// ---------------------------------------------------------------------------

/// SHA-256 over the tree of `rev` with the files `--update-ratchet` rewrites
/// removed: every `git ls-tree -r -z --full-tree` record (`<mode> <type>
/// <object>\t<path>`, NUL-terminated, a submodule's gitlink included) but the ones
/// at [`RATCHET`], [`MEMORY`] and [`COSTS`]. Two commits whose only difference is
/// those files answer the same digest; any other byte of any other file moves it.
///
/// # Errors
/// git cannot list `rev`'s tree.
pub fn tree_sans_floors(git: &dyn GitRunner, rev: &str) -> Result<String> {
    let out = git_ok(git, &["ls-tree", "-r", "-z", "--full-tree", rev])?;
    let mut h = Sha256::new();
    let mut entries = 0usize;
    for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let path = entry
            .iter()
            .position(|b| *b == b'\t')
            .map_or(&entry[..0], |t| &entry[t + 1..]);
        if [RATCHET, MEMORY, COSTS]
            .iter()
            .any(|f| path == f.as_bytes())
        {
            continue;
        }
        h.update(entry);
        h.update([0u8]);
        entries += 1;
    }
    if entries == 0 {
        return Err(Error::new(format!("git lists no file in {rev}'s tree")));
    }
    Ok(hex(&h.finalize()))
}

/// THE PROVER, by content: atpkg's `tree_root` — a SHA-256 over every file's
/// path, mode and content — of the toolchain directory whose `bin/` holds the
/// driver. For a store build it is the digest the signed index names; for any
/// other toolchain it is the same fold over what is on disk. A directory
/// without `targo` AND `trustc` in its `bin/` is no toolchain the lane or the
/// cut runs, and is not identified.
///
/// # Errors
/// Why the directory names no prover: no `trustc`/`targo` beside each other,
/// or a walk that failed.
pub fn prover_identity(bin: &Path) -> std::result::Result<(String, PathBuf), String> {
    for tool in ["targo", "trustc"] {
        if !bin.join(tool).is_file() {
            return Err(format!(
                "{} holds no {tool} (a prover is a toolchain with targo and trustc side by side)",
                bin.display()
            ));
        }
    }
    let root = bin
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", bin.display()))?;
    let root = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let digest = atpkg::tree::tree_root(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    Ok((digest, root))
}

/// The key a record is filed under and the assembler opens: SHA-256 over the
/// schema line and each input on its own line.
#[must_use]
pub fn facts_key(tree_sans_floors: &str, prover: &str, lane: &str) -> String {
    let material = format!(
        "{KEY_SCHEMA}\ntree-sans-floors {tree_sans_floors}\nprover {prover}\nlane {lane}\n"
    );
    hex(&Sha256::digest(material.as_bytes()))
}

// ---------------------------------------------------------------------------
// THE LANE'S END: [`conclude`]
// ---------------------------------------------------------------------------

/// What a lane run wrote in its log directory (`.plan`, read by [`conclude`]): the
/// run's decisions and where its logs are, and — when the run is one that files — what
/// the record is filed from.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub facts: Facts,
    /// Each selected library's gate spec, by name (its log is `<logdir>/<spec>.log`).
    pub specs: BTreeMap<String, String>,
    pub logdir: PathBuf,
    pub ratchet: PathBuf,
    /// The memory record the run read (MEMORY), which `--update-ratchet` keeps
    /// current.
    pub memory: PathBuf,
    /// This host's memory observations, `None` outside a git checkout.
    pub host_obs: Option<PathBuf>,
    /// `--try-refused`: nothing is refused for memory up front.
    pub try_refused: bool,
    /// `--remeasure`: the run measured again only the libraries the stored facts
    /// of this key leave undecided, and [`conclude`] folds them into that record
    /// ([`remeasured`]).
    pub remeasure: bool,
    /// Present when the run files: why it may not (empty when it may).
    pub file: Option<FilePlan>,
}

/// The inputs of filing ([`conclude`]).
#[derive(Clone, Debug, Default)]
pub struct FilePlan {
    /// Why this run files nothing, in the lane's words; empty when it files.
    pub refused: String,
    pub store: PathBuf,
    pub commit: String,
    pub tree: String,
    /// The prover identity the lane read when it started ([`prover_identity`]).
    pub prover: String,
    pub prover_bin: PathBuf,
    /// The environment the lane built for every compile ([`LaneEnv`]).
    pub lane_env: LaneEnv,
    /// The cargo home the lane's compiles resolve (`$HOME/.cargo`: the lane
    /// passes neither `CARGO_HOME` nor `TARGO_HOME`).
    pub cargo_home: Option<PathBuf>,
    /// The slots' target root, whose dep-info must not name the ratchet. The
    /// lane empties its slots before its checks start, so every dep-info file
    /// in them was written by this run.
    pub target_root: PathBuf,
    pub host: String,
    pub load: String,
    pub jobs: String,
    pub date: String,
}

/// THE LANE'S ENVIRONMENT, as the lane declared it in its plan: every variable
/// its compiles run with. `env -i` and these, nothing else (the script's "THE
/// LANE'S ENVIRONMENT").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneEnv {
    /// A variable the lane sets to a constant it holds (`lane-pin NAME=VALUE`):
    /// the value is the script's, and the script is in the tree the key covers.
    pub pins: BTreeMap<String, String>,
    /// A variable the lane passes from the host (`lane-passed NAME`: `HOME`,
    /// `TMPDIR`, the slot's target directory …). Its value is no input the key
    /// covers, so no compile may read it.
    pub passed: BTreeSet<String>,
}

impl Plan {
    /// Parse the plan the lane writes. Unknown or malformed lines are errors:
    /// a plan this judge cannot read is a lane it does not understand.
    ///
    /// # Errors
    /// No magic line, or a line that is not one this reader knows.
    pub fn parse(text: &str) -> std::result::Result<Plan, String> {
        let mut lines = text.lines();
        if lines.next() != Some(PLAN_MAGIC) {
            return Err(format!("not a lane plan (no `{PLAN_MAGIC}` line)"));
        }
        let mut p = Plan::default();
        let mut file = FilePlan::default();
        let mut files = false;
        let (mut mem_rows, mut refused) = (Vec::new(), Vec::new());
        for line in lines {
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            let bad = || format!("plan line {line:?} is not one this judge reads");
            match k {
                "driver" => p.facts.driver = v.into(),
                "driver-id" => p.facts.driver_id = v.into(),
                "trustd" => p.facts.trustd = v.into(),
                "only" => p.facts.only = v.into(),
                "logdir" => p.logdir = v.into(),
                "ratchet" => p.ratchet = v.into(),
                "memory-file" => p.memory = v.into(),
                "host-obs" => p.host_obs = (!v.is_empty()).then(|| v.into()),
                "try-refused" => p.try_refused = v == "1",
                "remeasure" => p.remeasure = v == "1",
                "host-mem" => p.facts.host.mem = v.parse().map_err(|_| bad())?,
                "host-mem-os" => p.facts.host.memtotal = v == "MemTotal",
                "now" => p.facts.measured_at = v.into(),
                "memory" => {
                    let f: Vec<&str> = v.split('\t').collect();
                    let row = MemRow::from_fields(f.get(1..).ok_or_else(bad)?).ok_or_else(bad)?;
                    mem_rows.push((f[0].to_string(), row));
                }
                "refused" => {
                    let f: Vec<&str> = v.split('\t').collect();
                    let seen = Seen::from_fields(f.get(1..).ok_or_else(bad)?).ok_or_else(bad)?;
                    refused.push((f[0].to_string(), seen));
                }
                "vanished" => p.facts.vanished.push(v.into()),
                "ambiguous" => {
                    let [n, a, b] = v.split('\t').collect::<Vec<_>>()[..] else {
                        return Err(bad());
                    };
                    p.facts.ambiguous.push((n.into(), a.into(), b.into()));
                }
                "crate" => {
                    let [name, spec, word] = v.split('\t').collect::<Vec<_>>()[..] else {
                        return Err(bad());
                    };
                    let decision = Decision::from_word(word).ok_or_else(bad)?;
                    p.specs.insert(name.into(), spec.into());
                    p.facts.crates.push(Crate::new(name, decision));
                }
                "file" => {
                    files = true;
                    file.refused = v.into();
                }
                "store" => file.store = v.into(),
                "commit" => file.commit = v.into(),
                "tree" => file.tree = v.into(),
                "prover" => file.prover = v.into(),
                "prover-bin" => file.prover_bin = v.into(),
                "lane-pin" => {
                    let (name, value) = v.split_once('=').ok_or_else(bad)?;
                    if name.is_empty() {
                        return Err(bad());
                    }
                    file.lane_env.pins.insert(name.into(), value.into());
                }
                "lane-passed" if !v.is_empty() && !v.contains(['=', ' ']) => {
                    file.lane_env.passed.insert(v.into());
                }
                "target-root" => file.target_root = v.into(),
                "cargo-home" => file.cargo_home = (!v.is_empty()).then(|| v.into()),
                "host" => file.host = v.into(),
                "load" => file.load = v.into(),
                "jobs" => file.jobs = v.into(),
                "date" => file.date = v.into(),
                _ => return Err(bad()),
            }
        }
        if files {
            p.file = Some(file);
        }
        // What [`Plan::admit`] decided before the run: each library's memory row,
        // and the libraries this host refused up front.
        for (name, row) in mem_rows {
            if let Some(c) = p.facts.crates.iter_mut().find(|c| c.name == name) {
                c.memory = row;
            }
        }
        for (name, seen) in refused {
            match p.facts.crates.iter_mut().find(|c| c.name == name) {
                Some(c) if matches!(c.decision, Decision::Gate(_)) => {
                    c.decision = Decision::Memory(seen);
                }
                _ => return Err(format!("plan refuses `{name}`, which it does not gate")),
            }
        }
        Ok(p)
    }

    /// REFUSE UP FRONT (MEMORY, before the run): read the memory
    /// record — held to its shape — this host's observations and the ratchet,
    /// and decide which gated libraries this host cannot reproduce the floor
    /// of. Returns the plan lines that record it (the row this host reads of
    /// each selected library's, [`row_for`], and each refusal) and the
    /// refusals, `(name, its MEMORY line)`.
    ///
    /// # Errors
    /// The memory record is missing or a row does not parse, in the lane's words.
    pub fn admit(&self) -> std::result::Result<(String, Vec<(String, String)>), String> {
        let shown = self.memory.display().to_string();
        let text = fs::read_to_string(&self.memory)
            .map_err(|_| format!("memory record {shown} is missing"))?;
        let record = parse_memory(&text, &shown)?;
        let ratchet = fs::read_to_string(&self.ratchet).ok();
        let obs = self
            .host_obs
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| parse_host_obs(&t))
            .unwrap_or_default();
        let driver_row = self.facts.driver_row();
        let (mut lines, mut refused) = (String::new(), Vec::new());
        let none = MemRow::default();
        for c in &self.facts.crates {
            let row = record
                .get(&c.name)
                .and_then(|rows| row_for(rows, &self.facts.host))
                .unwrap_or(&none);
            if record.contains_key(&c.name) {
                lines.push_str(&format!("memory {}\t{}\n", one_line(&c.name), row.fields()));
            }
            if !matches!(c.decision, Decision::Gate(_)) || self.try_refused {
                continue;
            }
            let floor = floor_of(ratchet.as_deref(), &c.name)
                .map(|f| f.proved)
                .filter(|f| !f.is_empty());
            let spec = self
                .specs
                .get(&c.name)
                .map_or(c.name.as_str(), String::as_str);
            let seen = floor.as_deref().and_then(|f| {
                obs.iter()
                    .find(|o| o.spec == spec && o.floor == f && o.prover == driver_row)
                    .map(HostObs::seen)
            });
            if let Some(r) = memory_refusal(
                &self.facts.host,
                &c.name,
                floor.is_some(),
                row,
                seen.as_ref(),
            ) {
                let seen_fields = seen
                    .as_ref()
                    .map_or_else(|| ["-"; 4].join("\t"), Seen::fields);
                lines.push_str(&format!("refused {}\t{seen_fields}\n", one_line(&c.name)));
                refused.push((c.name.clone(), r.line));
            }
        }
        Ok((lines, refused))
    }

    /// Read each gated library's log, exit status and — for a check targo would
    /// not start — what the socket held (`<log>.trustd`) into its facts, and how
    /// many grants the trustd keeper took, or what it found instead.
    pub fn measure(&mut self) {
        let read = |p: PathBuf| {
            fs::read_to_string(p)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        self.facts.grants = read(self.logdir.join(".trustd-grants"));
        self.facts.trustd_seen = read(self.logdir.join(".trustd-seen"));
        for c in &mut self.facts.crates {
            if let Decision::Gate(m) = &mut c.decision {
                let spec = self
                    .specs
                    .get(&c.name)
                    .map_or(c.name.as_str(), String::as_str);
                let log = self.logdir.join(format!("{spec}.log"));
                let rc = fs::read_to_string(self.logdir.join(format!("{spec}.log.rc")))
                    .map_or_else(|_| "?".to_string(), |s| s.trim().to_string());
                *m = match fs::File::open(&log) {
                    Ok(f) => Measured::of_log(BufReader::new(f), &rc),
                    Err(_) => Measured {
                        rc,
                        ..Measured::default()
                    },
                };
                if !m.launch.is_empty() {
                    let probe = read(self.logdir.join(format!("{spec}.log.trustd")));
                    let (held, what) = probe.split_once('\t').unwrap_or((probe.as_str(), ""));
                    (m.held, m.held_what) = (one_line(held), one_line(what));
                }
                // THE BUDGET: the lane ended this check at its per-crate wall
                // budget and left the budget in `<log>.budget`. The mark's
                // presence decides; a mark that names no budget is `?`.
                let mark = self.logdir.join(format!("{spec}.log.budget"));
                if mark.exists() {
                    let said = read(mark);
                    let said = said.lines().next().unwrap_or("");
                    m.wall_budget = if !said.is_empty() && said.bytes().all(|b| b.is_ascii_digit())
                    {
                        said.to_string()
                    } else {
                        "?".into()
                    };
                }
            }
        }
    }
}

/// A variable cargo itself sets for every compile, from the package and the
/// unit (the Cargo book's "Environment variables Cargo sets for crates"): its
/// value is the manifest's, the unit's or the target directory's — never the
/// caller's.
fn cargo_sets(name: &str) -> bool {
    matches!(
        name,
        "CARGO"
            | "CARGO_CRATE_NAME"
            | "CARGO_BIN_NAME"
            | "CARGO_PRIMARY_PACKAGE"
            | "CARGO_TARGET_TMPDIR"
            | "CARGO_RUSTC_CURRENT_DIR"
            | "OUT_DIR"
    ) || ["CARGO_PKG_", "CARGO_MANIFEST_", "CARGO_BIN_EXE_"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Why a compile's read of `name` (`value` when the variable was set) is a
/// value the key does not determine, or `None` when it is one it does: set by
/// cargo, emitted by a build script (`cargo:rustc-env`, whose inputs are the
/// tree's and the lane's), or a lane pin read as pinned. A variable the lane
/// passes from the host is refused whether set or not — unset here, it is set
/// on another host. One the lane neither pins nor passes, read unset, is unset
/// for every compile the lane runs.
fn env_dep_refusal(
    name: &str,
    value: Option<&str>,
    lane: &LaneEnv,
    emitted: &BTreeSet<String>,
) -> Option<String> {
    if lane.passed.contains(name) {
        return Some(format!(
            "the lane passes {name} from the host, and no key covers its value"
        ));
    }
    let value = value?;
    if cargo_sets(name) || emitted.contains(name) {
        return None;
    }
    match lane.pins.get(name) {
        Some(pin) if pin == value => None,
        Some(_) => Some(format!(
            "{name} is one of the lane's pins, and the compile read another value"
        )),
        None => Some(format!(
            "{name} was set, and neither cargo, a build script nor the lane sets it"
        )),
    }
}

/// THE DEP-INFO CHECK. The key leaves two things to the lane, and both are
/// checked here from what the compiles themselves recorded, not assumed:
///
/// * the FLOORS: no verification reads the ratchet or the memory record. Every
///   `.d` file the slots hold (rustc's list of each unit's input files) must
///   name neither;
/// * the ENVIRONMENT: every compile ran in the lane's own ([`LaneEnv`]). Every
///   `# env-dep:` line — rustc's record of an `env!`/`option_env!` read and the
///   value it compiled in (the plan's §4.2 env-deps, such as objc2's
///   `MACOSX_DEPLOYMENT_TARGET`) — must read a value the key determines
///   ([`env_dep_refusal`]), a build script's `cargo:rustc-env` output (under
///   the slots' `build/*/output`) counting as one.
///
/// And the slots must hold at least one `.d`: no dep-info at all is no
/// evidence. The lane empties its slots before its checks start, so each one
/// there was written by this run's compiles, whatever its file system's clock
/// stamped it.
fn dep_info_check(target_root: &Path, lane: &LaneEnv) -> std::result::Result<(), String> {
    let mut stack: Vec<PathBuf> = fs::read_dir(target_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with('j') && n[1..].bytes().all(|b| b.is_ascii_digit()) && n.len() > 1
        })
        .map(|e| e.path())
        .collect();
    let (mut found, mut named) = (0usize, None::<(PathBuf, &str)>);
    let mut reads: Vec<(String, PathBuf, Option<String>)> = Vec::new();
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().is_some_and(|x| x == "d") {
                let Ok(text) = fs::read(&p) else { continue };
                for floors in [RATCHET, MEMORY] {
                    let file = floors.rsplit('/').next().unwrap_or(floors).as_bytes();
                    if text.windows(file.len()).any(|w| w == file) {
                        named = Some((p.clone(), floors));
                    }
                }
                for line in text.split(|b| *b == b'\n') {
                    let Some(read) = line.strip_prefix(b"# env-dep:".as_slice()) else {
                        continue;
                    };
                    let read = String::from_utf8_lossy(read);
                    let read = read.trim_end_matches('\r');
                    let (name, value) = match read.split_once('=') {
                        Some((n, v)) => (n.to_string(), Some(v.to_string())),
                        None => (read.to_string(), None),
                    };
                    reads.push((name, p.clone(), value));
                }
                found += 1;
            } else if ft.is_file()
                && e.file_name() == "output"
                && p.parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "build")
            {
                let Ok(text) = fs::read_to_string(&p) else {
                    continue;
                };
                for line in text.lines() {
                    let Some(set) = line
                        .strip_prefix("cargo::rustc-env=")
                        .or_else(|| line.strip_prefix("cargo:rustc-env="))
                    else {
                        continue;
                    };
                    if let Some((name, _)) = set.split_once('=') {
                        emitted.insert(name.to_string());
                    }
                }
            }
        }
    }
    if let Some((p, floors)) = named {
        return Err(format!(
            "{} names {floors}: a compile read a floors file, so it is an input and a record keyed without it would be carried to other floors",
            p.display()
        ));
    }
    if found == 0 {
        return Err(format!(
            "no dep-info under {} was written by this run, so nothing shows which files its compiles read",
            target_root.display()
        ));
    }
    reads.sort();
    let refused: Vec<(String, PathBuf, String)> = reads
        .into_iter()
        .filter_map(|(name, file, value)| {
            env_dep_refusal(&name, value.as_deref(), lane, &emitted).map(|why| (name, file, why))
        })
        .collect();
    if let Some((name, file, why)) = refused.first() {
        let more = refused
            .iter()
            .map(|(n, _, _)| n)
            .collect::<BTreeSet<_>>()
            .len()
            - 1;
        let others = if more > 0 {
            format!(" (and {more} other variable(s) the same way)")
        } else {
            String::new()
        };
        return Err(format!(
            "{} records a compile-time read of {name}: {why}{others} — a record keyed without that value would be carried to builds that read another",
            file.display()
        ));
    }
    Ok(())
}

/// Cargo config files outside the tree that a build in `root` reads — each
/// ancestor's `.cargo/config[.toml]` and `cargo_home`'s (the home the lane's
/// compiles resolve, `$HOME/.cargo`: the lane passes neither `CARGO_HOME` nor
/// its `TARGO_HOME` twin, so no other home is read). Their content is no input
/// the key covers, so a run that read one files nothing (aterm-verify
/// `build_config` records them for the gate's receipts; the lane needs only to
/// know there are none).
pub(crate) fn outside_config(root: &Path, cargo_home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let names = ["config", "config.toml"];
    let mut at = root.parent();
    while let Some(dir) = at {
        for n in names {
            let p = dir.join(".cargo").join(n);
            if p.is_file() {
                out.push(p);
            }
        }
        at = dir.parent();
    }
    if let Some(home) = cargo_home {
        for n in names {
            let p = home.join(n);
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// What the lane's end decided, and what it has to say before the verdict.
#[derive(Debug)]
pub struct Concluded {
    pub verdict: Verdict,
    /// The FACTS line: where the record was filed, or why none was.
    pub facts_line: Option<String>,
    /// Notes ahead of the report: a record not written.
    pub said: Vec<String>,
}

/// MEASURED AGAIN, ON THE SAME KEY. `base` is the record a
/// whole-tree run filed; `again` is what a later `--remeasure` run measured of
/// some of its libraries — the ones `base` left undecided — under the SAME key
/// (the tree less its floors, the prover, the lane) on the same machine. The
/// answer is `base` with each of those libraries' facts replaced by the new
/// ones, and the new run named in its provenance: facts of one key, measured
/// twice, never facts of two keys. Refused, naming why: another key, another
/// host, another driver, or a library `base` does not select.
///
/// # Errors
/// Why the two cannot be one record.
pub fn remeasured(base: &Facts, again: &Facts) -> std::result::Result<Facts, String> {
    let (Some(b), Some(a)) = (&base.provenance, &again.provenance) else {
        return Err("a record without its key cannot be completed".into());
    };
    if a.key != b.key
        || a.tree_sans_floors != b.tree_sans_floors
        || a.prover != b.prover
        || a.lane != b.lane
    {
        return Err(format!(
            "the stored record is key {}, and this run measured key {}: facts of two keys are never one record",
            short(&b.key),
            short(&a.key)
        ));
    }
    if again.host != base.host {
        return Err(format!(
            "the stored record was measured on a host with {} bytes of memory, and this run on one with {}",
            base.host.mem, again.host.mem
        ));
    }
    if again.driver_id != base.driver_id {
        return Err(format!(
            "the stored record's driver says `{}`, and this run's `{}`",
            base.driver_id, again.driver_id
        ));
    }
    let mut out = base.clone();
    let mut names = Vec::new();
    for c in &again.crates {
        let Some(slot) = out.crates.iter_mut().find(|o| o.name == c.name) else {
            return Err(format!(
                "the stored record selects no library named {}",
                c.name
            ));
        };
        *slot = c.clone();
        names.push(c.name.clone());
    }
    if let Some(p) = out.provenance.as_mut() {
        p.remeasured.push(format!(
            "{} {} {}",
            a.date,
            a.head,
            if names.is_empty() {
                "nothing".to_string()
            } else {
                names.join(" ")
            }
        ));
    }
    Ok(out)
}

/// Where the facts of a run that files go, or why they do not: every input the
/// key does not cover checked ([`conclude`]). A key git cannot compute files
/// nothing, and decides nothing about the verdict.
fn filing(
    git: &dyn GitRunner,
    repo: &Path,
    plan: &Plan,
    file: &FilePlan,
) -> std::result::Result<Provenance, String> {
    if !file.refused.is_empty() {
        return Err(file.refused.clone());
    }
    // A PATH TARGO REFUSED is the host's (the module docs): a record keyed by the
    // code would carry it as the code's, so nothing this run measured is filed.
    let (mut paths, mut names) = (BTreeSet::new(), Vec::new());
    for c in &plan.facts.crates {
        if let Decision::Gate(m) = &c.decision
            && !m.path_refused.is_empty()
        {
            paths.extend(m.path_refused.iter().map(|r| r.path.as_str()));
            names.push(c.name.as_str());
        }
    }
    if !names.is_empty() {
        return Err(format!(
            "targo refused a path on this host ({}) for {} (REFUSED PATH above), which says nothing about the code",
            paths.into_iter().collect::<Vec<_>>().join(", "),
            names.join(" ")
        ));
    }
    if file.prover.is_empty() {
        return Err("the prover could not be identified when the run started".into());
    }
    if file.lane_env.passed.is_empty() {
        return Err(
            "the lane's plan declares no environment of its own, so nothing shows its compiles ran in one"
                .into(),
        );
    }
    if let Some(c) = outside_config(repo, file.cargo_home.as_deref()).first() {
        return Err(format!(
            "cargo read {}, a config file outside the tree the key covers",
            c.display()
        ));
    }
    let (prover, root) = match prover_identity(&file.prover_bin) {
        Err(why) => return Err(format!("the prover cannot be identified now ({why})")),
        Ok((now, _)) if now != file.prover => {
            return Err(format!(
                "the prover changed during the run ({} -> {})",
                short(&file.prover),
                short(&now)
            ));
        }
        Ok(found) => found,
    };
    // A `--remeasure` run that gated nothing (nothing was left undecided, or
    // this host refused it) compiled nothing to vouch for: the record it
    // completes was checked when it was filed.
    let gated = plan
        .facts
        .crates
        .iter()
        .any(|c| matches!(c.decision, Decision::Gate(_)));
    if (gated || !plan.remeasure)
        && let Err(why) = dep_info_check(&file.target_root, &file.lane_env)
    {
        return Err(why);
    }
    let tsr = tree_sans_floors(git, &file.commit)
        .map_err(|e| format!("git could not list the tree the key covers ({e})"))?;
    Ok(Provenance {
        key: facts_key(&tsr, &prover, LANE_WHOLE_TREE),
        tree_sans_floors: tsr,
        prover,
        lane: LANE_WHOLE_TREE.into(),
        prover_root: root.display().to_string(),
        trustc: trustc_commit(&file.prover_bin),
        head: file.commit.clone(),
        tree: file.tree.clone(),
        host: file.host.clone(),
        load: file.load.clone(),
        jobs: file.jobs.clone(),
        date: file.date.clone(),
        remeasured: Vec::new(),
    })
}

/// THE LANE'S END: measure the logs, judge against the ratchet, write the
/// ratchet when asked, and — for a run that files — file the record under its
/// key. A record is filed only when every input the key does not cover meets
/// the lane's filing requirements: its own checks (a whole-tree run, a clean
/// checkout that stayed clean but for the floors) arrive as `refused`; this
/// adds the prover (unchanged since the run started), the lane's declared
/// environment, the cargo config files (the tree's own only) and the dep-info
/// check (no compile read a floors file, or a value the key does not determine).
///
/// A `--remeasure` run ([`Plan::remeasure`]) measured only the libraries the
/// stored record of its key left undecided: its facts are folded into that
/// record ([`remeasured`]), and the verdict printed and filed is the whole
/// record's. One that may not file, or finds no record to complete, judges
/// what it measured alone, as a narrowed run.
///
/// A record git cannot key (it could not list the tree) is not filed, and the
/// verdict stands: the judge's answer never needed the key.
///
/// # Errors
/// The ratchet could not be written.
pub fn conclude(
    git: &dyn GitRunner,
    repo: &Path,
    plan: &mut Plan,
    update: Update<'_>,
) -> Result<Concluded> {
    plan.measure();
    let filed = plan.file.as_ref().map(|file| filing(git, repo, plan, file));
    // What is judged and filed: this run's facts, or — measured again — the
    // stored record they complete.
    let mut facts = plan.facts.clone();
    let mut why_not: Option<String> = None;
    if let Some(Ok(prov)) = &filed {
        facts.provenance = Some(prov.clone());
        if plan.remeasure {
            let store = plan
                .file
                .as_ref()
                .map(|f| f.store.clone())
                .unwrap_or_default();
            let stored = store.join(format!("{FACTS_PREFIX}{}", prov.key));
            let base = fs::read_to_string(&stored)
                .ok()
                .and_then(|t| Facts::parse(&t));
            match base.map(|b| remeasured(&b, &facts)) {
                Some(Ok(done)) => facts = done,
                Some(Err(why)) => why_not = Some(why),
                None => {
                    why_not = Some(format!(
                        "--remeasure found no whole record under this run's key ({}) to complete",
                        short(&prov.key)
                    ));
                }
            }
            if why_not.is_some() {
                facts.provenance = None;
            }
        }
    }
    let ratchet_text = fs::read_to_string(&plan.ratchet).ok();
    let mut verdict = judge(
        &facts,
        ratchet_text.as_deref(),
        &plan.ratchet.display().to_string(),
        &plan.memory.display().to_string(),
        update,
    );
    // A run measured again writes back only what IT measured: the memory rows
    // and this host's observations of the libraries it ran.
    if plan.remeasure {
        let ran: BTreeSet<&str> = plan.facts.crates.iter().map(|c| c.name.as_str()).collect();
        verdict
            .memory_rows
            .retain(|(n, _)| ran.contains(n.as_str()));
        verdict.host_obs.retain(|(n, _)| ran.contains(n.as_str()));
    }
    if let Some(text) = &verdict.ratchet {
        fs::write(&plan.ratchet, text).map_err(|e| {
            Error::new(format!(
                "could not write the ratchet {}: {e}",
                plan.ratchet.display()
            ))
        })?;
    }
    let mut said: Vec<String> = Vec::new();
    // KEEP THE MEMORY RECORD CURRENT (--update-ratchet): a write that fails is
    // said, and the next --update-ratchet run writes it.
    if !verdict.memory_rows.is_empty() {
        let written = fs::read_to_string(&plan.memory)
            .map(|t| memory_rewritten(&t, &verdict.memory_rows))
            .and_then(|t| fs::write(&plan.memory, t));
        if written.is_err() {
            for (name, _) in &verdict.memory_rows {
                said.push(format!(
                    "trust: {} NOT updated for {name} — the next --update-ratchet run writes it",
                    plan.memory.display()
                ));
            }
        }
    }
    // WHAT THIS HOST LEARNED goes back in one write: it only ever decides what
    // the next run here refuses, so a write that fails is said and decides
    // nothing.
    if let Some(path) = &plan.host_obs
        && let Err(e) = write_host_obs(path, plan, &verdict)
    {
        said.push(format!(
            "trust: host memory observations NOT written ({}: {e}) — the next run refuses only by what it already holds; nothing was decided by it",
            path.display()
        ));
    }
    let none = |why: &str| {
        Some(format!(
            "FACTS: none — {why}; filing requires a whole-tree run on a clean checkout"
        ))
    };
    let facts_line = match (&filed, &plan.file) {
        (None, _) | (_, None) => None,
        (Some(Err(why)), _) => none(why),
        (Some(Ok(_)), _) if why_not.is_some() => none(why_not.as_deref().unwrap_or_default()),
        (Some(Ok(prov)), Some(file)) => match file_record(&file.store, &facts) {
            Ok(path) => Some(format!(
                "FACTS: {} (key {}, prover {}, trustc {}{}) — available for explicit verdicts against a matching commit's floors",
                path.display(),
                short(&prov.key),
                short(&prov.prover),
                short(&prov.trustc),
                match (plan.remeasure, plan.facts.crates.is_empty()) {
                    (false, _) => String::new(),
                    (true, true) => "; nothing was left undecided".to_string(),
                    (true, false) => format!(
                        "; measured again: {}",
                        plan.facts
                            .crates
                            .iter()
                            .map(|c| c.name.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                }
            )),
            Err(e) => Some(format!("FACTS: NOT FILED — {e}")),
        },
    };
    Ok(Concluded {
        verdict,
        facts_line,
        said,
    })
}

/// Rewrite this host's memory observations with what the run learned (the
/// lane's host observations): every row but those of the libraries it checked,
/// and the rows it set, sorted, under the header. Nothing to change writes
/// nothing.
fn write_host_obs(path: &Path, plan: &Plan, verdict: &Verdict) -> std::io::Result<()> {
    if verdict.host_obs.is_empty() {
        return Ok(());
    }
    let spec = |name: &str| {
        plan.specs
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    };
    let gone: BTreeSet<String> = verdict.host_obs.iter().map(|(n, _)| spec(n)).collect();
    let set: Vec<String> = verdict
        .host_obs
        .iter()
        .filter_map(|(n, row)| {
            let [outcome, ceil, peak, proved, floor] = row.as_ref()?;
            Some(format!(
                "{}\t{outcome}\t{ceil}\t{peak}\t{proved}\t{floor}\t{}\t{}",
                spec(n),
                plan.facts.driver_row(),
                plan.facts.measured_at
            ))
        })
        .collect();
    let old = fs::read_to_string(path).ok();
    if set.is_empty() && old.is_none() {
        return Ok(());
    }
    let mut rows: Vec<String> = old
        .as_deref()
        .unwrap_or("")
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| !gone.contains(l.split('\t').next().unwrap_or("")))
        .map(str::to_string)
        .chain(set)
        .collect();
    rows.sort();
    let mut text = format!("{HOST_OBS_HEADER}\n");
    for r in rows {
        text.push_str(&r);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tsv.{}", std::process::id()));
    fs::write(&tmp, text)
        .and_then(|()| fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
}

/// File `facts` in `store` under the key its provenance names: written to a
/// temporary name and renamed into place, so a reader sees a whole record or
/// none. A record already there under that key is replaced — the same inputs,
/// measured again (the counts of this seal still move with load; P5 of the
/// plan makes them a function of their inputs, and a second measurement is
/// then a check of the first).
///
/// # Errors
/// A record with no key, or one that could not be written.
pub fn file_record(store: &Path, facts: &Facts) -> std::result::Result<PathBuf, String> {
    let key = facts
        .provenance
        .as_ref()
        .map(|p| p.key.clone())
        .filter(|k| !k.is_empty())
        .ok_or("a record with no key cannot be filed")?;
    let path = store.join(format!("{FACTS_PREFIX}{key}"));
    let tmp = store.join(format!(".{FACTS_PREFIX}{key}.{}", std::process::id()));
    fs::create_dir_all(store)
        .and_then(|()| fs::write(&tmp, facts.to_record()))
        .and_then(|()| fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("could not write {}: {e}", path.display())
        })?;
    Ok(path)
}

/// The `commit-hash:` a toolchain's `trustc -vV` answers, `unknown` when it
/// names none.
#[must_use]
pub fn trustc_commit(bin: &Path) -> String {
    std::process::Command::new(bin.join("trustc"))
        .arg("-vV")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find_map(|l| {
                    l.trim()
                        .strip_prefix("commit-hash:")
                        .map(str::trim)
                        .map(str::to_string)
                })
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// ASSEMBLY: the verdict for a commit, from its facts and its own ratchet
// ---------------------------------------------------------------------------

/// The toolchain requested for a stored-facts verdict.
#[derive(Clone, Debug)]
pub struct Prover {
    /// Its `bin/`; the default comes from [`crate::gates::cut_toolchain`].
    pub bin: PathBuf,
    /// Its identity ([`prover_identity`]), or why it has none.
    pub identity: std::result::Result<String, String>,
    /// What its trustc says it is — shown, never compared: the identity is.
    pub trustc: String,
}

impl Prover {
    /// Identify the toolchain whose `bin/` is `bin`.
    #[must_use]
    pub fn at(bin: &Path) -> Prover {
        Prover {
            bin: bin.to_path_buf(),
            identity: prover_identity(bin).map(|(id, _)| id),
            trustc: trustc_commit(bin),
        }
    }

    /// The prover a [`crate::gates::cut_toolchain`] identity names
    /// (`<bin dir> trustc <commit>`).
    #[must_use]
    pub fn of_toolchain(toolchain: &str) -> Prover {
        let bin = toolchain
            .rsplit_once(" trustc ")
            .map_or(toolchain, |(dir, _)| dir);
        Prover::at(Path::new(bin))
    }

    /// `<identity, short>` for a line, or `unidentified`.
    fn shown(&self) -> String {
        match &self.identity {
            Ok(id) => format!("{} (trustc {})", short(id), short(&self.trustc)),
            Err(_) => format!("unidentified (trustc {})", short(&self.trustc)),
        }
    }
}

/// The ratchet `commit` holds, `None` when it holds none.
///
/// # Errors
/// git cannot read the commit.
pub fn ratchet_at(git: &dyn GitRunner, commit: &str) -> Result<Option<String>> {
    let listed = git_ok(git, &["ls-tree", commit, "--", RATCHET])?.stdout_utf8();
    if listed.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(
        git_ok(git, &["cat-file", "blob", &format!("{commit}:{RATCHET}")])?.stdout_utf8(),
    ))
}

/// Every floor `judged` (a ratchet's text, `None` for none) lowers against the
/// floors of the commit `facts` were measured on ([`floors_lowered`]), with the
/// facts' own prover and the libraries they verified.
///
/// # Errors
/// Why the measured commit's floors cannot be read, in a few words.
pub fn lowered_since_measured(
    git: &dyn GitRunner,
    facts: &Facts,
    judged: Option<&str>,
) -> std::result::Result<Vec<String>, String> {
    let head = facts
        .provenance
        .as_ref()
        .map(|p| p.head.as_str())
        .filter(|h| !h.is_empty())
        .ok_or("the facts name no commit they were measured on")?;
    let measured = ratchet_at(git, head)
        .map_err(|e| format!("git cannot read {RATCHET} of {} ({e})", short(head)))?;
    let gated: BTreeSet<&str> = facts
        .crates
        .iter()
        .filter(|c| matches!(c.decision, Decision::Gate(_)))
        .map(|c| c.name.as_str())
        .collect();
    let driver = facts.driver_row();
    let driver = (!facts.driver_id.is_empty()).then_some(driver.as_str());
    Ok(floors_lowered(
        measured.as_deref().unwrap_or(""),
        judged.unwrap_or(""),
        &|name| gated.contains(name),
        driver,
    ))
}

/// The records in `receipts` about `tsr` (the tree less its floors), from
/// any prover: `(file name, facts)`, for naming what the store
/// holds when the requested facts key is absent.
fn records_about(receipts: &Path, tsr: &str) -> Vec<(String, Facts)> {
    let mut out: Vec<(String, Facts)> = fs::read_dir(receipts)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(FACTS_PREFIX).then_some(())?;
            let facts = Facts::parse(&fs::read_to_string(e.path()).ok()?)?;
            (facts.provenance.as_ref()?.tree_sans_floors == tsr).then_some((name, facts))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The facts record for `commit` from `prover` — `(facts, key)` — or why
/// there is none, in a few words. Only the record filed under the key
/// `commit` itself computes is opened, and one whose own lines name another
/// key is refused.
///
/// # Errors
/// git cannot list `commit`'s tree.
pub fn facts_for(
    git: &dyn GitRunner,
    receipts: &Path,
    prover: &Prover,
    commit: &str,
) -> Result<std::result::Result<(Facts, String), String>> {
    let id = match &prover.identity {
        Ok(id) => id.clone(),
        Err(why) => {
            return Ok(Err(format!(
                "the requested prover ({}) cannot be identified: {why}",
                prover.bin.display()
            )));
        }
    };
    let tsr = tree_sans_floors(git, commit)?;
    let key = facts_key(&tsr, &id, LANE_WHOLE_TREE);
    let name = format!("{FACTS_PREFIX}{key}");
    let shown = format!("{FACTS_PREFIX}{}", short(&key));
    let Ok(text) = fs::read_to_string(receipts.join(&name)) else {
        let others = records_about(receipts, &tsr);
        let mut why = format!(
            "no whole-tree Trust facts for its tree (every file but {RATCHET} and {MEMORY}) from the requested prover ({}, identity {}) (`{shown}`)",
            prover.bin.display(),
            short(&id)
        );
        for (n, f) in &others {
            let p = f.provenance.clone().unwrap_or_default();
            why.push_str(&format!(
                "; `{}` holds facts for this tree from prover {} (trustc {}, at {})",
                short_name(n),
                short(&p.prover),
                short(&p.trustc),
                p.prover_root
            ));
        }
        return Ok(Err(why));
    };
    let Some(facts) = Facts::parse(&text) else {
        return Ok(Err(format!("`{shown}` is not a complete facts record")));
    };
    let Some(p) = facts.provenance.clone() else {
        return Ok(Err(format!(
            "`{shown}` names no key: it is not a filed facts record"
        )));
    };
    if p.key != key || p.tree_sans_floors != tsr || p.prover != id || p.lane != LANE_WHOLE_TREE {
        return Ok(Err(format!(
            "`{shown}` is not the record its name says (its own lines name key {}, tree {}, prover {}, lane {})",
            short(&p.key),
            short(&p.tree_sans_floors),
            short(&p.prover),
            p.lane
        )));
    }
    Ok(Ok((facts, key)))
}

/// `trust-facts-<9 hex>` for a record's file name.
fn short_name(name: &str) -> String {
    name.strip_prefix(FACTS_PREFIX).map_or_else(
        || name.to_string(),
        |k| format!("{FACTS_PREFIX}{}", short(k)),
    )
}

// ---------------------------------------------------------------------------
// `targo --unverified ship trust <verb>`
// ---------------------------------------------------------------------------

/// A `trust` verb ([`parse_cmd`], run by [`run_cmd`]).
#[derive(Debug, PartialEq, Eq)]
pub enum TrustCmd {
    /// `run (-p <crate>… | --whole-tree | --remeasure) [flags]`: the lane
    /// ([`crate::trust_lane`]).
    Run(crate::trust_lane::Config),
    /// `judge [--commit REV] [--prover BIN] [--update-ratchet [--reason WHY]]`:
    /// the whole report for REV's facts against the working ratchet; with
    /// `--reason`, a floor another prover recorded that the facts fall below is
    /// lowered onto their prover, the reason in its fifth column.
    Judge {
        commit: String,
        prover: Option<PathBuf>,
        update: bool,
        reason: Option<String>,
    },
}

/// Parse `trust <verb> …`.
///
/// # Errors
/// A verb or flag this does not know, or one given twice.
pub fn parse_cmd<'a>(
    it: &mut impl Iterator<Item = &'a str>,
) -> std::result::Result<TrustCmd, String> {
    let verb = it.next().ok_or("trust needs a verb: run or judge")?;
    if verb == "run" {
        return crate::trust_lane::parse(it).map(TrustCmd::Run);
    }
    if verb != "judge" {
        return Err(format!("unknown trust verb {verb:?}"));
    }
    let mut flags: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    while let Some(flag) = it.next() {
        let value = if matches!(flag, "--commit" | "--prover" | "--reason") {
            let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
            if v.is_empty() || v.starts_with('-') {
                return Err(format!("{flag} needs a value, got {v:?}"));
            }
            Some(v)
        } else if flag == "--update-ratchet" {
            None
        } else {
            return Err(format!("trust {verb}: unknown flag {flag:?}"));
        };
        if flags.insert(flag, value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let value = |f: &str| flags.get(f).copied().flatten();
    let commit = value("--commit").unwrap_or("HEAD").to_string();
    let update = flags.contains_key("--update-ratchet");
    let reason = value("--reason").map(str::to_string);
    if reason.is_some() && !update {
        return Err(
            "trust judge: --reason says why --update-ratchet lowers a floor; give both".into(),
        );
    }
    Ok(TrustCmd::Judge {
        commit,
        prover: value("--prover").map(PathBuf::from),
        update,
        reason,
    })
}

/// The prover a verdict is asked for: `--prover`, else the pinned release
/// toolchain.
fn asked_prover(prover: Option<PathBuf>) -> std::result::Result<Prover, String> {
    match prover {
        Some(bin) => Ok(Prover::at(&bin)),
        None => crate::gates::cut_toolchain()
            .map(|tc| Prover::of_toolchain(&tc))
            .map_err(|e| format!("cannot name the pinned release toolchain: {e}")),
    }
}

/// What `rev`'s stored facts from `prover` leave UNDECIDED — the libraries
/// `ship trust run --remeasure` measures again — or `Err(why)` when there
/// are no facts to complete.
///
/// # Errors
/// The commit, the store or the commit's floors cannot be read.
pub fn pending_for(
    git: &dyn GitRunner,
    prover: &Prover,
    rev: &str,
) -> Result<std::result::Result<Vec<String>, String>> {
    let store = crate::gates::receipt_store(git)?;
    let head = rev_parse(git, &format!("{rev}^{{commit}}"))?;
    let (facts, _) = match facts_for(git, &store, prover, &head)? {
        Ok(found) => found,
        Err(why) => return Ok(Err(why)),
    };
    let ratchet = ratchet_at(git, &head)?;
    let verdict = judge(&facts, ratchet.as_deref(), RATCHET, MEMORY, Update::NONE);
    Ok(Ok(verdict.unverified))
}

/// Run a `trust` verb in the repository at `repo`; the exit status it answers:
/// 0 pass, 1 fail (or not verified), 2 could not run.
pub fn run_cmd(repo: &Path, cmd: TrustCmd) -> i32 {
    let git = crate::ledger::GitCli::new(repo);
    match cmd {
        TrustCmd::Run(mut cfg) => {
            cfg.repo = repo.to_path_buf();
            crate::trust_lane::run(&cfg, &mut std::io::stdout()).exit()
        }
        TrustCmd::Judge {
            commit,
            prover,
            update,
            reason,
        } => {
            let prover = match asked_prover(prover) {
                Ok(p) => p,
                Err(why) => {
                    eprintln!("aterm-release trust judge: {why}");
                    return 2;
                }
            };
            let found = crate::gates::receipt_store(&git).and_then(|store| {
                let head = rev_parse(&git, &format!("{commit}^{{commit}}"))?;
                facts_for(&git, &store, &prover, &head)
            });
            let (facts, key) = match found {
                Ok(Ok(found)) => found,
                Ok(Err(why)) => {
                    println!("no facts to judge: {why}");
                    return 1;
                }
                Err(e) => {
                    eprintln!("aterm-release trust judge: {e}");
                    return 2;
                }
            };
            let path = repo.join(RATCHET);
            let text = fs::read_to_string(&path).ok();
            let verdict = judge(
                &facts,
                text.as_deref(),
                RATCHET,
                MEMORY,
                Update {
                    on: update,
                    reason: reason.as_deref(),
                },
            );
            // The working floors — as written, when this run writes them — held
            // to the up-only rule against the floors the facts were measured on,
            // as the cut's verdict holds a commit that reuses them.
            let lowered = match lowered_since_measured(
                &git,
                &facts,
                verdict.ratchet.as_deref().or(text.as_deref()),
            ) {
                Ok(l) => l,
                Err(why) => {
                    eprintln!("aterm-release trust judge: {why}");
                    return 2;
                }
            };
            print!("{}", verdict.report);
            for l in &lowered {
                println!("LOWERED: {l}");
            }
            let p = facts.provenance.unwrap_or_default();
            println!(
                "FACTS: {FACTS_PREFIX}{} (measured on {}, prover {}), judged against the working {RATCHET}",
                short(&key),
                short(&p.head),
                prover.shown()
            );
            if let Some(new) = &verdict.ratchet {
                if let Err(e) = fs::write(&path, new) {
                    eprintln!(
                        "aterm-release trust judge: could not write {}: {e}",
                        path.display()
                    );
                    return 2;
                }
                println!("RATCHET: {RATCHET} rewritten from the stored facts");
            }
            if !verdict.memory_rows.is_empty() {
                let mem = repo.join(MEMORY);
                let written = fs::read_to_string(&mem)
                    .map(|t| memory_rewritten(&t, &verdict.memory_rows))
                    .and_then(|t| fs::write(&mem, t));
                if let Err(e) = written {
                    eprintln!(
                        "aterm-release trust judge: could not write {}: {e}",
                        mem.display()
                    );
                    return 2;
                }
                println!("MEMORY RECORD: {MEMORY} kept current from the stored facts");
            }
            if !lowered.is_empty() {
                println!(
                    "VERDICT: FAIL — the working floors lower {} floor(s) the ratchet's up-only rule refuses (LOWERED above), against those of {}, which the facts were measured on",
                    lowered.len(),
                    short(&p.head)
                );
                return 1;
            }
            if !verdict.tail.is_empty() {
                println!("{}", verdict.tail);
            }
            verdict.exit
        }
    }
}

/// Fixtures for stored-facts tests: a prover directory and a filed record,
/// as the lane would leave them.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// `<root>/prover-<tag>/bin` holding a `targo` whose content is `tag` and a
    /// `trustc` naming `commit` — a toolchain [`prover_identity`] identifies.
    pub(crate) fn prover_bin(root: &Path, tag: &str, commit: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join(format!("prover-{tag}/bin"));
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("targo"), format!("#!/bin/sh\n# {tag}\nexit 0\n")).unwrap();
        fs::write(
            bin.join("trustc"),
            format!("#!/bin/sh\necho 'commit-hash: {commit}'\n"),
        )
        .unwrap();
        for tool in ["targo", "trustc"] {
            fs::set_permissions(bin.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    /// [`prover_bin`]'s prover as [`Prover::at`] would name it, without running
    /// the stand-in `trustc`: the first exec of a script written a moment ago is
    /// the slow part of `Prover::at`, and the key reads only the identity (a digest
    /// of the files) — `trustc` is shown, never compared.
    /// `another_prover_is_another_key` runs `Prover::at` and holds it to this.
    pub(crate) fn prover(root: &Path, tag: &str, commit: &str) -> Prover {
        let bin = prover_bin(root, tag, commit);
        Prover {
            identity: prover_identity(&bin).map(|(id, _)| id),
            trustc: commit.to_string(),
            bin,
        }
    }

    /// File, for `commit` and `prover`, a whole-tree run's facts over `crates`
    /// (driver `driver_id`), under the key that run computes.
    pub(crate) fn file(
        git: &dyn GitRunner,
        store: &Path,
        prover: &Prover,
        commit: &str,
        driver_id: &str,
        crates: Vec<Crate>,
    ) -> PathBuf {
        let tsr = tree_sans_floors(git, commit).unwrap();
        let id = prover.identity.clone().unwrap();
        let facts = Facts {
            driver: prover.bin.join("targo").display().to_string(),
            driver_id: driver_id.into(),
            trustd: "/tmp/t.sock".into(),
            grants: "1".into(),
            crates,
            provenance: Some(Provenance {
                key: facts_key(&tsr, &id, LANE_WHOLE_TREE),
                tree_sans_floors: tsr,
                prover: id,
                lane: LANE_WHOLE_TREE.into(),
                prover_root: prover.bin.parent().unwrap().display().to_string(),
                trustc: prover.trustc.clone(),
                head: commit.into(),
                ..Provenance::default()
            }),
            ..Facts::default()
        };
        file_record(store, &facts).unwrap()
    }

    /// One gated library whose check printed `summary`.
    pub(crate) fn gated(name: &str, summary: &str) -> Crate {
        Crate::new(
            name,
            Decision::Gate(Measured {
                rc: "0".into(),
                summary: summary.into(),
                ..Measured::default()
            }),
        )
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn log(text: &str) -> Measured {
        Measured::of_log(text.as_bytes(), "0")
    }

    #[test]
    fn a_summary_line_parses_as_the_shell_read_it() {
        assert_eq!(
            parse_summary("  Summary: 3 proved, 0 failed, 0 assumed, 0 inconclusive (7 total)"),
            (Some(3), Some(7))
        );
        // the rightmost of each, as a greedy `.*` takes it
        assert_eq!(
            parse_summary("Summary: 1 proved (2 total) Summary: 5 proved (9 total)"),
            (Some(5), Some(9))
        );
        assert_eq!(
            parse_summary("  Summary: three properties proved, nothing outstanding"),
            (None, None)
        );
        assert_eq!(
            parse_summary("Summary: 4 proved, no total"),
            (Some(4), None)
        );
    }

    #[test]
    fn a_log_is_measured_as_the_shell_measured_it() {
        let m = log(concat!(
            "noise timed_out=3 and timed_out=4\n",
            "  [UNKNOWN] [unknown] unsupported MIR `MemoryAdmission`: memory admission failed before solver dispatch: configured trustd admission failed: could not connect to selected endpoint\n",
            "    [unsupported_mir] unsupported MIR `MemoryAdmission`: memory admission failed before solver dispatch: could not connect\n",
            "note: trust: memory census: 397 bodies verified and compacted, 2 compacted slots recomputed, 3 bodies skipped at the memory ceiling; resident 1.2 GiB, peak 3.5 GiB, ceiling 3.0 GiB\n",
            "  [UNKNOWN] [unknown] malformed: type lowering exceeded the per-function verification budget\n",
            "    [unsupported_mir] detail: type lowering exceeded the per-function verification budget\n",
            " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=proof validation cancelled by the caller (interrupt, deadline, or memory limit), checked_steps=6, skipped_hole_steps=0, total_steps=6\n",
            " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=step t9 has invalid resolution derivation, checked_steps=3, skipped_hole_steps=0, total_steps=9\n",
            "  Summary: 2 proved, 0 failed (3 total)\n",
        ));
        assert_eq!(
            (m.lapsed, m.cancelled),
            (7, 1),
            "seven timed out; one proof check cancelled, an invalid derivation not"
        );
        assert_eq!(m.refused, 1, "the [UNKNOWN] listing, not its detail line");
        assert_eq!(m.refused_why, "trustd unreachable: 1");
        assert_eq!(m.skipped, Some(3));
        assert_eq!(m.ceiling, "3.0 GiB");
        assert_eq!(m.budget_rows, 1, "result rows only, never the detail line");
        assert!(!m.transport);
        assert_eq!(parse_summary(&m.summary), (Some(2), Some(3)));
        assert_eq!(m.peak, "3.5 GiB");
        // with no census, the ceiling flag's bytes, as the log names them (the
        // judge reads them as GiB: `Census`)
        let m = log("rustc -Ztrust-verify-memory-ceiling-bytes=3221225472 x\n");
        assert_eq!(m.ceiling, "3221225472 B");
        assert_eq!(gib_of(&m.ceiling), Some(3.0));
        // A seal from trust main prints no census and has no ceiling flag; with
        // `-Ztrust-memory-notes` its census counts slots. Every memory fact is
        // then absent, never a zero, and is filed as absent.
        let m = log(concat!(
            "note: trust: memory census: 12 artifact slots compacted after their verify pass (0 after the pass had dropped its own handle), 0 compacted slots recomputed by a later pass; resident 1.1 GiB now, highest 2.3 GiB read at a body boundary\n",
            "  Summary: 2 proved, 0 failed (2 total)\n",
        ));
        assert_eq!(
            (m.skipped, m.ceiling.as_str(), m.peak.as_str()),
            (None, "", "")
        );
        let fields = m.to_fields();
        assert_eq!(fields.split('\t').nth(6), Some("-"), "{fields}");
        let back = Measured::from_fields(&fields.split('\t').collect::<Vec<_>>());
        assert_eq!(back.map(|b| b.skipped), Some(None));
        assert!(log("targo trust: error: verification did not run\n").transport);
    }

    /// The tail of a check that FINISHED, as a real one prints it (aterm-bits,
    /// 2026-09-30, targo 6606d3bdf): the Summary and targo-trust's Coverage line.
    const DONE: &str = "Summary: 5 proved, 3 failed, 8 runtime-checked, 0 assumed, 0 mandated, 0 contract-panic, 5 inconclusive (21 total)\n\
                        Coverage: 9/9 eligible function bodies verified (complete)\n\
                        \x20 Gate: FAIL [advisory lane, exit 1] (5 proved / 21 total)\n";

    /// The killed aterm-census check of 2026-09-29, in the shape its log gave it
    /// (lines 2970, 5011, 5016, 5072): cargo naming the compiler's signal,
    /// targo-trust's missing-crate-summary row, a parseable Summary of what ran,
    /// `Coverage: unknown`, and the advisory exit 101.
    const KILLED: &str = "error: could not compile `aterm-census` (lib); 216 warnings emitted\n\nCaused by:\n\
                          \x20 process didn't exit successfully: `/Users//x/toolchains/trust-6606d3bd/bin/trustc --crate-name aterm_census --edition=2024 crates/aterm-census/src/lib.rs --crate-type lib` (signal: 15, SIGTERM: termination signal)\n\
                          === Trust Verification Report ===\n\
                          Level: L2 | Solver timeout: 5000ms | Function budget: 120000ms | Duration: 7865903ms\n\
                          \x20 [UNKNOWN] [transport:missing-crate-summary] compiler emitted function rows without an end-of-compile target inventory (targo-trust)\n\
                          Summary: 207 proved, 440 failed, 252 runtime-checked, 56 assumed, 9 mandated, 0 contract-panic, 1073 inconclusive (2037 total)\n\
                          Coverage: unknown (compiler emitted no coverage_summary transport row; older toolchain?)\n\
                          \x20   [assertion] assertion: transport:missing-crate-summary UNKNOWN (targo-trust: missing crate_summary transport prevents proof that this rustc target completed)\n\
                          \x20 Gate: FAIL [advisory lane, exit 101] (207 proved / 2037 total)\n";

    /// A CHECK THAT DID NOT FINISH says so in its own record, and each sign is
    /// enough alone: every `transport:*` kind targo-trust has but `missing-json`
    /// (which is `transport`, verification that never ran), cargo's
    /// compiler-ended-by-a-signal line, the check's own exit by a signal, and a
    /// Summary without targo-trust's `(complete)` Coverage line. A finished check
    /// — whatever its exit, 101 included — says none of them.
    #[test]
    fn a_check_that_did_not_finish_says_so_in_its_own_record() {
        let ended = |text: &str, rc: &str| Measured::of_log(text.as_bytes(), rc).ended;
        // the killed census check: all three of its signs, named
        let m = Measured::of_log(KILLED.as_bytes(), "101");
        assert_eq!(
            parse_summary(&m.summary),
            (Some(207), Some(2037)),
            "its Summary still parses"
        );
        assert!(!m.transport);
        assert!(!m.ended.is_empty());
        // (control) a finished check, at every exit a finished check gives
        for rc in ["0", "1", "2", "101", "?"] {
            assert_eq!(ended(DONE, rc), "", "rc {rc}");
        }
        assert_eq!(ended("", "0"), "", "no Summary is GATE-BROKE's, not this");
        // each transport kind alone, beside a complete Coverage line
        for kind in [
            "missing-crate-summary",
            "summary-accounting",
            "crate-summary-accounting",
            "ordering",
            "unsupported-message",
            "malformed",
        ] {
            assert!(
                !ended(
                    &format!(
                        "  [UNKNOWN] [transport:{kind}] the {kind} defect (targo-trust)\n{DONE}"
                    ),
                    "1"
                )
                .is_empty(),
                "{kind}"
            );
        }
        let m = Measured::of_log(
            format!("  [UNKNOWN] [transport:missing-json] missing structured Trust JSON transport (targo-trust)\n{DONE}")
                .as_bytes(),
            "2",
        );
        assert!(
            m.transport && m.ended.is_empty(),
            "missing-json is `transport`: {m:?}"
        );
        // cargo's line alone; a compiler that exited with a status is not it
        assert!(
            !ended(
                &format!(
                    "  process didn't exit successfully: `/x/bin/trustc --crate-name a` (signal: 9, SIGKILL: kill)\n{DONE}"
                ),
                "101"
            )
            .is_empty()
        );
        assert_eq!(
            ended(
                &format!(
                    "  process didn't exit successfully: `/x/bin/trustc --crate-name a` (exit status: 1)\n{DONE}"
                ),
                "101"
            ),
            ""
        );
        // the check's own exit by a signal, after a whole report too
        for rc in ["143", "137", "158"] {
            assert!(!ended(DONE, rc).is_empty(), "rc {rc}");
        }
        assert_eq!(ended(DONE, "128"), "");
        assert_eq!(ended(DONE, "193"), "");
        // a Summary with no word that the crate was covered
        let summary = "Summary: 5 proved (21 total)\n";
        assert!(!ended(summary, "0").is_empty());
        assert!(
            !ended(&format!("{summary}Coverage: 9/12 — coverage shortfall: 3 function(s) were never verified (fail-closed: never a passing gate)\n"), "1")
                .is_empty()
        );
        assert!(
            !ended(
                &format!("{summary}Coverage: 8/9 eligible function bodies verified (complete)\n"),
                "0"
            )
            .is_empty(),
            "counts that disagree are no complete coverage"
        );
        assert!(!ended(&format!("{summary}Coverage: unknown (compiler emitted no coverage_summary transport row; older toolchain?)\n"), "0").is_empty());
        // a later targo-trust's whole test-harness compile, some bodies excluded
        assert_eq!(
            ended(
                &format!(
                    "{summary}Coverage: 12/12 eligible function bodies accounted for (10 verified, 2 excluded: test-harness) (complete)\n"
                ),
                "1"
            ),
            ""
        );
        assert!(
            !ended(
                &format!("{summary}Coverage: 11/12 eligible function bodies accounted for (9 verified, 2 excluded: test-harness) (complete)\n"),
                "1"
            )
            .is_empty(),
            "counts that disagree are no complete coverage, in either wording"
        );
    }

    /// …and it is NEVER SCORED (2026-09-29: the killed census check's partial
    /// Summary was scored, and the stage printed `ok`): a red row, exit 1, in
    /// neither total, not counted as gated, and never a floor — not a first one,
    /// not a high-water mark, not a re-base, not one lowered with a reason. The
    /// same for a check the lane ended at its per-crate wall budget, which names
    /// the crate and the budget. The control is the same Summary from a check
    /// that finished.
    #[test]
    fn a_check_that_did_not_finish_is_never_scored() {
        let check = |ended: &str, budget: &str| {
            Decision::Gate(Measured {
                rc: "101".into(),
                summary: "Summary: 207 proved (2037 total)".into(),
                ended: ended.into(),
                wall_budget: budget.into(),
                ..Measured::default()
            })
        };
        let run = |d: Decision| {
            facts(vec![
                ("alpha", d),
                ("beta", gate("Summary: 3 proved (3 total)")),
            ])
        };
        let low = format!("alpha\t200\t2037\t{DRV}\nbeta\t3\t3\t{DRV}\n");
        // (control) the same Summary, finished, holds a floor of 200
        let v = judge(&run(check("", "")), Some(&low), "R", "M", Update::NONE);
        assert_eq!((v.word, v.exit, v.total.as_str()), ("PASS", 0, "210/2040"));
        let v = judge(
            &run(check("cargo: trustc ended by signal 15 (SIGTERM)", "")),
            Some(&low),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(
            (v.word, v.exit, v.total.as_str()),
            ("FAIL", 1, "3/3"),
            "{}",
            v.report
        );
        assert_eq!(v.failed, ["alpha NOT-VERIFIED(incomplete)"]);
        let v = judge(&run(check("", "5400")), Some(&low), "R", "M", Update::NONE);
        assert_eq!((v.word, v.exit, v.total.as_str()), ("FAIL", 1, "3/3"));
        assert_eq!(v.failed, ["alpha OVER-BUDGET(5400s)"]);
        // never a floor: none recorded, none raised, none re-based or lowered
        let unfinished = [check("x", ""), check("", "60")];
        for d in &unfinished {
            for (ratchet, update) in [
                (format!("beta\t3\t3\t{DRV}\n"), Update::RAISE),
                (
                    format!("alpha\t100\t2037\t{DRV}\nbeta\t3\t3\t{DRV}\n"),
                    Update::RAISE,
                ),
                (
                    format!("alpha\t274\t2037\tanother prover\nbeta\t3\t3\t{DRV}\n"),
                    Update::RAISE,
                ),
                (
                    format!("alpha\t274\t2037\tanother prover\nbeta\t3\t3\t{DRV}\n"),
                    Update {
                        on: true,
                        reason: Some("the new prover proves less"),
                    },
                ),
            ] {
                let v = judge(&run(d.clone()), Some(&ratchet), "R", "M", update);
                assert_eq!(v.exit, 1);
                assert!(
                    v.ratchet
                        .as_deref()
                        .is_none_or(|r| !r.contains("alpha\t207")),
                    "a floor from a check that did not finish: {:?}",
                    v.ratchet
                );
                assert!(v.memory_rows.iter().all(|(n, _)| n != "alpha"));
                assert!(v.host_obs.iter().all(|(n, _)| n != "alpha"));
            }
        }
        // a record carries both, and reads them back
        let f = run(check("cargo: trustc ended by signal 15 (SIGTERM)", "5400"));
        assert_eq!(Facts::parse(&f.to_record()), Some(f.clone()));
        assert_eq!(
            Facts::parse(&f.to_record().replace(FACTS_MAGIC, "aterm-trust facts 5")),
            None,
            "a version-5 record, which could not tell, is never read"
        );
    }

    /// THE BUDGET's mark reaches the facts: the lane's `<log>.budget` (the
    /// budget in seconds) is read beside the log, its presence deciding — a mark
    /// that names no budget is `?` — and a check with no mark has none, however
    /// whole its log.
    #[test]
    fn the_lanes_budget_mark_is_read_into_the_facts() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-trust-facts-budget-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (spec, rc, mark) in [
            ("alpha@1.0.0", "137", Some("60\n")),
            ("beta@2.0.0", "1", Some("\n")),
            ("gamma@3.0.0", "1", None),
        ] {
            fs::write(dir.join(format!("{spec}.log")), DONE).unwrap();
            fs::write(dir.join(format!("{spec}.log.rc")), rc).unwrap();
            if let Some(mark) = mark {
                fs::write(dir.join(format!("{spec}.log.budget")), mark).unwrap();
            }
        }
        let mut p = Plan::parse(&format!(
            "{PLAN_MAGIC}\nlogdir {}\ncrate alpha\talpha@1.0.0\tgate\ncrate beta\tbeta@2.0.0\tgate\ncrate gamma\tgamma@3.0.0\tgate\n",
            dir.display()
        ))
        .unwrap();
        p.measure();
        let _ = fs::remove_dir_all(&dir);
        let got: Vec<(String, bool)> = p
            .facts
            .crates
            .iter()
            .map(|c| match &c.decision {
                Decision::Gate(m) => (m.wall_budget.clone(), !m.ended.is_empty()),
                other => panic!("{other:?}"),
            })
            .collect();
        let want: Vec<(String, bool)> = vec![
            ("60".to_string(), true),
            ("?".to_string(), false),
            (String::new(), false),
        ];
        assert_eq!(got, want);
    }

    fn gate(summary: &str) -> Decision {
        Decision::Gate(Measured {
            rc: "0".into(),
            summary: summary.into(),
            ..Measured::default()
        })
    }

    fn facts(crates: Vec<(&str, Decision)>) -> Facts {
        Facts {
            driver: "/store/trust/9192/bin/targo".into(),
            driver_id: "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)".into(),
            trustd: "/tmp/t.sock".into(),
            grants: "1".into(),
            crates: crates.into_iter().map(|(n, d)| Crate::new(n, d)).collect(),
            ..Facts::default()
        }
    }

    const DRV: &str = "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)";

    #[test]
    fn a_record_round_trips_and_a_cut_short_one_is_none() {
        let mut f = facts(vec![
            ("alpha", gate("  Summary: 3 proved (3 total)")),
            ("beta", Decision::NoLib),
            ("forky", Decision::Queued("queued".into())),
        ]);
        f.vanished.push("ghost".into());
        f.ambiguous
            .push(("dup".into(), "dup@1".into(), "dup@2".into()));
        f.crates.push(Crate::new("dup", Decision::Ambiguous));
        f.provenance = Some(Provenance {
            key: "k".into(),
            ..Provenance::default()
        });
        let text = f.to_record();
        assert_eq!(Facts::parse(&text), Some(f.clone()));
        let cut = text.trim_end().strip_suffix(FACTS_END).unwrap();
        assert_eq!(Facts::parse(cut), None, "no `end`, no record");
        assert_eq!(
            Facts::parse(&text.replace(FACTS_MAGIC, "aterm-trust facts 0")),
            None
        );
        // A schema-1 record — measured in its caller's environment, with the
        // `env` line that screened it by name — is no record of this reader's.
        let schema1 = text
            .replace(FACTS_MAGIC, "aterm-trust facts 1")
            .replace("key k\n", "key k\nenv none\n");
        assert_eq!(Facts::parse(&schema1), None);
        assert_eq!(
            Facts::parse(&format!("{text}crate x\tgate\n")),
            None,
            "nothing after `end`"
        );
    }

    // --- against real git, a real store and real prover directories -------

    use crate::ledger::testing::Git;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A repository with a library and a ratchet, a receipt store, and provers
    /// ([`World::prover`]) whose trustc names the same commit — two builds
    /// that only their content tells apart.
    struct World {
        root: PathBuf,
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    const PROVER_COMMIT: &str = "321aaeda75478038420d97830f14724c33c8dc39";

    impl World {
        fn new(label: &str) -> World {
            let root = std::env::temp_dir().join(format!(
                "aterm-trust-facts-{label}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("work")).unwrap();
            let w = World { root };
            crate::ledger::testing::init(
                &w.work(),
                &[
                    ("user.name", "Test"),
                    ("user.email", "test@example.invalid"),
                    ("commit.gpgsign", "false"),
                ],
            );
            w
        }
        fn work(&self) -> PathBuf {
            self.root.join("work")
        }
        fn git(&self, args: &[&str]) -> String {
            let out = crate::ledger::testing::git()
                .arg("-C")
                .arg(self.work())
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        /// Writes `files`, stages them with one `git add` (one per file was a git
        /// process each), and commits.
        fn commit(&self, files: &[(&str, &str)]) -> String {
            for (path, body) in files {
                let full = self.work().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, body).unwrap();
            }
            if !files.is_empty() {
                let mut add = vec!["add", "--"];
                add.extend(files.iter().map(|(path, _)| *path));
                self.git(&add);
            }
            self.git(&["commit", "-q", "--allow-empty", "-m", "c"]);
            self.git(&["rev-parse", "HEAD"])
        }
        fn cli(&self) -> Git {
            Git::new(self.work())
        }
        fn prover(&self, name: &str) -> Prover {
            testing::prover(&self.root, name, PROVER_COMMIT)
        }
        fn store(&self) -> PathBuf {
            self.work().join(".git/aterm-verify/receipts")
        }
        /// File what a whole-tree run on `commit` with `prover` measured —
        /// alpha `proved` of `total` — under the key that run computes.
        fn file(&self, commit: &str, prover: &Prover, proved: u64, total: u64) -> PathBuf {
            let alpha = testing::gated(
                "alpha",
                &format!("Summary: {proved} proved ({total} total)"),
            );
            testing::file(&self.cli(), &self.store(), prover, commit, DRV, vec![alpha])
        }
        /// `rev`'s stored facts from `prover` ([`facts_for`]), or why there
        /// are none.
        fn found(
            &self,
            prover: &Prover,
            rev: &str,
        ) -> std::result::Result<(Facts, String), String> {
            let head = self.git(&["rev-parse", &format!("{rev}^{{commit}}")]);
            facts_for(&self.cli(), &self.store(), prover, &head).unwrap()
        }
        /// HEAD's stored facts from `prover`, judged against HEAD's own floors.
        fn judged(&self, prover: &Prover) -> Verdict {
            let (facts, _) = self.found(prover, "HEAD").expect("the facts are found");
            let ratchet = ratchet_at(&self.cli(), "HEAD").unwrap();
            judge(&facts, ratchet.as_deref(), RATCHET, MEMORY, Update::NONE)
        }
    }

    fn floor(n: u64) -> String {
        format!("alpha\t{n}\t9\t{DRV}\n")
    }

    /// The ratchet's up-only rule, row by row — and, for reused facts, a first
    /// floor under another prover's name too.
    #[test]
    fn the_up_only_rule_holds_row_by_row() {
        let d = "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)";
        let base = format!("alpha\t987\t3806\t{d}\nbeta\t5\t9\t{d}\nold\t3\t3\tolder prover\n");
        let all = |_: &str| true;
        let low = |tree: &str| floors_lowered(&base, tree, &all, Some(d));
        assert!(low(&base).is_empty(), "unchanged");
        assert!(
            low(&base.replace("987\t3806", "990\t3806")).is_empty(),
            "raised"
        );
        assert!(
            low(&base.replace("987\t3806", "987\t3807")).is_empty(),
            "a total raised to the one measured"
        );
        let got = low(&base.replace("987\t3806", "900\t3806"));
        assert_eq!(got.len(), 1, "{got:?}");

        let said = base.replace(
            &format!("987\t3806\t{d}\n"),
            &format!("985\t3800\t{d}\tthe code left\n"),
        );
        assert!(low(&said).is_empty(), "a reason, the total down as far");
        let excuse = said.replace("985\t3800", "900\t3800");
        assert_eq!(
            low(&excuse).len(),
            1,
            "a reason does not excuse a lowering past the total"
        );
        assert!(
            low(&base.replace("older prover", d)).is_empty(),
            "re-labelled onto the measuring prover"
        );
        // A re-base that LOWERS goes down only with a new reason: a new prover
        // meets the floors it inherits, or its row says why not.
        let rebased = base.replace("3\t3\tolder prover", &format!("1\t3\t{d}"));
        assert_eq!(low(&rebased).len(), 1);
        assert!(
            low(&base.replace(
                "3\t3\tolder prover",
                &format!("1\t3\t{d}\tthe new seal proves two fewer")
            ))
            .is_empty(),
            "a reasoned re-base"
        );
        assert_eq!(
            low(&base.replace("3\t3\tolder prover", "1\t3\tolder prover")).len(),
            1
        );
        let relabel = low(&base.replace(&format!("5\t9\t{d}"), "5\t9\telsewhere"));
        assert_eq!(relabel.len(), 1, "{relabel:?}");
        assert_eq!(
            low(&base.replace(&format!("beta\t5\t9\t{d}\n"), "")).len(),
            1
        );
        assert!(
            floors_lowered(
                &base,
                &base.replace(&format!("beta\t5\t9\t{d}\n"), ""),
                &|n| n != "beta",
                Some(d)
            )
            .is_empty(),
            "a library the facts do not verify"
        );
        let added = format!("{base}gamma\t4\t4\tsomeone\n");
        assert_eq!(low(&added).len(), 1, "a first floor under another name");
        assert!(
            low(&format!("{base}gamma\t4\t4\t{d}\n")).is_empty(),
            "a first floor under the measuring prover"
        );
        assert!(
            floors_lowered(&base, &added, &|n| n != "gamma", Some(d)).is_empty(),
            "a first floor for a library the facts do not verify"
        );
        assert_eq!(low(&base.replace("987\t3806", "9x7\t3806")).len(), 1);
        // With no driver line, no re-label is admitted.
        assert_eq!(
            floors_lowered(&base, &base.replace("older prover", d), &all, None).len(),
            1
        );
    }

    /// ANOTHER PROVER IS ANOTHER KEY, whatever its trustc says: the two
    /// fixture provers name the same trustc commit and differ in one byte of
    /// their driver. Facts from one are never the other's, and the refusal
    /// names the record that IS there.
    #[test]
    fn another_prover_is_another_key() {
        let w = World::new("prover");
        let (a, b) = (w.prover("a"), w.prover("b"));
        assert_ne!(a.identity, b.identity, "two builds, one trustc commit");
        // The one place the stand-in `trustc` runs: `Prover::at` names the fixture
        // prover exactly as the exec-free constructor did.
        let real = Prover::at(&a.bin);
        assert_eq!(
            (&real.identity, &real.trustc),
            (&a.identity, &a.trustc),
            "the fixture is what Prover::at answers"
        );
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
        w.file(&head, &a, 5, 9);
        assert!(w.found(&a, "HEAD").is_ok());
        let why = w.found(&b, "HEAD").unwrap_err();
        assert!(why.contains("no whole-tree Trust facts"), "{why}");
        // a prover that cannot be identified finds nothing
        let bare = Prover::at(&w.root.join("nowhere/bin"));
        assert!(w.found(&bare, "HEAD").is_err());
    }

    /// A RECORD IS READ ONLY UNDER ITS OWN KEY: one copied to the name another
    /// tree's key computes is refused by its own lines, and a cut-short or
    /// foreign file under the right name is no record.
    #[test]
    fn a_record_under_another_key_is_never_read() {
        let w = World::new("forged");
        let a = w.prover("a");
        let first = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
        let filed = w.file(&first, &a, 5, 9);
        let second = w.commit(&[("src/lib.rs", "v2")]);
        let tsr = tree_sans_floors(&w.cli(), &second).unwrap();
        let key = facts_key(&tsr, a.identity.as_ref().unwrap(), LANE_WHOLE_TREE);
        let name = w.store().join(format!("{FACTS_PREFIX}{key}"));
        fs::copy(&filed, &name).unwrap();
        assert!(w.found(&a, "HEAD").is_err());
        fs::write(&name, format!("{FACTS_MAGIC}\ncrate alpha\tgate\n")).unwrap();
        assert!(w.found(&a, "HEAD").is_err());
    }

    /// THE FENCES FIRE FROM STORED FACTS as they did in the lane: a hand-lowered
    /// row whose total the facts exceed, a library with no floor, verification
    /// that did not run and a check that broke each FAIL the tree, and trustd's
    /// refusals that cover a drop leave it undecided.
    #[test]
    fn the_fences_still_fire_from_stored_facts() {
        let w = World::new("fences");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(5))]);
        let tsr = tree_sans_floors(&w.cli(), &head).unwrap();
        let id = a.identity.clone().unwrap();
        // Each case is measured on the commit it is judged at — the fences, not
        // the up-only rule a reusing commit is held to, are what this asks.
        let file_with = |d: Decision| {
            let mut f = facts(vec![("alpha", d)]);
            f.provenance = Some(Provenance {
                key: facts_key(&tsr, &id, LANE_WHOLE_TREE),
                tree_sans_floors: tsr.clone(),
                prover: id.clone(),
                lane: LANE_WHOLE_TREE.into(),
                head: w.git(&["rev-parse", "HEAD"]),
                ..Provenance::default()
            });
            file_record(&w.store(), &f).unwrap();
        };
        file_with(gate("Summary: 5 proved (9 total)"));
        assert_eq!(w.judged(&a).word, "PASS", "(control)");
        let cases: [(&str, Decision, &str); 4] = [
            (
                &format!("alpha\t5\t8\t{DRV}\tone proved obligation left with its code\n"),
                gate("Summary: 5 proved (9 total)"),
                "TOTAL-ABOVE-ROW(>8)",
            ),
            (
                "beta\t1\t1\n",
                gate("Summary: 5 proved (9 total)"),
                "alpha NO-FLOOR",
            ),
            (
                &floor(5),
                Decision::Gate(Measured {
                    rc: "2".into(),
                    transport: true,
                    ..Measured::default()
                }),
                "NOT-VERIFIED(rc=2)",
            ),
            (&floor(5), gate(""), "GATE-BROKE(rc=0)"),
        ];
        for (ratchet, decision, needle) in cases {
            w.commit(&[(RATCHET, ratchet)]);
            file_with(decision);
            let v = w.judged(&a);
            assert!(
                v.word == "FAIL" && v.failed.iter().any(|row| row.contains(needle)),
                "{needle}: {:?}",
                v.failed
            );
        }
        w.commit(&[(RATCHET, &floor(5))]);
        file_with(Decision::Gate(Measured {
            rc: "0".into(),
            summary: "Summary: 4 proved (9 total)".into(),
            refused: 1,
            refused_why: "trustd unreachable: 1".into(),
            ..Measured::default()
        }));
        assert_eq!(w.judged(&a).word, "COULD-NOT-RUN");
    }

    /// The lane files a record only when every input the key leaves out meets
    /// its filing requirements: each refusal below is named and files nothing,
    /// and the control files one the reader then finds.
    #[test]
    fn conclude_files_only_what_a_release_may_read() {
        let w = World::new("conclude");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(3))]);
        let logs = w.root.join("logs");
        let slots = w.root.join("troot");
        fs::create_dir_all(slots.join("j0/debug/deps")).unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("alpha@1.0.0.log"),
            "  Summary: 3 proved, 0 failed (3 total)\nCoverage: 2/2 eligible function bodies verified (complete)\n",
        )
        .unwrap();
        fs::write(logs.join("alpha@1.0.0.log.rc"), "0\n").unwrap();
        fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
        let dep = slots.join("j0/debug/deps/alpha-1.d");
        let plain = "alpha.rmeta: crates/alpha/src/lib.rs\n";
        fs::write(&dep, plain).unwrap();
        // The environment the lane declares it built for every compile.
        const LANE: &str = "lane-pin SOURCE_DATE_EPOCH=0\nlane-pin ATERM_BUILD_GIT_COMMIT=trust-lane\n\
                            lane-passed PATH\nlane-passed HOME\nlane-passed LANG\nlane-passed CARGO_TARGET_DIR\n";
        let plan = |extra: &str| {
            format!(
                "{PLAN_MAGIC}\ndriver {}/targo\ndriver-id {DRV}\ntrustd /tmp/t.sock\nonly \n\
                 logdir {}\nratchet {}\ncrate alpha\talpha@1.0.0\tgate\n\
                 store {}\ncommit {head}\ntree x\nprover {}\nprover-bin {}\ntarget-root {}\n{LANE}{extra}",
                a.bin.display(),
                logs.display(),
                w.work().join(RATCHET).display(),
                w.store().display(),
                a.identity.as_ref().unwrap(),
                a.bin.display(),
                slots.display(),
            )
        };
        let run = |extra: &str| {
            let mut p = Plan::parse(&plan(extra)).unwrap();
            conclude(&w.cli(), &w.work(), &mut p, Update::NONE).unwrap()
        };
        let facts_line = |extra: &str| run(extra).facts_line.unwrap_or_default();
        // not a run that files: the lane's own reason, said
        assert!(facts_line("file the checkout is not clean\n").starts_with("FACTS: none"));
        assert!(
            run("").facts_line.is_none(),
            "a run that does not file says nothing"
        );
        // a plan that declares no environment of its own shows none was built
        let mut bare = Plan::parse(&plan("file \n").replace(LANE, "")).unwrap();
        let l = conclude(&w.cli(), &w.work(), &mut bare, Update::NONE)
            .unwrap()
            .facts_line
            .unwrap();
        assert!(l.starts_with("FACTS: none"), "{l}");
        // a compile that read the floors: the key's premise is false
        fs::write(
            &dep,
            format!("alpha.rmeta: crates/alpha/src/lib.rs {RATCHET}\n"),
        )
        .unwrap();
        assert!(facts_line("file \n").starts_with("FACTS: none"));
        fs::write(
            &dep,
            format!("alpha.rmeta: crates/alpha/src/lib.rs {MEMORY}\n"),
        )
        .unwrap();
        assert!(facts_line("file \n").starts_with("FACTS: none"));
        // THE COMPILES' OWN RECORD OF WHAT THEY READ (`# env-dep:`, the plan's
        // §4.2 env-deps): a value the key does not determine files nothing,
        // named — never its value.
        let reads = |lines: &str| {
            fs::write(&dep, format!("{plain}\n{lines}")).unwrap();
            facts_line("file \n")
        };
        for lines in [
            "# env-dep:ATERM_RELEASE_BUILD=1\n",
            "# env-dep:HOME\n",
            "# env-dep:LANG=en_US.UTF-8\n# env-dep:HOME=/Users//x\n",
            "# env-dep:SOURCE_DATE_EPOCH=5\n",
            "# env-dep:ATERM_GIT_COMMIT=trust-lane\n",
        ] {
            let l = reads(lines);
            assert!(
                l.starts_with("FACTS: none") && !l.contains("/Users//x"),
                "{lines}: {l}"
            );
        }
        // …and one it does: unset and neither pinned nor passed, set by cargo,
        // a pin as pinned, or a build script's `cargo:rustc-env`.
        let build = slots.join("j0/debug/build/aterm-gui-0123/output");
        fs::create_dir_all(build.parent().unwrap()).unwrap();
        fs::write(
            &build,
            "cargo:rerun-if-env-changed=ATERM_BUILD_GIT_COMMIT\ncargo:rustc-env=ATERM_GIT_COMMIT=trust-lane\n",
        )
        .unwrap();
        let l = reads(
            "# env-dep:ATERM_RELEASE_BUILD\n# env-dep:CARGO_PKG_VERSION=1.0.0\n# env-dep:OUT_DIR=/x/out\n\
             # env-dep:SOURCE_DATE_EPOCH=0\n# env-dep:ATERM_GIT_COMMIT=trust-lane\n",
        );
        assert!(l.starts_with("FACTS: ") && !l.contains("none"), "{l}");
        fs::remove_file(&build).unwrap();
        fs::write(&dep, plain).unwrap();
        // cargo config outside the tree: another input the key does not cover
        let home = w.root.join("cargo-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "[profile.dev]\ndebug-assertions = false\n",
        )
        .unwrap();
        let l = facts_line(&format!("file \ncargo-home {}\n", home.display()));
        assert!(l.starts_with("FACTS: none"), "{l}");
        // The control meets the filing requirements: filed, and found.
        let l = run("file \n").facts_line.unwrap();
        assert!(
            l.starts_with("FACTS: ") && !l.contains("none"),
            "the control files: {l}"
        );
        assert!(w.found(&a, "HEAD").is_ok(), "the control is found");
        // slots that hold no dep-info show nothing of what the compiles read
        fs::remove_file(&dep).unwrap();
        let l = facts_line("file \n");
        assert!(
            l.starts_with("FACTS: none") && l.contains("no dep-info"),
            "{l}"
        );
        fs::write(&dep, plain).unwrap();
        // a tree git cannot list (a commit it cannot name, or a git that did not
        // answer) keys no record, and the verdict stands
        let mut unlisted =
            Plan::parse(&plan("file \n").replace(&format!("commit {head}"), "commit 0000"))
                .unwrap();
        let done = conclude(&w.cli(), &w.work(), &mut unlisted, Update::NONE).unwrap();
        let l = done.facts_line.unwrap();
        assert!(
            l.starts_with("FACTS: none") && l.contains("git could not list"),
            "{l}"
        );
        assert_eq!(done.verdict.word, "PASS");
        // a prover that changed during the run
        let mut moved =
            Plan::parse(&plan("file \n").replace(a.identity.as_ref().unwrap(), "0000")).unwrap();
        let l = conclude(&w.cli(), &w.work(), &mut moved, Update::NONE)
            .unwrap()
            .facts_line
            .unwrap();
        assert!(l.starts_with("FACTS: none"), "{l}");
        // the verdict the lane prints is the judge's, whatever is filed
        let done = run("");
        assert_eq!(done.verdict.word, "PASS");
    }

    /// `trust judge` answers 0 / 1 / 2: it judges stored facts against the
    /// WORKING floors (no facts is no pass; a commit git cannot name is no
    /// answer), and with `--update-ratchet` re-bases them from stored facts
    /// with no run — a first floor and a high-water mark, never a same-prover
    /// lowering.
    #[test]
    fn the_trust_verbs_answer_three_ways() {
        let w = World::new("cli");
        let a = w.prover("a");
        let measured = format!("alpha\t6\t10\t{DRV}\n");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &measured)]);
        let judge_at = |commit: &str, update| TrustCmd::Judge {
            commit: commit.into(),
            prover: Some(a.bin.clone()),
            update,
            reason: None,
        };
        let judge_cmd = |update| judge_at("HEAD", update);
        assert_eq!(
            run_cmd(&w.work(), judge_cmd(false)),
            1,
            "no facts: not verified"
        );
        assert_eq!(
            run_cmd(&w.work(), judge_at("no-such-rev", false)),
            2,
            "no commit: no answer"
        );
        w.file(&head, &a, 5, 9);
        let working = || fs::read_to_string(w.work().join(RATCHET)).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1, "5 of a floor of 6");
        // the working floors, lowered by hand to what was measured: judged, and
        // held to the up-only rule against the floors the facts were measured
        // with — a same-prover lowering with no reason fails, named, unwritten
        fs::write(w.work().join(RATCHET), floor(5)).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1);
        assert_eq!(working(), floor(5));
        // …and one with its reason, the total down as far, passes
        let said = format!("alpha\t5\t9\t{DRV}\tone proved obligation left with its code\n");
        fs::write(w.work().join(RATCHET), &said).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 0);
        // an update never lowers this prover's floor ...
        fs::write(w.work().join(RATCHET), &measured).unwrap();
        assert_eq!(run_cmd(&w.work(), judge_cmd(true)), 1);
        assert_eq!(working(), measured);
        // ... and records a first one, for a library the measured commit had
        // no floor for
        let w = World::new("cli-first");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, "")]);
        w.file(&head, &a, 5, 9);
        let judge_cmd = |update| TrustCmd::Judge {
            commit: "HEAD".into(),
            prover: Some(a.bin.clone()),
            update,
            reason: None,
        };
        assert_eq!(run_cmd(&w.work(), judge_cmd(false)), 1, "NO-FLOOR");
        assert_eq!(run_cmd(&w.work(), judge_cmd(true)), 0);
        assert_eq!(
            fs::read_to_string(w.work().join(RATCHET)).unwrap(),
            format!("alpha\t5\t9\t{DRV}\n")
        );
    }

    #[test]
    fn the_key_leaves_out_the_floors_and_nothing_else() {
        let w = World::new("key");
        let one = w.commit(&[
            ("src/lib.rs", "v1"),
            (RATCHET, "alpha\t1\t1\n"),
            (MEMORY, "alpha\t16.0\t1.4\t=\t-\n"),
        ]);
        let two = w.commit(&[(RATCHET, "alpha\t2\t2\n")]);
        // what --update-ratchet writes beside the floors: the memory record
        let mem = w.commit(&[(MEMORY, "alpha\t16.0\t1.6\t=\t2026-09-30 at 16.0 GiB\n")]);
        // and the cost-history seed
        let costs = w.commit(&[(COSTS, "alpha\t1\t1\t7\t2026-10-16T00:00:00Z\n")]);
        let three = w.commit(&[("crates/alpha/src/lib.rs", "pub fn f() {}\n")]);
        let t = |c: &str| tree_sans_floors(&w.cli(), c).unwrap();
        assert_eq!(t(&one), t(&two), "the floors alone move nothing");
        assert_eq!(t(&two), t(&mem), "nor does the memory record");
        assert_eq!(t(&mem), t(&costs), "nor does the cost-history seed");
        assert_ne!(t(&mem), t(&three), "any other file is an input");
        // a ratchet or memory record deleted, or added, is still only the floors
        w.git(&["rm", "-q", RATCHET, MEMORY, COSTS]);
        let four = w.commit(&[]);
        assert_eq!(t(&three), t(&four));
        let k = |t: &str, p: &str, l: &str| facts_key(t, p, l);
        assert_ne!(k("t", "p1", LANE_WHOLE_TREE), k("t", "p2", LANE_WHOLE_TREE));
        assert_ne!(k("t", "p", LANE_WHOLE_TREE), k("u", "p", LANE_WHOLE_TREE));
        assert_ne!(k("t", "p", LANE_WHOLE_TREE), k("t", "p", "narrowed"));
    }

    #[test]
    fn the_judge_holds_floors_as_the_lane_did() {
        let f = facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]);
        let pass = judge(
            &f,
            Some(&format!("alpha\t3\t3\t{DRV}\n")),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((pass.word, pass.exit), ("PASS", 0));
        // the same facts, a higher floor: a regression, judged, no run
        let fail = judge(
            &f,
            Some(&format!("alpha\t4\t3\t{DRV}\n")),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((fail.word, fail.exit), ("FAIL", 1));
        assert_eq!(fail.failed, ["alpha RATCHET-REGRESSION(<4) 3/3"]);
        // no floor fails; --update-ratchet records it
        let none = judge(&f, Some(""), "R", "M", Update::NONE);
        assert_eq!(none.word, "FAIL");
        let rec = judge(&f, None, "R", "M", Update::RAISE);
        assert_eq!(rec.word, "PASS");
        assert_eq!(
            rec.ratchet.as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        // a floor that is not a count fails, named — never a crash, never a pass
        let bad = judge(&f, Some("alpha\tthree\t3\n"), "R", "M", Update::NONE);
        assert_eq!(bad.word, "FAIL");
        // ANOTHER PROVER'S FLOOR IS HELD (no floor excuses): 3 proved under its
        // floor of 9 is a regression, named beside the prover that set it …
        let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
        let other = format!("alpha\t9\t9\t{theirs}\n");
        let moved = judge(&f, Some(&other), "R", "M", Update::NONE);
        assert_eq!((moved.word, moved.exit), ("FAIL", 1));
        assert_eq!(moved.failed, ["alpha RATCHET-REGRESSION(<9) 3/3"]);
        // … which --update-ratchet never lowers, and says how it goes down …
        let kept = judge(&f, Some(&other), "R", "M", Update::RAISE);
        assert_eq!(kept.ratchet, None);
        // … only with its reason, in the fifth column the up-only rule reads
        let reason = "trust 5ec1 proves fewer; owner-approved";
        let lowered = judge(
            &f,
            Some(&other),
            "R",
            "M",
            Update {
                on: true,
                reason: Some(reason),
            },
        );
        assert_eq!(
            lowered.ratchet.as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\t{reason}\n").as_str())
        );
        // A re-base another prover's floor this run MEETS needs no reason.
        let met = format!("alpha\t2\t2\t{theirs}\n");
        assert_eq!(judge(&f, Some(&met), "R", "M", Update::NONE).word, "PASS");
        assert_eq!(
            judge(&f, Some(&met), "R", "M", Update::RAISE)
                .ratchet
                .as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        // a same-prover floor is never lowered by an update, reason or none
        for update in [
            Update::RAISE,
            Update {
                on: true,
                reason: Some(reason),
            },
        ] {
            assert_eq!(
                judge(&f, Some(&format!("alpha\t4\t3\t{DRV}\n")), "R", "M", update).ratchet,
                None
            );
        }
        // …and a total corrected with the floor unchanged is written
        assert_eq!(
            judge(
                &f,
                Some(&format!("alpha\t3\t4\t{DRV}\n")),
                "R",
                "M",
                Update::RAISE
            )
            .ratchet
            .as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        // a hand-lowered row is held to its total
        let why = judge(
            &f,
            Some(&format!("alpha\t2\t2\t{DRV}\twent with its code\n")),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(why.word, "FAIL");
        assert!(why.report.contains("TOTAL-ABOVE-ROW(>2)"), "{}", why.report);
    }

    /// THE RE-BASE GUARD holds a reasoned lowering too: another prover's floor
    /// set at 16 GiB is not lowered from a run that ran out of a 1 GiB ceiling —
    /// it would be a small host's count every larger host then passes without
    /// trying — and is lowered from one whose census shows room, the memory
    /// record taking that run's ceiling and need.
    #[test]
    fn a_reasoned_rebase_is_held_to_the_memory_guard() {
        let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
        let floor = format!("alpha\t3\t3\t{theirs}\n");
        let reason = Update {
            on: true,
            reason: Some("the new seal proves two fewer"),
        };
        let run = |budget_rows: u64, peak: &str| {
            let mut f = facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "0".into(),
                    summary: "Summary: 1 proved (1 total)".into(),
                    budget_rows,
                    peak: peak.into(),
                    ceiling: "1.0 GiB".into(),
                    ..Measured::default()
                }),
            )]);
            f.crates[0].memory = MemRow {
                f: "16.0".into(),
                ..MemRow::default()
            };
            f.host = Host {
                mem: 8 * 1_073_741_824,
                memtotal: false,
            };
            f.measured_at = "2026-09-30T00:00:00Z".into();
            judge(&f, Some(&floor), "R", "M", reason)
        };
        let ran_out = run(2, "1.3 GiB");
        assert_eq!(ran_out.ratchet, None);
        let roomy = run(0, "0.6 GiB");
        assert_eq!(
            roomy.ratchet.as_deref(),
            Some(format!("alpha\t1\t1\t{DRV}\tthe new seal proves two fewer\n").as_str())
        );
        assert_eq!(
            roomy.memory_rows,
            [(
                "alpha".to_string(),
                MemRow {
                    f: "1.0".into(),
                    ram: "8".into(),
                    at: "1.0".into(),
                    need: "0.6".into(),
                    bound: "=".into(),
                    when: "2026-09-30".into(),
                }
            )]
        );
    }

    /// A LOG THAT NAMES NO CEILING records none (every log of a seal from trust
    /// main, which has no in-run ceiling), never this host's RAM / 8: a floor
    /// value its run writes takes no floor ceiling, its reasoned lowering of a
    /// floor set at one is withheld, and a `held` observation files its ceiling
    /// as absent. On this 24 GiB host RAM / 8 is 3.0, above the floor's 1.0.
    #[test]
    fn a_log_that_names_no_ceiling_records_none() {
        let theirs = "targo 1.99.0-dev (bbbbbbbbb 2026-09-17) (targo 0.1.0)";
        let run = |skipped: Option<u64>, floor: &str, update: Update<'_>| {
            let mut f = facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "0".into(),
                    summary: "Summary: 3 proved (3 total)".into(),
                    skipped,
                    ..Measured::default()
                }),
            )]);
            f.crates[0].memory = MemRow {
                f: "1.0".into(),
                ..MemRow::default()
            };
            f.host = host(24);
            judge(&f, Some(floor), "R", "M", update)
        };
        // A re-base it meets: written, and its floor ceiling is absent, not 3.0.
        let met = run(None, &format!("alpha\t2\t2\t{theirs}\n"), Update::RAISE);
        assert_eq!(
            met.ratchet.as_deref(),
            Some(format!("alpha\t3\t3\t{DRV}\n").as_str())
        );
        assert_eq!(met.memory_rows, [("alpha".to_string(), MemRow::default())]);
        // A reasoned lowering of a floor set at 1.0 GiB: withheld, and said.
        let low = run(
            None,
            &format!("alpha\t9\t9\t{theirs}\n"),
            Update {
                on: true,
                reason: Some("the new seal proves fewer"),
            },
        );
        assert_eq!(low.ratchet, None);
        assert!(
            low.report
                .contains("RE-BASE WITHHELD: alpha: this run's log names no verifier ceiling"),
            "{}",
            low.report
        );
        // It ran out (bodies skipped) and met its floor: held, at no ceiling.
        let held = run(Some(2), &format!("alpha\t3\t3\t{DRV}\n"), Update::NONE);
        assert_eq!(
            held.host_obs,
            [(
                "alpha".to_string(),
                Some(["held", "-", "-", "3", "3"].map(String::from))
            )]
        );
    }

    /// A SHORTFALL THE CLOCK MAY HAVE CAUSED decides nothing (no floor excuses):
    /// 2 proved with 1 lapsed against a floor of 3 is UNDECIDED — in neither total,
    /// COULD NOT RUN, and named for a re-measure — never a pass; a drop larger
    /// than every lapse is a regression; lapses beside a floor met change nothing.
    /// The re-measure named is one the lane runs: `--remeasure` completes FILED
    /// facts; a `-p` run files none, so it names its own scope again.
    #[test]
    fn a_lapsed_shortfall_is_undecided_never_a_pass() {
        let lapsed = |proved: u64, n: u64| {
            let mut f = facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "0".into(),
                    summary: format!("Summary: {proved} proved (9 total)"),
                    lapsed: n,
                    ..Measured::default()
                }),
            )]);
            f.provenance = Some(Provenance::default());
            f
        };
        let floor3 = format!("alpha\t3\t9\t{DRV}\n");
        let v = judge(&lapsed(2, 1), Some(&floor3), "R", "M", Update::NONE);
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
        assert_eq!(v.unverified, ["alpha"]);
        assert_eq!(v.total, "0/0", "in neither total");
        // no floor is written from it
        assert_eq!(
            judge(&lapsed(2, 1), Some(&floor3), "R", "M", Update::RAISE).ratchet,
            None
        );
        let v = judge(&lapsed(1, 1), Some(&floor3), "R", "M", Update::NONE);
        assert_eq!(v.failed, ["alpha RATCHET-REGRESSION(<3) 1/9"]);
        let v = judge(&lapsed(3, 5), Some(&floor3), "R", "M", Update::NONE);
        assert_eq!(v.word, "PASS");
    }

    /// A PROOF CHECK ITS CALLER CANCELLED is the clock's credit ONCE (aterm-codec,
    /// 0.101.0's lane: 310 proved against a floor of 383 at load ~50, with 131 of
    /// these lines and no `timed_out=`; 383 at load ~15 with 91-121 of them). In a
    /// filed record's first measurement a shortfall they could cover is UNDECIDED
    /// and named for a re-measure; one they cannot cover is still the code's; a
    /// floor met is untouched; an invalid derivation counts for nothing. The
    /// re-measure, and a run that files nothing, are judged on `timed_out=` alone:
    /// a passing run prints these lines too, so crediting them every time would
    /// leave a real drop undecided on every run.
    #[test]
    fn a_cancelled_proof_check_is_the_clocks_credit_once() {
        let cancelled = " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=proof validation cancelled by the caller (interrupt, deadline, or memory limit), checked_steps=1139, skipped_hole_steps=0, total_steps=1139\n";
        let invalid = " ERROR ay_dpll::executor::proof::check::internal internal proof checker rejected UNSAT proof, error=step t5 has invalid resolution derivation, checked_steps=4, skipped_hole_steps=0, total_steps=9\n";
        let run = |proved: u64, n: usize| {
            let text = format!(
                "{}{}    = note: native verifier status: Inconclusive; requested=82, proved=27, failed=0, unknown=34, timed_out=0, unsupported=21\n  Summary: {proved} proved, 26 failed (553 total)\nCoverage: 101/101 eligible function bodies verified (complete)\n",
                cancelled.repeat(n),
                invalid.repeat(7),
            );
            let mut m = log(&text);
            m.rc = "1".into();
            let mut f = facts(vec![("aterm-codec", Decision::Gate(m))]);
            f.provenance = Some(Provenance::default());
            f
        };
        let only = |f: &Facts| match &f.crates[0].decision {
            Decision::Gate(m) => (m.lapsed, m.cancelled),
            _ => unreachable!(),
        };
        assert_eq!(
            only(&run(310, 131)),
            (0, 131),
            "each cancellation, and no invalid derivation"
        );
        let floor = format!("aterm-codec\t383\t551\t{DRV}\n");
        let v = judge(&run(310, 131), Some(&floor), "R", "M", Update::NONE);
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2), "{}", v.report);
        let regression = ["aterm-codec RATCHET-REGRESSION(<383) 310/553"];
        let v = judge(&run(310, 72), Some(&floor), "R", "M", Update::NONE);
        assert_eq!(v.failed, regression);
        let v = judge(&run(310, 0), Some(&floor), "R", "M", Update::NONE);
        assert_eq!(v.failed, regression);
        let v = judge(&run(383, 91), Some(&floor), "R", "M", Update::NONE);
        assert_ne!(v.word, "COULD-NOT-RUN", "{}", v.report);
        assert!(v.failed.is_empty(), "{:?}", v.failed);
        // Measured AGAIN on the same key: the same shortfall is decided — the
        // code's, since no `timed_out=` covers it — and a met floor passes.
        let again = |proved: u64, n: usize| {
            let mut f = run(proved, n);
            if let Some(p) = f.provenance.as_mut() {
                p.remeasured
                    .push("2026-10-01T00:00:00Z abc aterm-codec".into());
            }
            f
        };
        let v = judge(&again(310, 131), Some(&floor), "R", "M", Update::NONE);
        assert_eq!(v.failed, regression, "{}", v.report);
        let v = judge(&again(383, 131), Some(&floor), "R", "M", Update::NONE);
        assert!(
            v.failed.is_empty() && v.word != "COULD-NOT-RUN",
            "{}",
            v.report
        );
        // Another library's re-measure leaves this one's first measurement as it was.
        let mut other = run(310, 131);
        if let Some(p) = other.provenance.as_mut() {
            p.remeasured
                .push("2026-10-01T00:00:00Z abc aterm-grid".into());
        }
        let v = judge(&other, Some(&floor), "R", "M", Update::NONE);
        assert_eq!(v.word, "COULD-NOT-RUN", "{}", v.report);
        // A run that files nothing has no re-measure to resolve it.
        let mut unfiled = run(310, 131);
        unfiled.provenance = None;
        let v = judge(&unfiled, Some(&floor), "R", "M", Update::NONE);
        assert_eq!(v.failed, regression, "{}", v.report);
    }

    /// A CHECK TARGO WOULD NOT START is measured from its log and judged
    /// NOT-VERIFIED(trustd) — COULD NOT RUN, never GATE-BROKE — naming what held
    /// the socket and what to do.
    #[test]
    fn a_check_targo_would_not_start_is_undecided() {
        let m = log(&format!(
            "error: {TRUSTD_REFUSED}: another trustd owns the socket\n"
        ));
        assert_eq!(m.launch, "owned");
        let m = log(&format!("error: {TRUSTD_REFUSED}\n{TRUSTD_DIRTY}\n"));
        assert_eq!(m.launch, "dirty");
        assert_eq!(
            log(&format!(
                "{TRUSTD_REFUSED}\n  Summary: 1 proved (1 total)\n"
            ))
            .launch,
            "",
            "a check that reached its Summary started"
        );
        let refused = |launch: &str, held: &str| {
            facts(vec![(
                "alpha",
                Decision::Gate(Measured {
                    rc: "1".into(),
                    launch: launch.into(),
                    held: held.into(),
                    held_what: "a trustd of another build serves /tmp/t.sock".into(),
                    ..Measured::default()
                }),
            )])
        };
        let v = judge(
            &refused("owned", "foreign"),
            Some(""),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
        assert_eq!(v.unverified, ["alpha"]);
        let v = judge(&refused("dirty", ""), Some(""), "R", "M", Update::NONE);
        assert_eq!((v.word, v.exit), ("COULD-NOT-RUN", 2));
    }

    /// The recovery command is pasted into a shell, and atpkg's store lives under
    /// `Application Support`: the path stays one word.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_recovery_command_survives_a_store_path_with_a_space() {
        let r = trustd_remedy("dirty", "/tmp/t.sock", "/a b/bin/trustd");
        assert!(
            r.contains("`'/a b/bin/trustd' --recover-after-crash --confirm-no-solvers --socket /tmp/t.sock`"),
            "{r}"
        );
    }

    /// The absence of matches must include a matching ancestor, not just
    /// siblings. A uniquely named owned libtest child is the ancestor here;
    /// no actual compiler, solver, daemon or recovery is involved.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_recovery_probe_observes_a_matching_owned_ancestor() {
        use std::os::unix::fs::MetadataExt as _;
        use std::time::{Duration, Instant};

        const CHILD: &str = "ATERM_TEST_PGREP_ANCESTOR";
        if let Some(path) = std::env::var_os(CHILD) {
            let path = PathBuf::from(path);
            let own = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
            let linked = std::fs::metadata(&path).unwrap();
            assert_eq!((own.dev(), own.ino()), (linked.dev(), linked.ino()));
            let name = path.file_name().unwrap().to_str().unwrap();
            let probe = |options: &str| {
                aterm_uds::spawnfd::command("/usr/bin/pgrep")
                    .args([options, name])
                    .output()
                    .expect("owned process-name probe")
            };
            let matches_self = |out: &std::process::Output| {
                out.status.success()
                    && String::from_utf8_lossy(&out.stdout).lines().any(|line| {
                        line.split_whitespace()
                            .next()
                            .and_then(|pid| pid.parse::<u32>().ok())
                            == Some(std::process::id())
                    })
            };
            // First establish that the real actor has its unique process
            // name; the old options then demonstrate the omission.
            assert!(matches_self(&probe("-alx")), "owned ancestor is observable");
            let old = probe("-lx");
            assert_eq!(old.status.code(), Some(1));
            assert!(old.stdout.is_empty());
            let current = probe(trustd_probe_options().expect("macOS probe"));
            println!(
                "OWNED_ANCESTOR_PID={} PROBE={}",
                std::process::id(),
                String::from_utf8_lossy(&current.stdout).trim()
            );
            assert!(
                matches_self(&current),
                "the declared probe must see its matching ancestor"
            );
            return;
        }

        let scratch = aterm_tempfile::Builder::new()
            .prefix("trustd-probe-ancestor-")
            .tempdir()
            .unwrap();
        let named = scratch.path().join(format!("atpa{}", std::process::id()));
        std::fs::hard_link(std::env::current_exe().unwrap(), &named).unwrap();
        let test = "trust_facts::tests::the_recovery_probe_observes_a_matching_owned_ancestor";
        let mut child = aterm_uds::spawnfd::command(&named);
        child
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, &named);
        // The parent owns the child's whole process group, including its
        // synchronous pgrep children, through a one-minute hang detector.
        let out =
            aterm_uds::deadline::output_until(&mut child, Instant::now() + Duration::from_secs(60))
                .expect("owned ancestor fixture finishes and is reaped");
        println!("{}", String::from_utf8_lossy(&out.stdout));
        assert!(out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("OWNED_ANCESTOR_PID="));
    }

    /// MEASURED AGAIN ON THE SAME KEY: the fold replaces the libraries the new
    /// run measured and names it; facts of another key, another host or another
    /// driver, or of a library the record does not select, are never folded.
    #[test]
    fn a_remeasure_folds_only_its_own_key() {
        let prov = |key: &str| Provenance {
            key: key.into(),
            tree_sans_floors: "t".into(),
            prover: "p".into(),
            lane: LANE_WHOLE_TREE.into(),
            head: "h1".into(),
            date: "d1".into(),
            ..Provenance::default()
        };
        let mut base = facts(vec![
            ("alpha", gate("Summary: 1 proved (3 total)")),
            ("beta", gate("Summary: 2 proved (2 total)")),
        ]);
        base.provenance = Some(prov("k"));
        let mut again = facts(vec![("alpha", gate("Summary: 3 proved (3 total)"))]);
        again.provenance = Some(Provenance {
            head: "h2".into(),
            date: "d2".into(),
            ..prov("k")
        });
        let done = remeasured(&base, &again).unwrap();
        assert_eq!(done.crates[0].decision, gate("Summary: 3 proved (3 total)"));
        assert_eq!(done.crates[1], base.crates[1]);
        let p = done.provenance.unwrap();
        assert_eq!(
            (p.head.as_str(), p.remeasured),
            ("h1", vec!["d2 h2 alpha".to_string()])
        );
        let mut other_key = again.clone();
        other_key.provenance = Some(prov("k2"));
        assert!(
            remeasured(&base, &other_key)
                .unwrap_err()
                .contains("two keys")
        );
        let mut other_host = again.clone();
        other_host.host.mem = 1;
        assert!(remeasured(&base, &other_host).unwrap_err().contains("host"));
        let mut other_driver = again.clone();
        other_driver.driver_id = "targo 2".into();
        assert!(
            remeasured(&base, &other_driver)
                .unwrap_err()
                .contains("driver")
        );
        let mut stranger = again.clone();
        stranger.crates = vec![Crate::new("gamma", gate("Summary: 1 proved (1 total)"))];
        assert!(remeasured(&base, &stranger).unwrap_err().contains("gamma"));
        // the record round-trips with its re-measure named
        let text = remeasured(&base, &again).unwrap().to_record();
        assert!(text.contains("\nremeasured d2 h2 alpha\n"), "{text}");
        assert_eq!(
            Facts::parse(&text).unwrap().provenance.unwrap().remeasured,
            ["d2 h2 alpha"]
        );
    }

    /// A host of `gib` GiB of RAM, as the lane reads a Mac's.
    fn host(gib: u64) -> Host {
        Host {
            mem: gib * 1_073_741_824,
            memtotal: false,
        }
    }

    /// One memory row: `<ram> <run ceiling> <need> <bound>`, its floor set at 16.0.
    fn mem(ram: &str, at: &str, need: &str, bound: &str) -> MemRow {
        MemRow {
            f: "16.0".into(),
            ram: ram.into(),
            at: at.into(),
            need: need.into(),
            bound: bound.into(),
            when: "m".into(),
        }
    }

    /// THE NEED IS THE HOST'S (2026-10-01). aterm-cli's need measured with room
    /// on m101 (24 GiB, the ceiling faked to 4.0) printed 3.0, and at m101's
    /// real 3.0 GiB ceiling it proved 0 of 443 against a floor of 75 — the
    /// record said ~2.1, measured under the 8 GiB Mac's memory pressure, so the
    /// lane gated it and called the 0 a regression. Each host now reads the row
    /// measured on its own RAM, else the nearest below (a lower bound, which
    /// never says it fits), else the nearest above (an upper bound, read as
    /// before): a 24 GiB host refuses it up front, naming the 32 GiB its need
    /// was measured to fit; a 32 GiB host gates it; the 8 GiB and 16 GiB hosts
    /// refuse it as they always did.
    #[test]
    fn a_need_printed_at_its_own_hosts_ceiling_is_refused_and_names_what_fits() {
        let cli = [mem("8", "4.0", "2.1", "="), mem("24", "4.0", "3.0", "=")];
        let on = |gib: u64| {
            let h = host(gib);
            let row = row_for(&cli, &h).unwrap().clone();
            (memory_refusal(&h, "aterm-cli", true, &row, None), row)
        };
        let (at24, row) = on(24);
        assert_eq!(row.ram, "24");
        let at24 = at24.expect("a 24 GiB host refuses it up front");
        assert_eq!(at24.token, "~3.0");
        assert_eq!(at24.sure, Some(32));
        // 32 GiB reads the 24 GiB row as a lower bound: gated, never "fits".
        let (at32, row) = on(32);
        assert_eq!((at32, row.ram.as_str()), (None, "24"));
        assert!(!fits(&row, &host(32), "4.0"));
        // 16 GiB reads the 8 GiB row (a lower bound above its 2.0): refused, as before.
        let (at16, row) = on(16);
        assert_eq!(row.ram, "8");
        assert!(at16.is_some());
        let (at8, _) = on(8);
        assert!(at8.is_some());
    }

    /// NO HOST LOSES WHAT IT GATED (2026-10-01): a single row read on another
    /// RAM gates and refuses what it did before rows had a RAM. A need measured
    /// on MORE RAM is an upper bound, so one printed at this host's ceiling still
    /// fits — aterm-regex's 1.0, measured on 24 GiB, at an 8 GiB host's 1.0 —
    /// and one measured on LESS RAM is a lower bound: it refuses only what is
    /// above the ceiling, gates the rest, and never says it fits (aterm-census's
    /// 1.3 from the 8 GiB Mac was 6.4 on 24 GiB). On its own RAM a need fits
    /// when printed below the ceiling, or measured with room at no more than it.
    #[test]
    fn a_need_from_another_ram_is_read_as_the_bound_it_is() {
        let regex = [mem("24", "3.0", "1.0", "=")];
        let census = [mem("8", "4.0", "1.3", "=")];
        for gib in [8, 16, 24, 32, 128] {
            let h = host(gib);
            let c = h.ceiling().unwrap();
            let row = row_for(&regex, &h).unwrap();
            assert_eq!(
                memory_refusal(&h, "aterm-regex", true, row, None),
                None,
                "{gib}"
            );
            assert_eq!(fits(row, &h, &c), gib <= 24, "regex {gib}");
            let row = row_for(&census, &h).unwrap();
            assert!(!fits(row, &h, &c), "census {gib}");
            assert_eq!(
                memory_refusal(&h, "aterm-census", true, row, None).is_some(),
                gib == 8,
                "census {gib}"
            );
        }
        // Measured here with room at this very ceiling, its peak above it: fits.
        let verify = mem("24", "3.0", "3.2", "=");
        assert!(fits(&verify, &host(24), "3.0"));
        assert_eq!(memory_refusal(&host(24), "v", true, &verify, None), None);
        // A lower bound (`>`) at or above the ceiling refuses, from any RAM.
        let gui = [mem("128", "16.0", "16.4", ">")];
        assert!(memory_refusal(&host(24), "g", true, &gui[0], None).is_some());
        assert_eq!(row_for(&gui, &host(24)).unwrap().ram, "128");
        // Unreadable memory: the row measured on the most RAM, nothing refused.
        let both = [mem("8", "4.0", "2.1", "="), mem("24", "4.0", "3.0", "=")];
        assert_eq!(row_for(&both, &Host::default()).unwrap().ram, "24");
        assert_eq!(
            memory_refusal(&Host::default(), "x", true, &both[1], None),
            None
        );
    }

    /// THE RECORD'S SHAPE (2026-10-01): one floor ceiling per library, one row
    /// per RAM, a need with its bound and the RAM it was measured on, and a row
    /// with no need only alone — each a COULD NOT RUN naming the row.
    #[test]
    fn the_memory_record_is_held_to_its_shape() {
        let ok = "# c\nalpha\t16.0\t24\t4.0\t3.0\t=\tm101\nalpha\t16.0\t8\t4.0\t2.1\t=\tA18\nbeta\t16.0\t-\t-\t-\t-\t-\n";
        let rec = parse_memory(ok, "R").unwrap();
        assert_eq!(rec["alpha"].len(), 2);
        if let Err(e) = parse_memory(include_str!("../trust/memory.tsv"), MEMORY) {
            panic!("the committed record does not parse: {e}");
        }
        assert_eq!(
            rec["beta"],
            [MemRow {
                f: "16.0".into(),
                ..MemRow::default()
            }]
        );
        for (text, rule) in [
            (
                "alpha\t16.0\t24\t4.0\t3.0\t=\tx\nalpha\t1.0\t8\t1.0\t0.3\t=\tx\n",
                "two floor ceilings",
            ),
            (
                "alpha\t16.0\t24\t4.0\t3.0\t=\tx\nalpha\t16.0\t24.0\t3.0\t3.1\t>\tx\n",
                "two rows measured on 24.0 GiB",
            ),
            (
                "alpha\t16.0\t-\t-\t-\t-\t-\nalpha\t16.0\t24\t3.0\t1.0\t=\tx\n",
                "a row with no need beside another",
            ),
            (
                "alpha\t16.0\t-\t3.0\t1.0\t=\tx\n",
                "a need names the RAM it was measured on",
            ),
            (
                "alpha\t16.0\t24\t-\t-\t-\tx\n",
                "a need names the RAM it was measured on",
            ),
            (
                "alpha\t16.0\t-\t3.0\t-\t-\tx\n",
                "the run ceiling '3.0' with no need",
            ),
            ("alpha\t16.0\tlots\t3.0\t1.0\t=\tx\n", "the host RAM 'lots'"),
            ("alpha\t16.0\t24\tx\t1.0\t=\tx\n", "the run ceiling 'x'"),
        ] {
            assert!(parse_memory(text, "R").is_err(), "{rule}");
        }
    }

    /// WHAT A RUN LEARNS IS ITS OWN RAM'S (2026-10-01): `--update-ratchet` on a
    /// 24 GiB host writes a row measured on 24 GiB — a new one beside the 8 GiB
    /// row it read, never over it — or rewrites its own; the floor ceiling it
    /// writes is every row's; a host whose RAM is unknown files no need.
    #[test]
    fn a_run_writes_the_need_it_measured_to_its_own_ram() {
        let census = |peak: &str, ceil: &str, exhausted: bool| Census {
            peak: Some(peak.into()),
            ceil: Some(ceil.into()),
            exhausted,
            ..Census::default()
        };
        let a18 = mem("8", "4.0", "2.1", "=");
        let new = mem_learn(
            &a18,
            "16.0",
            &census("3.0", "3.0", true),
            "2026-10-01",
            &host(24),
        )
        .unwrap();
        assert_eq!(
            (
                new.ram.as_str(),
                new.at.as_str(),
                new.need.as_str(),
                new.bound.as_str(),
                new.when.as_str()
            ),
            ("24", "3.0", "3.0", ">", "2026-10-01")
        );
        let own = mem("24", "4.0", "3.0", "=");
        assert_eq!(
            mem_learn(&own, "16.0", &census("2.9", "3.0", true), "d", &host(24)),
            None
        );
        let more = mem_learn(&own, "16.0", &census("3.3", "3.0", true), "d", &host(24)).unwrap();
        assert_eq!(
            (more.ram.as_str(), more.need.as_str(), more.bound.as_str()),
            ("24", "3.3", ">")
        );
        let unknown = mem_learn(
            &a18,
            "4.0",
            &census("3.0", "3.0", false),
            "d",
            &Host::default(),
        );
        assert_eq!(
            unknown,
            Some(MemRow {
                f: "4.0".into(),
                ..a18.clone()
            })
        );
        // Written in: beside the row it read, the floor ceiling every row's, by RAM.
        let text = "# h\naterm-cli\t16.0\t8\t4.0\t2.1\t=\tA18\nbeta\t16.0\t-\t-\t-\t-\t-\n";
        let mut learned = new.clone();
        learned.f = "4.0".into();
        let beta = mem_learn(
            &MemRow {
                f: "16.0".into(),
                ..MemRow::default()
            },
            "16.0",
            &census("0.6", "16.0", false),
            "d",
            &host(128),
        )
        .unwrap();
        let out = memory_rewritten(
            text,
            &[("aterm-cli".into(), learned), ("beta".into(), beta)],
        );
        assert_eq!(
            out,
            "# h\naterm-cli\t4.0\t8\t4.0\t2.1\t=\tA18\naterm-cli\t4.0\t24\t3.0\t3.0\t>\t2026-10-01\nbeta\t16.0\t128\t16.0\t0.6\t=\td\n"
        );
        assert!(parse_memory(&out, "R").is_ok());
    }

    /// A RECORD CARRIES THE ROW THE RUN READ, with the RAM its need was measured
    /// on (facts 4).
    #[test]
    fn the_facts_carry_each_rows_ram_and_run_ceiling() {
        let mut f = facts(vec![("aterm-cli", Decision::Memory(None))]);
        f.crates[0].memory = mem("24", "4.0", "3.0", "=");
        f.host = host(24);
        let text = f.to_record();
        assert!(
            text.contains("\ncrate aterm-cli\tmemory\t16.0\t24\t4.0\t3.0\t=\tm\t-\t-\t-\t-\n"),
            "{text}"
        );
        assert_eq!(Facts::parse(&text), Some(f.clone()));
        assert_eq!(
            Facts::parse(&text.replace(FACTS_MAGIC, "aterm-trust facts 4")),
            None
        );
        // The judge narrows the run around it.
        let v = judge(
            &f,
            Some(&format!("aterm-cli\t75\t443\t{DRV}\n")),
            "R",
            "M",
            Update::NONE,
        );
        assert_eq!(v.word, "NARROWED");
    }

    /// A REAL REFUSAL (2026-10-17, seal 41d79979, umask 0002): the whole log of
    /// `targo trust check -p aterm-dirfd --lib --allow-l0-gaps`, exit 2, run with
    /// `CARGO_TARGET_DIR` under a 0775 directory of the caller's (the home renamed
    /// `/home/u`). Its two `verified tool launcher … carries CALLER-OWNED
    /// (unsealed) pathname authority` warnings are no refusal.
    const REFUSED_LOG: &str = include_str!("../tests/fixtures/trust/refused-path-umask-002.log");

    /// The path [`REFUSED_LOG`] refused.
    const REFUSED_DIR: &str = "/home/u/.cache/aterm-judge-fixture.wGWg0z/g";

    /// The same check with `CARGO_TARGET_DIR` under `/tmp` (real, the same day):
    /// root's sticky directory, which targo refuses all the same.
    const REFUSED_TMP: &str = "error: verified runtime-library directory path component `/tmp` is group- or world-writable (uid=0, mode=0o1777); any local principal could substitute the executed object between authorization and execution\n\
                               targo trust: error: verification did not run: no per-function TRUST_JSON transport row, authenticated coverage summary, or terminal Cargo target inventory was collected\n";

    /// trustd's refusal of its executable's ancestor, inside targo-trust's
    /// wrapper (`pipeline/run.rs`; trust-router `coordinator.rs`), as targo prints it.
    const REFUSED_TRUSTD: &str = "targo trust: error: could not establish the selected same-sysroot trustd memory authority at /tmp/trustd-runtime-locks-1000/trustd.sock: could not authorize the same-sysroot trustd executable /home/u/tc/bin/trustd: trustd executable ancestor is foreign-owned or cross-user writable: /home/u/tc; refusing to launch a split-authority Cargo fan-out. Confirm the sibling trustd at `/home/u/tc/bin/trustd` is installed/executable and stop any incompatible live daemon.\n";

    fn writable(owner: Option<u32>, mode: u32) -> Refused {
        Refused::Writable {
            owner,
            mode: Some(mode),
            link: None,
        }
    }

    fn refusal(path: &str, why: Refused) -> PathRefusal {
        PathRefusal {
            path: path.into(),
            why,
        }
    }

    /// A PATH TARGO REFUSED is read in each of targo's and trustd's words, from a
    /// real log and wherever a line carries them (cargo's `error:` and `Caused
    /// by:`, a JSON string, ANSI colours), once per path; nothing else reads as one.
    #[test]
    fn a_refused_path_is_read_in_targos_own_words() {
        let m = Measured::of_log(REFUSED_LOG.as_bytes(), "2");
        assert!(m.transport && m.summary.is_empty(), "{m:?}");
        assert_eq!(
            m.path_refused,
            [refusal(REFUSED_DIR, writable(Some(1000), 0o775))]
        );
        // The real log's lines before its refusal, each case twice, and its end.
        let (head, _) = REFUSED_LOG
            .split_once("error: verified runtime-library directory")
            .unwrap();
        let foreign = Refused::Foreign {
            owner: 501,
            me: 1000,
        };
        let cases = [
            (REFUSED_TMP.to_string(), refusal("/tmp", writable(Some(0), 0o1777))),
            (
                "error: failed to run custom build command for `x`\n\nCaused by:\n  verified tool launcher path component `/opt/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); an execution object a foreign principal owns can be replaced between authorization and execution\n".into(),
                refusal("/opt/trust", foreign.clone()),
            ),
            (
                "error: verified runtime-library directory path component `/srv/shared` remains writable by Targo's effective identity (uid=1000); ACL-backed write authority is incompatible with verified pathname execution\n".into(),
                refusal("/srv/shared", Refused::Acl { me: 1000 }),
            ),
            (
                "error: the directory `/home/u/.rustup/toolchains` holding protected Trust launch path symlink `/home/u/.rustup/toolchains/trust` is group- or world-writable without the sticky bit (mode=0o775); any local principal could repoint the symlink at another toolchain\n".into(),
                refusal(
                    "/home/u/.rustup/toolchains",
                    Refused::Writable {
                        owner: None,
                        mode: Some(0o775),
                        link: Some("/home/u/.rustup/toolchains/trust".into()),
                    },
                ),
            ),
            (
                "error: the directory `/opt/links` holding protected Trust launch path symlink `/opt/links/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); its owner can repoint the symlink at another toolchain\n".into(),
                refusal("/opt/links", foreign.clone()),
            ),
            (
                "error: protected Trust launch path symlink `/opt/links/trust` is owned by uid=501, which is neither root nor the invoking identity (uid=1000); a redirection a foreign principal owns can be repointed at another toolchain\n".into(),
                refusal("/opt/links/trust", foreign),
            ),
            (
                REFUSED_TRUSTD.into(),
                refusal(
                    "/home/u/tc",
                    Refused::Ancestor {
                        authority: "trustd executable".into(),
                    },
                ),
            ),
            (
                "trustd: trustd runtime ancestor is foreign-owned or cross-user writable: /tmp\n".into(),
                refusal(
                    "/tmp",
                    Refused::Ancestor {
                        authority: "trustd runtime".into(),
                    },
                ),
            ),
            (
                "{\"reason\":\"compiler-message\",\"message\":{\"rendered\":\"error: verified runtime-library candidate path component `/home/u/work/target-trust/j0/debug/deps/libaterm_x-1.rlib` is group- or world-writable (uid=1000, mode=0o664); any local principal could substitute the executed object between authorization and execution\\n\",\"level\":\"error\"}}\n".into(),
                refusal(
                    "/home/u/work/target-trust/j0/debug/deps/libaterm_x-1.rlib",
                    writable(Some(1000), 0o664),
                ),
            ),
            (
                "\x1b[1m\x1b[91merror\x1b[0m\x1b[1m:\x1b[0m verified tool launcher path component `\x1b[1m/opt/trust/bin\x1b[0m` is group- or world-writable (uid=0, mode=0o775); any local principal could substitute the executed object between authorization and execution\n".into(),
                refusal("/opt/trust/bin", writable(Some(0), 0o775)),
            ),
            (
                // `process_authority.rs`'s own symlink hop, on the way to a
                // runtime-library directory (a --target-root through another's
                // link) or a launcher.
                "error: verified runtime-library directory launch path symlink `/home/u/shared/troot` is owned by uid=501, which is not permitted to own a redirection into the verified toolchain (invoking uid=1000); its owner can repoint it at another sysroot\n".into(),
                refusal(
                    "/home/u/shared/troot",
                    Refused::Foreign {
                        owner: 501,
                        me: 1000,
                    },
                ),
            ),
            (
                // trustd's demands, each inside the targo-trust wrapper that
                // carries it (`pipeline/run.rs`; trust-router `coordinator.rs`).
                format!("targo trust: error: {TRUSTD_REFUSED} at /tmp/trustd-runtime-locks-1000/trustd.sock: socket parent must be euid-owned/non-writable or root-owned/sticky: /tmp/trustd-runtime-locks-1000; refusing to launch a split-authority Cargo fan-out.\n"),
                refusal(
                    "/tmp/trustd-runtime-locks-1000",
                    Refused::Demand {
                        demand: "socket parent must be euid-owned/non-writable or root-owned/sticky".into(),
                    },
                ),
            ),
            (
                "targo trust: error: could not establish the private per-user trustd runtime directory: trustd runtime root must be an euid-owned 0700 directory: /tmp/trustd-runtime-locks-1000\n".into(),
                refusal(
                    "/tmp/trustd-runtime-locks-1000",
                    Refused::Demand {
                        demand: "trustd runtime root must be an euid-owned 0700 directory".into(),
                    },
                ),
            ),
            (
                format!("targo trust: error: {TRUSTD_REFUSED} at /tmp/trustd-runtime-locks-1000/trustd.sock: could not authorize the same-sysroot trustd executable /home/u/tc/bin/trustd: trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits: /home/u/tc/bin/trustd; refusing to launch a split-authority Cargo fan-out.\n"),
                refusal(
                    "/home/u/tc/bin/trustd",
                    Refused::Demand {
                        demand: "trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits".into(),
                    },
                ),
            ),
        ];
        for (text, want) in cases {
            let log = format!("{head}{text}{text}targo trust: error: verification did not run\n");
            let m = Measured::of_log(log.as_bytes(), "2");
            assert_eq!(m.path_refused, [want], "{text}");
        }
        // trustd's keeps the check a launch refusal, as before.
        let m = Measured::of_log(REFUSED_TRUSTD.as_bytes(), "2");
        assert_eq!(m.launch, "other");
        // Nothing else is one: the real log's head (the launcher warnings), a
        // finished check, a sealed-launcher demand the caller opted into (and a
        // hop's words for the caller's own link under it), and targo failing to
        // inspect a path or finding a non-directory.
        for text in [
            head,
            DONE,
            "error: verified tool launcher path component `/opt/trust/bin` is owned by uid=501, but TRUST_REQUIRE_SEALED_LAUNCHER demands a root-owned installation for a sealed-release provenance claim\n",
            "error: verified tool launcher launch path symlink `/home/u/tc` is owned by uid=1000, which is not permitted to own a redirection into the verified toolchain (invoking uid=1000); its owner can repoint it at another sysroot\n",
            "error: cannot inspect the directory `/x` holding protected Trust launch path symlink `/x/t`: No such file or directory (os error 2)\n",
            "error: the directory `/x` holding verified tool launcher launch path symlink `/x/t` is not a plain directory\n",
            "error: failed to inspect verified tool launcher path component `/x`\n",
        ] {
            let m = Measured::of_log(text.as_bytes(), "1");
            assert!(m.path_refused.is_empty(), "{text}: {:?}", m.path_refused);
        }
    }

    /// One `REFUSED PATH:` line per path, naming every library it failed (one bad
    /// directory fails every check) and what to do; each row keeps the status it
    /// had, and the verdict's own line says it too.
    #[test]
    fn a_refused_path_is_said_once_with_every_library_it_failed() {
        let me = aterm_uds::peer::our_uid();
        let mine = REFUSED_LOG.replace("uid=1000", &format!("uid={me}"));
        let gated = |text: &str| Decision::Gate(Measured::of_log(text.as_bytes(), "2"));
        let ratchet = format!("eps\t5\t21\t{DRV}\n");
        let v = judge(
            &facts(vec![
                ("alpha", gated(&mine)),
                ("beta", gated(&mine)),
                ("gamma", gated(REFUSED_TMP)),
                ("delta", gated(REFUSED_TRUSTD)),
                ("eps", gate(DONE)),
            ]),
            Some(&ratchet),
            "R",
            "M",
            Update::NONE,
        );
        let said: Vec<&str> = v
            .report
            .lines()
            .filter(|l| l.starts_with("REFUSED PATH:"))
            .collect();
        assert_eq!(said.len(), 3, "{}", v.report);
        // Each path's line, by the libraries it names.
        let line = |libs: &str| {
            said.iter()
                .copied()
                .find(|l| l.starts_with(&format!("REFUSED PATH: {libs}: ")))
                .unwrap_or_else(|| panic!("no line for {libs}: {said:#?}"))
        };
        let both = line("alpha beta");
        assert!(
            both.contains(REFUSED_DIR)
                && both.contains("mode 775")
                && both.ends_with(&format!("run: chmod go-w {REFUSED_DIR}")),
            "{both}"
        );
        // Root's sticky `/tmp` is no `chmod` of the caller's: it must move.
        let tmp = line("gamma");
        assert!(
            tmp.contains("/tmp")
                && tmp.contains("mode 1777")
                && !tmp.contains("chmod")
                && tmp.ends_with(REFUSED_MOVE),
            "{tmp}"
        );
        // trustd's names no owner: both.
        let trustd = line("delta");
        assert!(
            trustd.contains("chmod go-w /home/u/tc") && trustd.ends_with(REFUSED_MOVE),
            "{trustd}"
        );
        // The rows are what they were.
        assert_eq!(
            v.failed,
            [
                "alpha NOT-VERIFIED(rc=2)",
                "beta NOT-VERIFIED(rc=2)",
                "gamma NOT-VERIFIED(rc=2)"
            ]
        );
        assert!(
            v.report
                .lines()
                .any(|l| l.starts_with("delta ") && l.contains("NOT-VERIFIED(trustd)")),
            "{}",
            v.report
        );
        assert_eq!(v.unverified, ["delta"]);
        assert!(
            v.tail.starts_with(
                "VERDICT: FAIL — alpha NOT-VERIFIED(rc=2); beta NOT-VERIFIED(rc=2); gamma NOT-VERIFIED(rc=2)"
            ) && v.tail.contains("REFUSED PATH")
                && v.tail.contains(&format!("run: chmod go-w {REFUSED_DIR}")),
            "{}",
            v.tail
        );
        // The control: a run no path was refused in says nothing of one.
        let clean = judge(
            &facts(vec![("eps", gate(DONE))]),
            Some(&ratchet),
            "R",
            "M",
            Update::NONE,
        );
        assert!(
            !clean.report.contains("REFUSED PATH") && !clean.tail.contains("REFUSED PATH"),
            "{}",
            clean.report
        );
    }

    /// What to do about a refused path turns on whose it is: `chmod go-w` only a
    /// path the caller owns, never a shared sticky directory; one that is
    /// another's, foreign-owned or writable through an ACL must hold nothing targo
    /// checks; a message that names no owner gets both. trustd's runtime
    /// directory, whose path is fixed, is never sent to move.
    #[test]
    fn a_refused_paths_remedy_is_chmod_only_for_the_callers_own() {
        let me = 1000;
        let at = |why: Refused| refusal("/home/u/a b", why);
        // The command, quoted for the shell.
        let chmod = "chmod go-w '/home/u/a b'";
        for (said, who) in [
            (at(writable(Some(me), 0o775)).remedy(me), "the caller's"),
            (
                at(writable(Some(0), 0o775)).remedy(0),
                "root's own, to root",
            ),
        ] {
            assert!(
                said.ends_with(&format!("run: {chmod}")) && !said.contains(REFUSED_MOVE),
                "{who}: {said}"
            );
        }
        for why in [
            writable(Some(0), 0o775),
            writable(Some(0), 0o1777),
            writable(Some(me), 0o1777),
            Refused::Foreign { owner: 501, me },
            Refused::Acl { me },
        ] {
            let said = at(why.clone()).remedy(me);
            assert!(
                !said.contains("chmod") && said.ends_with(REFUSED_MOVE),
                "{why:?}: {said}"
            );
        }
        for why in [
            Refused::Writable {
                owner: None,
                mode: Some(0o775),
                link: Some("/home/u/a b/trust".into()),
            },
            Refused::Ancestor {
                authority: "trustd executable".into(),
            },
            Refused::Demand {
                demand: "trustd executable must be same-user (or immutable root-owned system), regular, executable, and have no group/other write bits".into(),
            },
        ] {
            let said = at(why.clone()).remedy(me);
            assert!(
                said.contains(&format!("run: {chmod}")) && said.ends_with(REFUSED_MOVE),
                "{why:?}: {said}"
            );
        }
        // trustd's runtime directory: chmod'ed to what trustd demands, never moved.
        for (demand, chmod) in [
            (
                "trustd runtime root must be an euid-owned 0700 directory",
                "chmod 700 '/home/u/a b'",
            ),
            (
                "socket parent must be euid-owned/non-writable or root-owned/sticky",
                chmod,
            ),
        ] {
            let said = at(Refused::Demand {
                demand: demand.into(),
            })
            .remedy(me);
            assert!(
                said.contains(&format!("run: {chmod}")) && !said.contains(REFUSED_MOVE),
                "{demand}: {said}"
            );
        }
    }

    /// A RUN TARGO REFUSED A PATH IN FILES NOTHING: what it measured beside the
    /// refusal is no fact about the code, so no record keyed by the code carries
    /// it. The control, the same run with the check whole, files.
    #[test]
    fn a_run_targo_refused_a_path_in_files_nothing() {
        let w = World::new("refused-path");
        let a = w.prover("a");
        let head = w.commit(&[("src/lib.rs", "v1"), (RATCHET, &floor(3))]);
        let logs = w.root.join("logs");
        let slots = w.root.join("troot");
        fs::create_dir_all(slots.join("j0/debug/deps")).unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            slots.join("j0/debug/deps/alpha-1.d"),
            "alpha.rmeta: crates/alpha/src/lib.rs\n",
        )
        .unwrap();
        fs::write(logs.join(".trustd-grants"), "1\n").unwrap();
        let plan = format!(
            "{PLAN_MAGIC}\ndriver {}/targo\ndriver-id {DRV}\ntrustd /tmp/t.sock\nonly \n\
             logdir {}\nratchet {}\ncrate alpha\talpha@1.0.0\tgate\n\
             store {}\ncommit {head}\ntree x\nprover {}\nprover-bin {}\ntarget-root {}\n\
             lane-pin SOURCE_DATE_EPOCH=0\nlane-passed PATH\nlane-passed HOME\nlane-passed CARGO_TARGET_DIR\nfile \n",
            a.bin.display(),
            logs.display(),
            w.work().join(RATCHET).display(),
            w.store().display(),
            a.identity.as_ref().unwrap(),
            a.bin.display(),
            slots.display(),
        );
        let run = |log: &str, rc: &str| {
            fs::write(logs.join("alpha@1.0.0.log"), log).unwrap();
            fs::write(logs.join("alpha@1.0.0.log.rc"), rc).unwrap();
            let mut p = Plan::parse(&plan).unwrap();
            conclude(&w.cli(), &w.work(), &mut p, Update::NONE).unwrap()
        };
        let done = run(REFUSED_LOG, "2\n");
        let l = done.facts_line.unwrap_or_default();
        assert!(
            l.starts_with("FACTS: none") && l.contains(REFUSED_DIR) && l.contains("alpha"),
            "{l}"
        );
        assert!(done.verdict.report.contains("REFUSED PATH: alpha: "));
        assert!(w.found(&a, "HEAD").is_err(), "nothing was filed");
        // The control: the check whole, the record filed and found.
        let done = run(
            "  Summary: 3 proved, 0 failed (3 total)\nCoverage: 2/2 eligible function bodies verified (complete)\n",
            "0\n",
        );
        let l = done.facts_line.unwrap_or_default();
        assert!(l.starts_with("FACTS: ") && !l.contains("none"), "{l}");
        assert!(w.found(&a, "HEAD").is_ok(), "the control is found");
    }
}
