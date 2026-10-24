// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-search`: the candidate search under the pinned acceptance policy, in process, and the
//! offline verifier of the bundles it writes.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_aos::Digest;
use aterm_aos::wire::WireFault;
use aterm_effects::aos_momentum::bundle::{
    BUNDLE_DOMAIN, BUNDLE_MAGIC, BundleField, BundleRefusal, CandidateBundle, UncheckedBundle,
};
use aterm_effects::aos_momentum::m4::{Compared, Evaluator, NoOutcome};
use aterm_effects::aos_momentum::policy::{MAX_DISTINCT, MAX_ROUNDS, PolicyRefusal, Totals};
use aterm_effects::aos_momentum::search::{
    FileProposer, GridProposer, Mode, Proposer, ProposerId, SearchStats, TrainFeedback,
};

use crate::instance::{take_flag, take_value};
use crate::paths::{from_caller, paths};
use crate::{Fail, Res};

/// The verb's own help (`aos-search --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-search [--grid] [--plans FILE] --out DIR [--toolchain LINE]
       targo --unverified lab aos-search --verify FILE

Aos M4's candidate search, in process (no aterm runs, no window opens): the M4 evaluator under the pinned policy
momentum-glow.m4-acceptance v1 judges each named proposer's candidates, selects on the training split, reads the
held-out split once for the training winner, and writes each winner's provenance bundle (verified again by
re-evaluating every row) and a summary: proposed, distinct, refused by reason and admitted; the winner and its training
and held-out T and Z against the pinned baseline, S0's, usefulness and gain; and, labelled MEASURED (never scored),
the wall time and the events the evaluator stepped. One search per proposer, grid first.

  --grid            the grid proposer: the policy's 54 plans
  --plans FILE      the file proposer over FILE: one hex plan (24 digits) per line, '#' comments
  --out DIR         where <proposer>.bundle and summary.txt go, OUTSIDE the repository (required)
  --toolchain LINE  the toolchain line each bundle declares, e.g. \"$(targo --unverified --version)\" (default: this
                    lab's build string)
  --verify FILE     no search: decode the bundle FILE and verify it (CandidateBundle::verify, every row re-evaluated)
                    under an evaluator of this lab's build; a bundle of another build is refused ForeignBuild, and one
                    of a dirty build DirtyBuild, before any evaluator is built. One line per check (pass, fail, not-run,
                    none or declared), then `aos-search verify=pass|fail reason=...`

exit: 0 every search had a winner and every bundle verified and was written; 1 this lab's build names no commit, a
search had no outcome, a bundle did not verify or a file could not be written; 2 usage
exit (--verify): 0 the bundle verified; 1 it was refused (the reason named); 2 usage, or FILE cannot be read
";

/// The build the lab's evaluator judges in and its bundles name.
pub const LAB_BUILD: &str = concat!(
    "aterm-lab ",
    env!("CARGO_PKG_VERSION"),
    "+g",
    env!("ATERM_LAB_GIT_COMMIT")
);

/// Refuses a build that names no commit.
/// # Errors
/// Exit 1, naming the build.
pub fn check_build(build: &str) -> Result<(), Fail> {
    let commit = build.rsplit_once("+g").map_or("", |(_, commit)| commit);
    let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
    if hash.len() >= 12 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(Fail::die(format!(
            "aos-search: the build `{build}` names no commit (built without git; pin one with \
             ATERM_BUILD_GIT_COMMIT), so its bundles could not name the interpreter that \
             judged them"
        )))
    }
}

/// The label of the wall time a bundle records for its search.
pub const SEARCH_TIMING: &str = "aos-search: select and held-out, wall time";

/// The label of the wall time a bundle records for building the evaluator.
pub const EVALUATOR_TIMING: &str = "aos-search: Evaluator::v1 (the baseline measured), wall time";

/// What the verb was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Run the grid proposer.
    pub grid: bool,
    /// The file proposer's file, as given.
    pub plans: Option<String>,
    /// The output directory, as given.
    pub out: String,
    /// The toolchain line to declare.
    pub toolchain: String,
}

/// The verb's arguments. Pure.
/// # Errors
/// A usage error: an unknown word, a flag without its value, no proposer, no `--out`.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let grid = take_flag(&mut a, "--grid");
    let plans = take_value(&mut a, "--plans")?;
    let out = take_value(&mut a, "--out")?;
    let toolchain = take_value(&mut a, "--toolchain")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "aos-search: unexpected '{extra}' (aos-search --help)"
        )));
    }
    if !grid && plans.is_none() {
        return Err(Fail::usage(
            "aos-search: name a proposer: --grid, --plans FILE, or both (aos-search --help)",
        ));
    }
    let out = out.ok_or_else(|| Fail::usage("aos-search: --out DIR is required"))?;
    let toolchain = toolchain.unwrap_or_else(|| LAB_BUILD.to_owned());
    if toolchain.trim().is_empty() {
        return Err(Fail::usage(
            "aos-search: --toolchain needs a non-empty line",
        ));
    }
    Ok(Args {
        grid,
        plans,
        out,
        toolchain,
    })
}

struct Recorded<P> {
    inner: P,
    returned: u64,
    shown: Vec<TrainFeedback>,
}

impl<P: Proposer> Proposer for Recorded<P> {
    fn id(&self) -> ProposerId {
        self.inner.id()
    }

    fn propose(&mut self, round: u8, feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
        self.shown = feedback.to_vec();
        let proposed = self.inner.propose(round, feedback);
        self.returned = self
            .returned
            .saturating_add(u64::try_from(proposed.len()).unwrap_or(u64::MAX));
        proposed
    }
}

struct Searched {
    /// The proposer's short name (the bundle's file stem).
    name: &'static str,
    id: ProposerId,
    /// What the evaluator counted, when the outcome carries it.
    stats: Option<SearchStats>,
    /// The proposals the proposer returned.
    returned: u64,
    /// The last feedback it was shown.
    shown: Vec<TrainFeedback>,
    /// The winner's summary lines, or why there is no outcome.
    result: Result<Vec<String>, String>,
    wall: Duration,
    stepped: u64,
}

fn vs(c: &Compared) -> String {
    format!(
        "T={} Z={} baseline T={} Z={}",
        c.candidate.ticks, c.candidate.wasted, c.baseline.ticks, c.baseline.wasted
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn search<P: Proposer>(
    ev: &Evaluator,
    name: &'static str,
    proposer: P,
    toolchain: &str,
    built: Duration,
    out: &Path,
) -> Searched {
    let mut rec = Recorded {
        inner: proposer,
        returned: 0,
        shown: Vec::new(),
    };
    let id = rec.id();
    let stepped_before = ev.stepped();
    let started = Instant::now();
    let selected = ev.select(&mut rec);
    let (stats, outcome) = match selected {
        Ok(selection) => {
            let stats = selection.stats();
            (Some(stats), ev.held_out(selection))
        }
        Err(no) => {
            let stats = match &no {
                NoOutcome::NoneAdmitted { stats } | NoOutcome::NoGain { stats, .. } => Some(*stats),
                NoOutcome::HeldOut { .. } | NoOutcome::Foreign { .. } => None,
            };
            (stats, Err(no))
        }
    };
    let wall = started.elapsed();
    let stepped = ev.stepped().saturating_sub(stepped_before);
    let result = outcome
        .map_err(|no| no.to_string())
        .and_then(|mut admitted| {
            admitted.declare_toolchain(toolchain);
            let timed = admitted.record_timing(SEARCH_TIMING, wall)
                && admitted.record_timing(EVALUATOR_TIMING, built);
            if !timed {
                return Err("the bundle refused a lab timing".to_owned());
            }
            let plan = admitted.plan();
            let gain = admitted.gain();
            let useful = admitted.useful();
            let bundle = admitted.into_bundle();
            let bytes = bundle.encode();
            let verified = CandidateBundle::verify(&bytes, ev)
                .map_err(|r| format!("the winner's bundle does not verify: {r}"))?;
            if verified != bundle {
                return Err("the verified bundle is not the one written".to_owned());
            }
            let file = format!("{name}.bundle");
            std::fs::write(out.join(&file), &bytes)
                .map_err(|e| format!("cannot write {}: {e}", out.join(&file).display()))?;
            let parts = bundle.parts();
            Ok(vec![
                format!(
                    "winner plan={} digest={} round={} index={} useful={useful}",
                    hex(&plan.encode()),
                    plan.digest().short(),
                    parts.round,
                    parts.index
                ),
                format!(
                    "selection provenance={}",
                    verified.selection_provenance().name()
                ),
                format!("train {}", vs(&parts.selection.train)),
                format!("held_out {} gain_ppm={}", vs(&gain.held_out), gain.ppm()),
                format!("s0 {} gain_ppm={}", vs(&gain.s0), gain.s0_ppm()),
                format!(
                    "bundle file={file} bytes={} digest={} verified=true build={} toolchain={}",
                    bytes.len(),
                    bundle.digest().short(),
                    parts.build.replace(' ', "_"),
                    parts.toolchain.replace(' ', "_")
                ),
            ])
        });
    Searched {
        name,
        id,
        stats,
        returned: rec.returned,
        shown: rec.shown,
        result,
        wall,
        stepped,
    }
}

fn summary_of(s: &Searched, out: &mut String) {
    let _ = writeln!(
        out,
        "search proposer={} {} v{} config={}",
        s.name,
        s.id.name,
        s.id.version,
        s.id.config.short()
    );
    let mut refused: BTreeMap<&str, u64> = BTreeMap::new();
    for f in s.shown.iter().filter(|f| !f.is_admitted()) {
        *refused.entry(f.law().unwrap_or("unnamed")).or_insert(0) += 1;
    }
    let shown_admitted = s.shown.iter().filter(|f| f.is_admitted()).count();
    match s.stats {
        Some(st) => {
            let _ = writeln!(
                out,
                "  counts proposed={} distinct={} admitted={} oversized={} duplicates={} \
                 over_budget={} rounds={}",
                st.proposed,
                st.distinct,
                st.admitted,
                st.oversized,
                st.duplicates,
                st.over_budget,
                st.rounds
            );
            if st.oversized > 0 {
                refused.insert("Budget", st.oversized);
            }
            let unreported = st
                .distinct
                .saturating_sub(u64::try_from(s.shown.len()).unwrap_or(u64::MAX));
            let words: Vec<String> = refused.iter().map(|(k, n)| format!("{k}={n}")).collect();
            let _ = writeln!(
                out,
                "  refused {} unreported={unreported}",
                if words.is_empty() {
                    "none".to_owned()
                } else {
                    words.join(" ")
                }
            );
        }
        None => {
            let words: Vec<String> = refused.iter().map(|(k, n)| format!("{k}={n}")).collect();
            let _ = writeln!(
                out,
                "  counts returned={} (the evaluator's counts are not returned with this outcome) \
                 shown={} shown_admitted={shown_admitted}",
                s.returned,
                s.shown.len()
            );
            let _ = writeln!(
                out,
                "  refused {}",
                if words.is_empty() {
                    "none".to_owned()
                } else {
                    words.join(" ")
                }
            );
        }
    }
    match &s.result {
        Ok(lines) => {
            for line in lines {
                let _ = writeln!(out, "  {line}");
            }
        }
        Err(why) => {
            let _ = writeln!(out, "  no-outcome {why}");
        }
    }
    let _ = writeln!(
        out,
        "  MEASURED (never scored) wall_ms={:.1} stepped={}",
        s.wall.as_secs_f64() * 1e3,
        s.stepped
    );
}

/// The searches `args` names.
/// # Errors
/// This lab's build names no commit (exit 1, before any evaluator is built), the pinned policy
/// refused (exit 1), or the summary could not be written.
pub fn run(args: &Args, plans_text: Option<&str>, out: &Path) -> Result<(i32, String), Fail> {
    run_in(args, plans_text, out, LAB_BUILD)
}

fn run_in(
    args: &Args,
    plans_text: Option<&str>,
    out: &Path,
    build: &str,
) -> Result<(i32, String), Fail> {
    check_build(build)?;
    let started = Instant::now();
    let ev = Evaluator::v1(build, started)
        .map_err(|e| Fail::die(format!("aos-search: the pinned policy is refused: {e}")))?;
    let built = started.elapsed();
    let mut searched = Vec::new();
    if args.grid {
        searched.push(search(
            &ev,
            "grid",
            GridProposer::new(),
            &args.toolchain,
            built,
            out,
        ));
    }
    if let Some(text) = plans_text {
        searched.push(search(
            &ev,
            "plans",
            FileProposer::from_text(text),
            &args.toolchain,
            built,
            out,
        ));
    }
    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "aos-search policy={} build={} toolchain={}",
        ev.policy().id().digest.short(),
        build.replace(' ', "_"),
        args.toolchain.replace(' ', "_")
    );
    let _ = writeln!(
        summary,
        "MEASURED (never scored) evaluator_build_ms={:.1}",
        built.as_secs_f64() * 1e3
    );
    for s in &searched {
        summary_of(s, &mut summary);
    }
    let winners = searched.iter().filter(|s| s.result.is_ok()).count();
    let pass = winners == searched.len();
    let _ = writeln!(
        summary,
        "aos-search result={} searches={} winners={winners}",
        if pass { "pass" } else { "fail" },
        searched.len()
    );
    std::fs::write(out.join("summary.txt"), &summary).map_err(|e| {
        Fail::die(format!(
            "aos-search: cannot write {}: {e}",
            out.join("summary.txt").display()
        ))
    })?;
    Ok((i32::from(!pass), summary))
}

/// `--verify FILE`'s file, or `None` when `args` do not ask for it. Pure.
/// # Errors
/// A usage error: `--verify` without its value, or any other argument beside it (it runs no
/// search).
pub fn parse_verify(args: &[String]) -> Result<Option<String>, Fail> {
    let mut a = args.to_vec();
    let Some(file) = take_value(&mut a, "--verify")? else {
        return Ok(None);
    };
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "aos-search: --verify FILE runs no search and takes no other argument; unexpected \
             '{extra}' (aos-search --help)"
        )));
    }
    Ok(Some(file))
}

/// A check `--verify` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    /// `UncheckedBundle::decode`: the magic, the format, the seal (before any part is read), every
    /// part, nothing trailing.
    Decode,
    /// The bundle names exactly this lab's build, and that build names one clean commit.
    Build,
    /// `Evaluator::v1` of that build: the pinned policy, its corpus and its baseline.
    Evaluator,
    /// The policy, build, base, contracts, interpreter, lane model, oracle, corpus, schema,
    /// profile, grade and assumption lists.
    Identities,
    /// The plan decodes, and the candidate is that plan's code in the bundle's build.
    Plan,
    /// The proposer, round and index are consistent with the mode.
    Proposer,
    /// One row per policy item, in order, each passing, re-evaluated and compared field by field.
    Rows,
    /// The counts, the one held-out look, the sums, the usefulness and the gains.
    Selection,
    /// A search credited to the grid is the grid's own selection, re-run on the training split.
    Grid,
    /// The timings are labelled.
    Timings,
    /// The publication part, if there is one, is consistent with the rest.
    Publication,
}

impl Check {
    /// Every check, in the order they run.
    pub const ALL: [Check; 11] = [
        Check::Decode,
        Check::Build,
        Check::Evaluator,
        Check::Identities,
        Check::Plan,
        Check::Proposer,
        Check::Rows,
        Check::Selection,
        Check::Grid,
        Check::Timings,
        Check::Publication,
    ];

    /// Its name in a report line (`  check <name>=<status> …`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Check::Decode => "decode",
            Check::Build => "build",
            Check::Evaluator => "evaluator",
            Check::Identities => "identities",
            Check::Plan => "plan",
            Check::Proposer => "proposer",
            Check::Rows => "rows",
            Check::Selection => "selection",
            Check::Grid => "grid",
            Check::Timings => "timings",
            Check::Publication => "publication",
        }
    }
}

/// Why `--verify` refused a bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyRefusal {
    /// `UncheckedBundle::decode` or `CandidateBundle::verify` refused it.
    Bundle(BundleRefusal),
    /// It names another build than this lab's.
    ForeignBuild {
        /// The build the bundle names.
        bundle: String,
    },
    /// The build it names (this lab's) ends `-dirty`.
    DirtyBuild,
    /// The build it names (this lab's) names no commit, a name every unstamped build shares.
    UnstampedBuild,
    /// No evaluator of the build could be built.
    Policy(PolicyRefusal),
}

impl VerifyRefusal {
    fn check(&self, decoded: Option<&UncheckedBundle>) -> Check {
        match (self, decoded) {
            (VerifyRefusal::Bundle(r), Some(b)) => verify_step(r, b),
            (VerifyRefusal::Bundle(_), None) => Check::Decode,
            (
                VerifyRefusal::ForeignBuild { .. }
                | VerifyRefusal::DirtyBuild
                | VerifyRefusal::UnstampedBuild,
                _,
            ) => Check::Build,
            (VerifyRefusal::Policy(_), _) => Check::Evaluator,
        }
    }

    /// Its token in `aos-search verify=fail reason=<token>`.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            VerifyRefusal::Bundle(BundleRefusal::Magic) => "Magic".to_owned(),
            VerifyRefusal::Bundle(BundleRefusal::Format { found }) => {
                format!("Format{{found={found}}}")
            }
            VerifyRefusal::Bundle(BundleRefusal::Seal { .. }) => "Seal".to_owned(),
            VerifyRefusal::Bundle(BundleRefusal::Wire(w)) => format!("Wire{{{}}}", wire_reason(w)),
            VerifyRefusal::Bundle(BundleRefusal::Forged { field, row: None }) => {
                format!("Forged{{field={field:?}}}")
            }
            VerifyRefusal::Bundle(BundleRefusal::Forged {
                field,
                row: Some(item),
            }) => format!("Forged{{field={field:?},row={}}}", word(item)),
            VerifyRefusal::ForeignBuild { .. } => "ForeignBuild".to_owned(),
            VerifyRefusal::DirtyBuild => "DirtyBuild".to_owned(),
            VerifyRefusal::UnstampedBuild => "UnstampedBuild".to_owned(),
            VerifyRefusal::Policy(PolicyRefusal::PolicyMutated { .. }) => {
                "PolicyMutated".to_owned()
            }
            VerifyRefusal::Policy(PolicyRefusal::CorpusDrift { item }) => {
                format!("CorpusDrift{{item={}}}", word(item))
            }
            VerifyRefusal::Policy(PolicyRefusal::BaselineDrift { item, field }) => {
                format!("BaselineDrift{{item={},field={field:?}}}", word(item))
            }
        }
    }

    fn sentence(&self, lab: &str) -> String {
        match self {
            VerifyRefusal::Bundle(BundleRefusal::Forged { field, row: None }) => format!(
                "the bundle is forged: its {} is not what the pinned policy computes",
                field.name()
            ),
            VerifyRefusal::Bundle(BundleRefusal::Forged {
                field,
                row: Some(item),
            }) => format!(
                "the bundle is forged: the {} of the row for {} is not what the pinned policy \
                 computes",
                field.name(),
                word(item)
            ),
            VerifyRefusal::Bundle(r) => r.to_string(),
            VerifyRefusal::ForeignBuild { bundle } => format!(
                "the bundle names the build {}, not this lab's {}: another interpreter judged \
                 it, so it is not re-evaluated here",
                word(bundle),
                word(lab)
            ),
            VerifyRefusal::DirtyBuild => format!(
                "the bundle names this lab's build {}, a dirty tree's, which every dirty tree of \
                 that commit shares: it names no one interpreter",
                word(lab)
            ),
            VerifyRefusal::UnstampedBuild => format!(
                "the bundle names this lab's build {}, which names no commit: every unstamped \
                 build shares that name",
                word(lab)
            ),
            VerifyRefusal::Policy(p) => format!("no evaluator of this build: {p}"),
        }
    }
}

fn wire_reason(w: &WireFault) -> String {
    match *w {
        WireFault::Truncated { field, .. } => format!("Truncated,field={field}"),
        WireFault::Trailing { extra } => format!("Trailing,extra={extra}"),
        WireFault::BadTag { field, tag } => format!("BadTag,field={field},tag={tag}"),
        WireFault::TooLong { field, .. } => format!("TooLong,field={field}"),
        WireFault::NonCanonical { field } => format!("NonCanonical,field={field}"),
    }
}

/// The most bytes `--verify` reads from its file.
pub const MAX_BUNDLE_BYTES: u64 = 16 << 20;

/// The regular file at `path`.
/// # Errors
/// Why the file was not read, as a phrase.
pub fn read_bounded(path: &str, max: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".to_owned());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    if u64::try_from(bytes.len()).map_or(true, |n| n > max) {
        return Err(format!("holds more than {max} bytes"));
    }
    Ok(bytes)
}

/// `s` as one word of a report line.
#[must_use]
pub fn word(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push('_'),
            '\\' => out.push_str("\\\\"),
            c if c.is_ascii_graphic() => out.push(c),
            c => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
        }
    }
    out
}

fn credits_the_grid(b: &UncheckedBundle) -> bool {
    b.selection.mode == Mode::Search && b.proposer == GridProposer::new().id()
}

fn search_counts_hold(b: &UncheckedBundle) -> bool {
    let st = &b.selection.stats;
    let counted = st
        .oversized
        .checked_add(st.duplicates)
        .and_then(|n| n.checked_add(st.over_budget))
        .and_then(|n| n.checked_add(st.distinct));
    st.rounds == MAX_ROUNDS
        && counted == Some(st.proposed)
        && st.distinct <= u64::try_from(MAX_DISTINCT).unwrap_or(u64::MAX)
        && (1..=st.distinct).contains(&st.admitted)
        && b.selection.held_out_looks == 1
        && b.index < st.proposed
}

fn verify_step(r: &BundleRefusal, b: &UncheckedBundle) -> Check {
    let BundleRefusal::Forged { field, .. } = r else {
        return Check::Decode;
    };
    match field {
        BundleField::Policy
        | BundleField::Build
        | BundleField::Base
        | BundleField::StrandedLightContract
        | BundleField::LaneFidelityContract
        | BundleField::Interpreter
        | BundleField::Lane
        | BundleField::Oracle
        | BundleField::Corpus
        | BundleField::Schema
        | BundleField::Profile
        | BundleField::Grade
        | BundleField::Assumptions => Check::Identities,
        BundleField::Plan | BundleField::Candidate => Check::Plan,
        BundleField::Proposer if credits_the_grid(b) && b.round < MAX_ROUNDS => Check::Grid,
        BundleField::Proposer => Check::Proposer,
        BundleField::Row
        | BundleField::Run
        | BundleField::StrandedLightVerdict
        | BundleField::LaneFidelityVerdict
        | BundleField::LaneDigest
        | BundleField::Chain
        | BundleField::Ticks
        | BundleField::Presented
        | BundleField::Wasted
        | BundleField::Firings => Check::Rows,
        BundleField::Stats if credits_the_grid(b) && search_counts_hold(b) => Check::Grid,
        BundleField::Stats | BundleField::Sums | BundleField::Useful | BundleField::Gain => {
            Check::Selection
        }
        BundleField::Timings => Check::Timings,
        BundleField::Publication => Check::Publication,
    }
}

fn same_clean_build(bundle: &str, lab: &str) -> Result<(), VerifyRefusal> {
    if bundle != lab {
        Err(VerifyRefusal::ForeignBuild {
            bundle: bundle.to_owned(),
        })
    } else if check_build(lab).is_err() {
        Err(VerifyRefusal::UnstampedBuild)
    } else if lab.ends_with("-dirty") {
        Err(VerifyRefusal::DirtyBuild)
    } else {
        Ok(())
    }
}

fn tpz(c: &Compared) -> String {
    let t = |t: &Totals| format!("T={} P={} Z={}", t.ticks, t.presented, t.wasted);
    format!("{} baseline {}", t(&c.candidate), t(&c.baseline))
}

fn passed(
    check: Check,
    b: &UncheckedBundle,
    bytes: &[u8],
    evaluator: &str,
) -> (&'static str, String) {
    let s = &b.selection;
    let st = &s.stats;
    match check {
        Check::Decode => (
            "pass",
            format!(
                "digest={} format={}",
                Digest::of(BUNDLE_DOMAIN, &[bytes]).short(),
                BUNDLE_MAGIC[8]
            ),
        ),
        Check::Build => ("pass", format!("build={}", word(&b.build))),
        Check::Evaluator => ("pass", evaluator.to_owned()),
        Check::Identities => (
            "pass",
            "the policy, base, contracts, interpreter, lane model, oracle, corpus, schema, \
             profile, grade and assumptions are the pinned policy's"
                .to_owned(),
        ),
        Check::Plan => (
            "pass",
            format!(
                "plan={} candidate={} {}",
                hex(&b.plan),
                word(&b.candidate.name),
                b.candidate.digest.short()
            ),
        ),
        Check::Proposer => (
            "pass",
            format!(
                "mode={} proposer={} v{} config={} round={} index={}",
                match s.mode {
                    Mode::Search => "search",
                    Mode::OnDemand => "on-demand",
                },
                word(&b.proposer.name),
                b.proposer.version,
                b.proposer.config.short(),
                b.round,
                b.index
            ),
        ),
        Check::Rows => (
            "pass",
            format!(
                "rows={}, each passing, each re-evaluated under the pinned policy",
                b.rows.len()
            ),
        ),
        Check::Selection => (
            "pass",
            format!(
                "proposed={} distinct={} admitted={} oversized={} duplicates={} over_budget={} \
                 rounds={} held_out_looks={} train {} held_out {} s0 {} useful={} gain_ppm={} \
                 s0_gain_ppm={}",
                st.proposed,
                st.distinct,
                st.admitted,
                st.oversized,
                st.duplicates,
                st.over_budget,
                st.rounds,
                s.held_out_looks,
                tpz(&s.train),
                tpz(&s.held_out),
                tpz(&s.s0),
                s.useful,
                s.gain_ppm,
                s.s0_gain_ppm
            ),
        ),
        Check::Grid if credits_the_grid(b) => (
            "pass",
            "provenance=rederived (the grid's own selection, re-run on the training split, \
             chose this plan at this round and index with these counts)"
                .to_owned(),
        ),
        Check::Grid => (
            "declared",
            format!(
                "provenance=declared (credited to {} v{}, not the grid: the mode, proposer, \
                 round, index and counts are declared, checked for consistency only)",
                word(&b.proposer.name),
                b.proposer.version
            ),
        ),
        Check::Timings => (
            "pass",
            format!("labelled={} (declared data, never scored)", b.timings.len()),
        ),
        Check::Publication => match &b.publication {
            None => (
                "none",
                "no publication part (nothing was published)".to_owned(),
            ),
            Some(p) => (
                "pass",
                format!(
                    "revision={} live={} candidate={} (the witness's own parts are checked for \
                     consistency only)",
                    p.revision,
                    word(&p.live_code.name),
                    word(&p.candidate_code.name)
                ),
            ),
        },
    }
}

/// How far a `--verify` run got.
#[derive(Default)]
struct Reached {
    decoded: Option<UncheckedBundle>,
    evaluator: String,
    measured: Option<(Duration, Duration)>,
}

fn run_checks<E: Borrow<Evaluator>>(
    bytes: &[u8],
    lab: &str,
    evaluator: impl FnOnce() -> Result<E, PolicyRefusal>,
    reached: &mut Reached,
) -> Result<(), VerifyRefusal> {
    let b = UncheckedBundle::decode(bytes).map_err(VerifyRefusal::Bundle)?;
    same_clean_build(&reached.decoded.insert(b).build, lab)?;
    let started = Instant::now();
    let built = evaluator().map_err(VerifyRefusal::Policy)?;
    let ev: &Evaluator = built.borrow();
    let build_time = started.elapsed();
    reached.evaluator = format!(
        "policy={} corpus={}",
        ev.policy().id().digest.short(),
        ev.corpus_digest().short()
    );
    let started = Instant::now();
    let verdict = CandidateBundle::verify(bytes, ev);
    reached.measured = Some((build_time, started.elapsed()));
    verdict.map(|_| ()).map_err(VerifyRefusal::Bundle)
}

/// `--verify`'s exit status (0 verified, 1 refused) and report of `bytes` (read from `file`) under
/// the build `lab`.
pub fn verify_bundle<E: Borrow<Evaluator>>(
    bytes: &[u8],
    file: &str,
    lab: &str,
    evaluator: impl FnOnce() -> Result<E, PolicyRefusal>,
) -> (i32, String) {
    let mut reached = Reached::default();
    let refusal = run_checks(bytes, lab, evaluator, &mut reached).err();
    let Reached {
        decoded,
        evaluator: evaluated,
        measured,
    } = reached;
    let failed = refusal.as_ref().map(|r| r.check(decoded.as_ref()));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "aos-search verify file={} bytes={} lab={}",
        word(file),
        bytes.len(),
        word(lab)
    );
    for check in Check::ALL {
        let (status, detail) = match (failed, &refusal, decoded.as_ref()) {
            (Some(at), Some(r), _) if check == at => {
                ("fail", format!("{}: {}", r.reason(), r.sentence(lab)))
            }
            (Some(at), _, _) if check > at => ("not-run", String::new()),
            (_, _, Some(b)) => passed(check, b, bytes, &evaluated),
            _ => ("not-run", String::new()),
        };
        let _ = writeln!(
            out,
            "  check {}={status}{}{detail}",
            check.name(),
            if detail.is_empty() { "" } else { " " }
        );
    }
    if let Some((built, verifying)) = measured {
        let _ = writeln!(
            out,
            "  MEASURED (never scored) evaluator_build_ms={:.1} verify_ms={:.1}",
            built.as_secs_f64() * 1e3,
            verifying.as_secs_f64() * 1e3
        );
    }
    let _ = writeln!(
        out,
        "aos-search verify={} reason={}",
        if refusal.is_some() { "fail" } else { "pass" },
        refusal
            .as_ref()
            .map_or_else(|| "none".to_owned(), VerifyRefusal::reason)
    );
    (i32::from(refusal.is_some()), out)
}

/// `aos-search`: runs the named searches into `--out`, or with `--verify FILE` verifies one bundle
/// offline.
pub fn cmd_aos_search(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    if let Some(file) = parse_verify(args)? {
        let path = from_caller(&file);
        let bytes = read_bounded(&path, MAX_BUNDLE_BYTES)
            .map_err(|why| Fail::usage(format!("aos-search: --verify {path}: {why}")))?;
        let (code, report) = verify_bundle(&bytes, &path, LAB_BUILD, || {
            Evaluator::v1(LAB_BUILD, Instant::now())
        });
        print!("{report}");
        return Ok(code);
    }
    let args = parse_args(args)?;
    let out = crate::trail_gallery::resolve_out(
        &from_caller(&args.out),
        &crate::trail_gallery::checkouts(&paths().repo),
    )
    .map_err(|e| Fail::usage(format!("aos-search: --out {e}")))?;
    let text = args
        .plans
        .as_ref()
        .map(|p| {
            let path = from_caller(p);
            std::fs::read_to_string(&path)
                .map_err(|e| Fail::usage(format!("aos-search: --plans {path}: {e}")))
        })
        .transpose()?;
    std::fs::create_dir_all(&out)
        .map_err(|e| Fail::die(format!("aos-search: cannot make {}: {e}", out.display())))?;
    let (code, summary) = run(&args, text.as_deref(), &out)?;
    print!("{summary}");
    println!("aos-search: wrote {}", out.display());
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    fn usage_of(args: &[&str]) -> String {
        let f = parse_args(&words(args)).expect_err("a usage error");
        assert_eq!(f.code, 2, "{:?}", f.msg);
        f.msg.unwrap_or_default()
    }

    #[test]
    fn aos_search_parses_its_proposers_and_refuses_a_run_without_one_or_an_out() {
        assert_eq!(
            parse_args(&words(&["--grid", "--out", "/tmp/x"])).expect("parses"),
            Args {
                grid: true,
                plans: None,
                out: "/tmp/x".into(),
                toolchain: LAB_BUILD.into(),
            }
        );
        assert_eq!(
            parse_args(&words(&[
                "--out",
                "o",
                "--plans",
                "p.txt",
                "--grid",
                "--toolchain",
                "targo 1.99.0-dev (targo 0.11.0)",
            ]))
            .expect("parses"),
            Args {
                grid: true,
                plans: Some("p.txt".into()),
                out: "o".into(),
                toolchain: "targo 1.99.0-dev (targo 0.11.0)".into(),
            }
        );
        assert!(usage_of(&["--out", "o"]).contains("name a proposer"));
        assert!(usage_of(&["--grid"]).contains("--out DIR is required"));
        assert!(usage_of(&["--grid", "--out"]).contains("--out needs a value"));
        assert!(usage_of(&["--plans"]).contains("--plans needs a value"));
        assert!(usage_of(&["--grid", "--out", "o", "--toolchain", " "]).contains("non-empty"));
        assert!(usage_of(&["--grid", "--out", "o", "extra"]).contains("unexpected 'extra'"));
        assert!(LAB_BUILD.starts_with("aterm-lab "));
    }

    #[test]
    fn the_lab_build_names_its_commit_and_a_build_that_names_none_is_refused() {
        assert!(
            LAB_BUILD.starts_with(concat!("aterm-lab ", env!("CARGO_PKG_VERSION"), "+g")),
            "{LAB_BUILD}"
        );
        if let Err(f) = check_build(LAB_BUILD) {
            panic!("{LAB_BUILD}: {:?}", f.msg);
        }
        let v = env!("CARGO_PKG_VERSION");
        for commit in [
            "0123456789ab",
            "0123456789ab-dirty",
            "0123456789abcdef0123456789abcdef01234567",
        ] {
            let build = format!("aterm-lab {v}+g{commit}");
            assert!(check_build(&build).is_ok(), "{build}");
        }
        for build in [
            format!("aterm-lab {v}+gunknown"),
            format!("aterm-lab {v}"),
            format!("aterm-lab {v}+g"),
            format!("aterm-lab {v}+g0123456789a"),
            format!("aterm-lab {v}+g0123456789AB"),
            format!("aterm-lab {v}+g0123456789ab-dirty-dirty"),
        ] {
            let f = check_build(&build).expect_err(&build);
            assert_eq!(f.code, 1, "{build}");
            let msg = f.msg.unwrap_or_default();
            assert!(
                msg.contains(&format!("the build `{build}` names no commit")),
                "{msg}"
            );
        }
    }

    #[test]
    fn a_small_plans_search_writes_a_verified_bundle_and_its_summary() {
        let dir = aterm_tempfile::tempdir().expect("an exclusive out dir");
        let text = "\
# the grid winner {SettleAtStep{0}, NextStep}, the cadence plan, a cold-only plan
010100000000000000000000
010003000000000000000000
010001000000000000000000
010100000000000000000000  # again: a duplicate
not a plan
";
        let args = Args {
            grid: false,
            plans: Some("plans.txt".into()),
            out: dir.path().display().to_string(),
            toolchain: "targo test-line".into(),
        };
        let (code, summary) = run(&args, Some(text), dir.path()).expect("runs");
        assert_eq!(code, 0, "{summary}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("summary.txt")).expect("written"),
            summary
        );
        for want in [
            "search proposer=plans file v1 config=",
            "  counts proposed=5 distinct=4 admitted=2 oversized=0 duplicates=1 over_budget=0 rounds=4\n",
            "NoStepSlept=1",
            "PlanRefusal=1",
            " unreported=0\n",
            "  winner plan=010100000000000000000000 digest=",
            " round=0 index=0 useful=true\n",
            "  selection provenance=declared\n",
            "  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255\n",
            "  bundle file=plans.bundle bytes=",
            " verified=true build=aterm-lab_",
            " toolchain=targo_test-line\n",
            "  MEASURED (never scored) wall_ms=",
            " stepped=",
            "aos-search result=pass searches=1 winners=1\n",
        ] {
            assert!(summary.contains(want), "{want:?} in:\n{summary}");
        }
        let bytes = std::fs::read(dir.path().join("plans.bundle")).expect("the bundle");
        let ev = Evaluator::v1(LAB_BUILD, Instant::now()).expect("the pinned policy");
        let bundle = CandidateBundle::verify(&bytes, &ev).expect("the bundle verifies");
        assert_eq!(bundle.selection_provenance().name(), "declared");
        let parts = bundle.parts();
        assert_eq!(parts.toolchain, "targo test-line");
        let labels: Vec<&str> = parts.timings.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, [SEARCH_TIMING, EVALUATOR_TIMING]);
        assert!(
            parts.publication.is_none(),
            "a lab search publishes nothing"
        );
        let (version, commit) = LAB_BUILD
            .rsplit_once("+g")
            .expect("LAB_BUILD names its commit");
        let first = if commit.starts_with('0') { '1' } else { '0' };
        let other = format!("{version}+g{first}{}", commit.get(1..).unwrap_or_default());
        let ev = Evaluator::v1(&other, Instant::now()).expect("the pinned policy");
        assert_eq!(
            CandidateBundle::verify(&bytes, &ev),
            Err(BundleRefusal::Forged {
                field: BundleField::Build,
                row: None
            }),
            "{other}"
        );

        let none = aterm_tempfile::tempdir().expect("an exclusive out dir");
        let (code, summary) =
            run(&args, Some("010001000000000000000000\n"), none.path()).expect("runs");
        assert_eq!(code, 1, "{summary}");
        assert!(
            summary.contains("  no-outcome no outcome: none of 1 distinct"),
            "{summary}"
        );
        assert!(summary.contains("aos-search result=fail searches=1 winners=0\n"));
        assert!(!none.path().join("plans.bundle").exists());
    }

    const TEST_BUILD: &str = concat!("aterm-lab ", env!("CARGO_PKG_VERSION"), "+g0123456789ab");

    const GRID_STATS: SearchStats = SearchStats {
        rounds: 4,
        proposed: 54,
        oversized: 0,
        duplicates: 0,
        over_budget: 0,
        distinct: 54,
        admitted: 18,
    };

    fn evaluator() -> &'static Evaluator {
        static EV: std::sync::OnceLock<Evaluator> = std::sync::OnceLock::new();
        EV.get_or_init(|| Evaluator::v1(TEST_BUILD, Instant::now()).expect("the pinned policy"))
    }

    fn searched_bundle() -> Vec<u8> {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES
            .get_or_init(|| {
                let dir = aterm_tempfile::tempdir().expect("an exclusive out dir");
                let args = Args {
                    grid: false,
                    plans: Some("plans.txt".into()),
                    out: dir.path().display().to_string(),
                    toolchain: "targo test-line".into(),
                };
                let (code, summary) = run_in(
                    &args,
                    Some("010100000000000000000000\n"),
                    dir.path(),
                    TEST_BUILD,
                )
                .expect("runs");
                assert_eq!(code, 0, "{summary}");
                std::fs::read(dir.path().join("plans.bundle")).expect("the bundle it wrote")
            })
            .clone()
    }

    fn report(bytes: &[u8], lab: &str) -> (i32, String) {
        verify_bundle(bytes, "t.bundle", lab, || {
            Ok::<_, PolicyRefusal>(evaluator())
        })
    }

    fn report_unbuilt(bytes: &[u8], lab: &str) -> (i32, String) {
        verify_bundle(
            bytes,
            "t.bundle",
            lab,
            || -> Result<&Evaluator, PolicyRefusal> {
                panic!("an evaluator was built for a bundle refused before one")
            },
        )
    }

    fn resealed(bytes: &[u8], edit: impl FnOnce(&mut UncheckedBundle)) -> Vec<u8> {
        let mut b = UncheckedBundle::decode(bytes).expect("the bundle decodes");
        edit(&mut b);
        b.encode()
    }

    fn statuses(report: &str) -> Vec<(String, String)> {
        report
            .lines()
            .filter_map(|l| l.strip_prefix("  check "))
            .map(|l| {
                let head = l.split(' ').next().unwrap_or_default();
                let (name, status) = head.split_once('=').unwrap_or((head, ""));
                (name.to_owned(), status.to_owned())
            })
            .collect()
    }

    fn refused_at(at: Check, grid: &str) -> Vec<(String, String)> {
        Check::ALL
            .into_iter()
            .map(|c| {
                let status = match c.cmp(&at) {
                    std::cmp::Ordering::Less if c == Check::Grid => grid,
                    std::cmp::Ordering::Less => "pass",
                    std::cmp::Ordering::Equal => "fail",
                    std::cmp::Ordering::Greater => "not-run",
                };
                (c.name().to_owned(), status.to_owned())
            })
            .collect()
    }

    fn verdict(report: &str) -> &str {
        report.lines().last().unwrap_or_default()
    }

    #[test]
    fn the_verify_flag_takes_one_file_and_nothing_else() {
        let verify_usage = |args: &[&str]| {
            let f = parse_verify(&words(args)).expect_err("a usage error");
            assert_eq!(f.code, 2, "{:?}", f.msg);
            f.msg.unwrap_or_default()
        };
        assert_eq!(
            parse_verify(&words(&["--verify", "b.bundle"])).expect("parses"),
            Some("b.bundle".to_owned())
        );
        assert_eq!(
            parse_verify(&words(&["--grid", "--out", "o"])).expect("parses"),
            None
        );
        assert!(verify_usage(&["--verify"]).contains("--verify needs a value"));
        assert!(verify_usage(&["--verify", "b", "--grid"]).contains("unexpected '--grid'"));
        assert!(verify_usage(&["--out", "o", "--verify", "b"]).contains("unexpected '--out'"));
        assert!(
            verify_usage(&["--verify", "a", "--verify", "b"]).contains("unexpected '--verify'")
        );
        assert!(Check::ALL.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(
            word("a b\n\\\u{e9}=train-03"),
            "a_b\\u{a}\\\\\\u{e9}=train-03"
        );
    }

    #[test]
    fn a_bundle_a_small_search_wrote_verifies_offline_one_line_per_check() {
        let bytes = searched_bundle();
        let (code, r) = report(&bytes, TEST_BUILD);
        assert_eq!(code, 0, "{r}");
        let mut want = refused_at(Check::Publication, "declared");
        want[10].1 = "none".to_owned();
        assert_eq!(statuses(&r), want, "{r}");
        for line in [
            "aos-search verify file=t.bundle bytes=",
            &format!(" lab={}\n", word(TEST_BUILD)),
            &format!("  check build=pass build={}\n", word(TEST_BUILD)),
            "  check plan=pass plan=010100000000000000000000 candidate=momentum-glow.schedule-plan ",
            "  check proposer=pass mode=search proposer=file v1 config=",
            " round=0 index=0\n",
            "  check rows=pass rows=26,",
            " proposed=1 distinct=1 admitted=1 oversized=0 duplicates=0 over_budget=0 rounds=4 \
             held_out_looks=1 ",
            " held_out T=5119 P=4958 Z=161 baseline T=6685 P=4935 Z=1750 ",
            " useful=true gain_ppm=234255 s0_gain_ppm=99423\n",
            "  check grid=declared provenance=declared (credited to file v1, not the grid",
            "  check timings=pass labelled=2 ",
            "  check publication=none ",
            "  MEASURED (never scored) evaluator_build_ms=",
        ] {
            assert!(r.contains(line), "{line:?} in:\n{r}");
        }
        assert_eq!(verdict(&r), "aos-search verify=pass reason=none");
        assert!(r.contains(&format!(
            "  check decode=pass digest={} format=1\n",
            Digest::of(BUNDLE_DOMAIN, &[&bytes]).short()
        )));
    }

    #[test]
    fn a_cut_a_flipped_and_a_resealed_bundle_are_refused_naming_the_reason() {
        let bytes = searched_bundle();
        let n = bytes.len();
        let flipped = |at: usize, bit: u8| {
            let mut b = bytes.clone();
            b[at] ^= bit;
            b
        };
        for (forged, reason) in [
            (bytes[..n - 1].to_vec(), "Seal"),
            (bytes[..n / 2].to_vec(), "Seal"),
            (bytes[..20].to_vec(), "Wire{Truncated,field=bundle.seal}"),
            (bytes[..5].to_vec(), "Magic"),
            (Vec::new(), "Magic"),
            (flipped(n / 2, 0x10), "Seal"),
            (flipped(n - 1, 0x01), "Seal"),
            (flipped(0, 0x01), "Magic"),
            (flipped(8, 0x02), "Format{found=3}"),
        ] {
            let (code, r) = report_unbuilt(&forged, TEST_BUILD);
            assert_eq!(code, 1, "{r}");
            assert_eq!(statuses(&r), refused_at(Check::Decode, "pass"), "{r}");
            assert_eq!(
                verdict(&r),
                format!("aos-search verify=fail reason={reason}")
            );
            assert!(!r.contains("MEASURED"), "{r}");
        }

        let item = UncheckedBundle::decode(&bytes).expect("decodes").rows[3]
            .item
            .clone();
        assert_eq!(item, "train-03");
        let more_ticks = resealed(&bytes, |b| b.rows[3].ticks += 1);
        let (code, r) = report(&more_ticks, TEST_BUILD);
        assert_eq!(code, 1, "{r}");
        assert_eq!(statuses(&r), refused_at(Check::Rows, "declared"), "{r}");
        assert!(r.contains(
            "  check rows=fail Forged{field=Ticks,row=train-03}: the bundle is forged: the T of \
             the row for train-03 is not what the pinned policy computes\n"
        ));
        assert_eq!(
            verdict(&r),
            "aos-search verify=fail reason=Forged{field=Ticks,row=train-03}"
        );
        assert!(r.contains("  MEASURED (never scored) evaluator_build_ms="));

        for (edit, at, reason) in [
            (
                (|b: &mut UncheckedBundle| b.round = MAX_ROUNDS) as fn(&mut UncheckedBundle),
                Check::Proposer,
                "Forged{field=Proposer}",
            ),
            (
                |b: &mut UncheckedBundle| b.selection.stats.proposed += 1,
                Check::Selection,
                "Forged{field=Stats}",
            ),
            (
                |b: &mut UncheckedBundle| b.selection.train.candidate.ticks += 1,
                Check::Selection,
                "Forged{field=Sums}",
            ),
            (
                |b: &mut UncheckedBundle| b.timings[0].label.clear(),
                Check::Timings,
                "Forged{field=Timings}",
            ),
            (
                |b: &mut UncheckedBundle| b.plan[0] = 2,
                Check::Plan,
                "Forged{field=Plan}",
            ),
            (
                |b: &mut UncheckedBundle| b.grade = "Proved".into(),
                Check::Identities,
                "Forged{field=Grade}",
            ),
            (
                |b: &mut UncheckedBundle| {
                    b.plan[0] = 2;
                    b.grade = "Proved".into();
                },
                Check::Identities,
                "Forged{field=Grade}",
            ),
        ] {
            let (code, r) = report(&resealed(&bytes, edit), TEST_BUILD);
            assert_eq!(code, 1, "{r}");
            assert_eq!(statuses(&r), refused_at(at, "declared"), "{reason}:\n{r}");
            assert_eq!(
                verdict(&r),
                format!("aos-search verify=fail reason={reason}")
            );
        }
    }

    #[test]
    fn a_bundle_of_another_build_is_refused_before_any_evaluator_is_built() {
        let bytes = searched_bundle();
        let v = env!("CARGO_PKG_VERSION");
        let other = format!("aterm-lab {v}+g0123456789ac");
        let (code, r) = report_unbuilt(&bytes, &other);
        assert_eq!(code, 1, "{r}");
        assert_eq!(statuses(&r), refused_at(Check::Build, "pass"), "{r}");
        assert_eq!(verdict(&r), "aos-search verify=fail reason=ForeignBuild");
        assert!(r.contains(&format!(
            "  check build=fail ForeignBuild: the bundle names the build {}, not this lab's {}:",
            word(TEST_BUILD),
            word(&other)
        )));
        let window = resealed(&bytes, |b| b.build = format!("{v}+g0123456789ab"));
        let (code, r) = report_unbuilt(&window, TEST_BUILD);
        assert_eq!(
            (code, verdict(&r)),
            (1, "aos-search verify=fail reason=ForeignBuild")
        );

        for (build, reason) in [
            (format!("{TEST_BUILD}-dirty"), "DirtyBuild"),
            (format!("aterm-lab {v}+gunknown"), "UnstampedBuild"),
            (format!("aterm-lab {v}"), "UnstampedBuild"),
        ] {
            let named = resealed(&bytes, |b| b.build = build.clone());
            let (code, r) = report_unbuilt(&named, &build);
            assert_eq!(code, 1, "{r}");
            assert_eq!(statuses(&r), refused_at(Check::Build, "pass"), "{r}");
            assert_eq!(
                verdict(&r),
                format!("aos-search verify=fail reason={reason}")
            );
        }

        let hostile = resealed(&bytes, |b| {
            b.build = "x\naos-search verify=pass reason=none\n  check build=pass".into();
        });
        let (code, r) = report_unbuilt(&hostile, TEST_BUILD);
        assert_eq!(code, 1, "{r}");
        assert_eq!(r.lines().count(), 1 + Check::ALL.len() + 1, "{r}");
        assert_eq!(
            r.lines()
                .filter(|l| l.starts_with("aos-search verify="))
                .count(),
            1,
            "{r}"
        );
        assert_eq!(verdict(&r), "aos-search verify=fail reason=ForeignBuild");
        for (file, shown) in [
            (
                "x\naos-search verify=pass reason=none\n.bundle",
                "file=x\\u{a}aos-search_verify=pass_reason=none\\u{a}.bundle ",
            ),
            ("My Bundles/b.bundle", "file=My_Bundles/b.bundle "),
        ] {
            let (code, r) = verify_bundle(
                &bytes,
                file,
                &other,
                || -> Result<&Evaluator, PolicyRefusal> {
                    panic!("an evaluator was built for a bundle refused before one")
                },
            );
            assert_eq!(code, 1, "{r}");
            assert_eq!(r.lines().count(), 1 + Check::ALL.len() + 1, "{r}");
            assert_eq!(
                r.lines()
                    .filter(|l| l.starts_with("aos-search verify="))
                    .count(),
                1,
                "{r}"
            );
            assert!(
                r.lines()
                    .next()
                    .is_some_and(|l| l.starts_with(&format!("aos-search verify {shown}"))),
                "{r}"
            );
            assert_eq!(verdict(&r), "aos-search verify=fail reason=ForeignBuild");
        }

        let (code, r) = verify_bundle(&bytes, "t.bundle", TEST_BUILD, || {
            Err::<&Evaluator, _>(PolicyRefusal::CorpusDrift {
                item: "golden".into(),
            })
        });
        assert_eq!(code, 1, "{r}");
        assert_eq!(statuses(&r), refused_at(Check::Evaluator, "pass"), "{r}");
        assert_eq!(
            verdict(&r),
            "aos-search verify=fail reason=CorpusDrift{item=golden}"
        );

        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let file = dir.path().join("plans.bundle");
        std::fs::write(&file, &bytes).expect("written");
        let path = file.display().to_string();
        assert_eq!(
            cmd_aos_search(&words(&["--verify", &path])).expect("a report"),
            1,
            "a bundle of TEST_BUILD is foreign to this lab's build {LAB_BUILD}"
        );
        let missing = dir.path().join("none.bundle").display().to_string();
        let f = cmd_aos_search(&words(&["--verify", &missing])).expect_err("usage");
        assert_eq!(f.code, 2, "{:?}", f.msg);
    }

    #[test]
    fn a_published_bundle_of_this_labs_build_passes_the_publication_check() {
        use aterm_effects::aos_momentum::plan::{SchedulePlan, Split, Wake};
        use aterm_effects::aos_momentum::{MomentumCode, MomentumHost};
        let origin = Instant::now();
        let winner =
            SchedulePlan::new(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep).expect("in range");
        let admitted = evaluator()
            .admit_one(&winner.encode())
            .expect("the winner is admitted");
        let mut host = MomentumHost::default();
        host.aos_attach(origin, MomentumCode::Shipped, TEST_BUILD, 1);
        host.aos_pause(origin, Duration::from_nanos(16_666_667))
            .expect("a running glow pauses");
        let published = host.aos_publish(admitted).expect("the winner publishes");
        let bytes = published.bundle().encode();
        let (code, r) = report(&bytes, TEST_BUILD);
        assert_eq!(code, 0, "{r}");
        let mut want = refused_at(Check::Publication, "declared");
        want[10].1 = "pass".to_owned();
        assert_eq!(statuses(&r), want, "{r}");
        assert!(
            r.contains(&format!(
                "  check publication=pass revision={} live=momentum-glow.shipped \
                 candidate=momentum-glow.schedule-plan ",
                published.revision().0
            )),
            "{r}"
        );
        assert_eq!(verdict(&r), "aos-search verify=pass reason=none");
    }

    #[test]
    fn the_verify_file_is_read_bounded_and_only_when_it_is_a_regular_file() {
        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let sized = |name: &str, len: u64| {
            let path = dir.path().join(name);
            std::fs::File::create(&path)
                .and_then(|f| f.set_len(len))
                .expect("a sparse file");
            path.display().to_string()
        };
        let usage = |path: &str| {
            let f = cmd_aos_search(&words(&["--verify", path])).expect_err("a usage error");
            assert_eq!(f.code, 2, "{:?}", f.msg);
            f.msg.unwrap_or_default()
        };
        let over = sized("over.bundle", MAX_BUNDLE_BYTES + 1);
        assert!(
            usage(&over).ends_with(&format!(": holds more than {MAX_BUNDLE_BYTES} bytes")),
            "{over}"
        );
        let at = sized("at.bundle", MAX_BUNDLE_BYTES);
        assert_eq!(
            cmd_aos_search(&words(&["--verify", &at])).expect("a report"),
            1,
            "read whole, then refused at decode"
        );
        for path in [dir.path().display().to_string(), "/dev/zero".to_owned()] {
            assert!(usage(&path).ends_with(": not a regular file"), "{path}");
        }
        assert_eq!(
            read_bounded(&at, MAX_BUNDLE_BYTES).map(|b| b.len()),
            Ok(usize::try_from(MAX_BUNDLE_BYTES).expect("16 MiB"))
        );
        assert_eq!(
            read_bounded(&over, MAX_BUNDLE_BYTES),
            Err(format!("holds more than {MAX_BUNDLE_BYTES} bytes"))
        );
    }

    #[test]
    fn a_selection_credited_to_the_grid_is_refused_at_the_step_that_checks_it() {
        let bytes = searched_bundle();
        let grid = |b: &mut UncheckedBundle| b.proposer = GridProposer::new().id();
        for (edit, at, reason) in [
            (
                (|_: &mut UncheckedBundle| {}) as fn(&mut UncheckedBundle),
                Some(Check::Grid),
                "Forged{field=Proposer}",
            ),
            (
                |b: &mut UncheckedBundle| {
                    b.index = 6;
                    b.selection.stats = SearchStats {
                        admitted: 17,
                        ..GRID_STATS
                    };
                },
                Some(Check::Grid),
                "Forged{field=Stats}",
            ),
            (
                |b: &mut UncheckedBundle| b.round = MAX_ROUNDS,
                Some(Check::Proposer),
                "Forged{field=Proposer}",
            ),
            (
                |b: &mut UncheckedBundle| {
                    b.index = 6;
                    b.selection.stats = SearchStats {
                        rounds: 3,
                        ..GRID_STATS
                    };
                },
                Some(Check::Selection),
                "Forged{field=Stats}",
            ),
            (
                |b: &mut UncheckedBundle| {
                    b.index = 6;
                    b.selection.stats = GRID_STATS;
                },
                None,
                "none",
            ),
        ] {
            let forged = resealed(&bytes, |b| {
                grid(b);
                edit(b);
            });
            let (code, r) = report(&forged, TEST_BUILD);
            match at {
                Some(at) => {
                    assert_eq!(code, 1, "{r}");
                    assert_eq!(statuses(&r), refused_at(at, "pass"), "{reason}:\n{r}");
                    assert_eq!(
                        verdict(&r),
                        format!("aos-search verify=fail reason={reason}")
                    );
                }
                None => {
                    assert_eq!(code, 0, "{r}");
                    let mut want = refused_at(Check::Publication, "pass");
                    want[10].1 = "none".to_owned();
                    assert_eq!(statuses(&r), want, "{r}");
                    assert!(r.contains("  check grid=pass provenance=rederived "), "{r}");
                    assert!(r.contains(" proposer=grid v1 "), "{r}");
                    assert_eq!(verdict(&r), "aos-search verify=pass reason=none");
                }
            }
        }
    }
}
