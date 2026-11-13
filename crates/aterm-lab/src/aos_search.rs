// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-search`: the candidate search under the pinned acceptance policy, in process, and the
//! offline verifier of the bundles it writes.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_aos::Digest;
use aterm_aos::wire::WireFault;
use aterm_effects::aos_momentum::bundle::{
    BUNDLE_DOMAIN, BUNDLE_MAGIC, BundleField, BundleRefusal, CandidateBundle, UncheckedBundle,
};
use aterm_effects::aos_momentum::m4::{Compared, Evaluator, NoOutcome, Selection, Serial, Spread};
use aterm_effects::aos_momentum::policy::{MAX_DISTINCT, MAX_ROUNDS, PolicyRefusal, Totals};
use aterm_effects::aos_momentum::search::{
    FileProposer, GridProposer, Mode, Proposer, ProposerId, SearchStats, TrainFeedback,
};

use crate::instance::{take_flag, take_value};
use crate::lab_build::{check_build, lab_build};
use crate::paths::{from_caller, paths};
use crate::spread::{Phase, Stepped, Threads, available, say, say_built};
use crate::{Fail, Res};

/// The verb's own help (`aos-search --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-search [--grid] [--plans FILE] --out DIR [--toolchain LINE]
                                         [--threads N]
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
  --threads N       judge each round's distinct candidates on N threads: the local search, its mode `search`
  --frames          (debug builds) say each step the run takes on stderr, for tests' hang detector: the evaluator
                    built, each candidate trained, and the winner's held-out read and verify as each begins
  --verify FILE     no search: decode the bundle FILE and verify it (CandidateBundle::verify_spread on every core, every row re-evaluated)
                    under an evaluator of this lab's build; a bundle of another build is refused ForeignBuild, and one
                    of a dirty build DirtyBuild, before any evaluator is built. One line per check (pass, fail, not-run,
                    none or declared), then `aos-search verify=pass|fail reason=...`

exit: 0 every search had a winner and every bundle verified and was written; 1 this lab's build names no commit, a
search had no outcome, a bundle did not verify, or a file could not be written; 2 usage
exit (--verify): 0 the bundle verified; 1 it was refused (the reason named); 2 usage, or FILE cannot be read
";

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
    /// `--threads N`: the local search over N threads.
    pub threads: Option<usize>,
    /// `--frames` (debug builds): the run says its steps on stderr, for tests' hang detector.
    pub frames: bool,
}

/// The verb's arguments. Pure.
/// # Errors
/// A usage error: an unknown word, a flag without its value or with a bad one, no proposer, no
/// `--out`, or a debug-only flag in a release build.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let grid = take_flag(&mut a, "--grid");
    let plans = take_value(&mut a, "--plans")?;
    let out = take_value(&mut a, "--out")?;
    let toolchain = take_value(&mut a, "--toolchain")?;
    let threads = take_value(&mut a, "--threads")?;
    let frames = take_flag(&mut a, "--frames");
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
    let toolchain = toolchain.unwrap_or_else(|| lab_build().to_owned());
    if toolchain.trim().is_empty() {
        return Err(Fail::usage(
            "aos-search: --toolchain needs a non-empty line",
        ));
    }
    let threads = threads
        .map(|v| {
            v.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| v.parse::<usize>().ok())
                .flatten()
                .filter(|n| (1..=256).contains(n))
                .ok_or_else(|| {
                    Fail::usage(format!("aos-search: --threads takes 1 to 256, not '{v}'"))
                })
        })
        .transpose()?;
    if frames && !cfg!(debug_assertions) {
        return Err(Fail::usage(
            "aos-search: --frames exists in debug builds only",
        ));
    }
    Ok(Args {
        grid,
        plans,
        out,
        toolchain,
        threads,
        frames,
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

/// How a search judges its candidates.
enum How {
    /// M4's serial path, each candidate trained said as a step of the search numbered here under
    /// `--frames` ([`Stepped`]).
    Serial(Option<u32>),
    /// `--threads N`: the local fold over a thread spread, said as [`How::Serial`]'s is.
    Threads(usize, Option<u32>),
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
    /// The held-out read's wall time, when it ran.
    held_out: Option<Duration>,
    /// The in-search verify's wall time, when it ran.
    verify: Option<Duration>,
    /// The bundle file written.
    bundle: Option<PathBuf>,
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

/// A local search's selection over `spread`, each candidate trained said as a step of search
/// `said` under `--frames` ([`Stepped`]): a spread changes only speed, so it is
/// [`Evaluator::select`]'s.
fn trained<P: Proposer>(
    ev: &Evaluator,
    rec: &mut Recorded<P>,
    spread: &dyn Spread,
    said: Option<u32>,
) -> Result<Selection, NoOutcome> {
    match said {
        Some(serial) => ev.select_spread(rec, &Stepped::new(spread, serial)),
        None => ev.select_spread(rec, spread),
    }
}

fn search<P: Proposer>(
    ev: &Evaluator,
    name: &'static str,
    proposer: P,
    toolchain: &str,
    built: Duration,
    out: &Path,
    how: How,
) -> Searched {
    let mut rec = Recorded {
        inner: proposer,
        returned: 0,
        shown: Vec::new(),
    };
    let id = rec.id();
    let stepped_before = ev.stepped();
    let started = Instant::now();
    let (picked, said) = match how {
        How::Serial(said) => (trained(ev, &mut rec, &Serial, said), said),
        How::Threads(n, said) => (trained(ev, &mut rec, &Threads(n), said), said),
    };
    let stats = match &picked {
        Ok(selection) => Some(selection.stats()),
        Err(NoOutcome::NoneAdmitted { stats } | NoOutcome::NoGain { stats, .. }) => Some(*stats),
        Err(NoOutcome::HeldOut { .. } | NoOutcome::Foreign { .. }) => None,
    };
    if let Some(serial) = said
        && picked.is_ok()
    {
        say(serial, Phase::HeldOut);
    }
    let reading = Instant::now();
    let outcome = picked.and_then(|selection| ev.held_out(selection));
    let held_out = outcome.is_ok().then(|| reading.elapsed());
    let wall = started.elapsed();
    let stepped = ev.stepped().saturating_sub(stepped_before);
    let mut verify = None;
    let mut bundle_file = None;
    let result = match outcome {
        Err(no) => Err(no.to_string()),
        Ok(mut admitted) => (|| {
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
            if let Some(serial) = said {
                say(serial, Phase::Verify);
            }
            let verifying = Instant::now();
            let verified = CandidateBundle::verify(&bytes, ev)
                .map_err(|r| format!("the winner's bundle does not verify: {r}"))?;
            verify = Some(verifying.elapsed());
            if verified != bundle {
                return Err("the verified bundle is not the one written".to_owned());
            }
            let file = format!("{name}.bundle");
            crate::util::write_fresh(&out.join(&file), &bytes)
                .map_err(|e| format!("cannot write {}: {e}", out.join(&file).display()))?;
            bundle_file = Some(out.join(&file));
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
        })(),
    };
    Searched {
        name,
        id,
        stats,
        returned: rec.returned,
        shown: rec.shown,
        result,
        wall,
        stepped,
        held_out,
        verify,
        bundle: bundle_file,
    }
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1e3)
}

fn opt_ms(d: Option<Duration>) -> String {
    d.map_or_else(|| "none".to_owned(), ms)
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
        "  MEASURED (never scored) wall_ms={} stepped={}",
        ms(s.wall),
        s.stepped
    );
    let _ = writeln!(
        out,
        "  MEASURED (never scored) held_out_ms={} verify_ms={}",
        opt_ms(s.held_out),
        opt_ms(s.verify)
    );
}

/// How `args` judge a search: `--frames` says its steps as the command's search `serial`, from 1.
fn how(args: &Args, serial: usize) -> How {
    let said = args
        .frames
        .then(|| u32::try_from(serial).unwrap_or(u32::MAX));
    match args.threads {
        Some(n) => How::Threads(n, said),
        None => How::Serial(said),
    }
}

/// The searches `args` names.
/// # Errors
/// This lab's build names no commit (exit 1, before any evaluator is built), the pinned policy
/// refused (exit 1), or the summary could not be written.
pub fn run(args: &Args, plans_text: Option<&str>, out: &Path) -> Result<(i32, String), Fail> {
    run_in(args, plans_text, out, lab_build())
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
    if args.frames {
        say_built();
    }
    let mut searched = Vec::new();
    if args.grid {
        searched.push(search(
            &ev,
            "grid",
            GridProposer::new(),
            &args.toolchain,
            built,
            out,
            how(args, 1),
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
            how(args, searched.len() + 1),
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
    if let Some(n) = args.threads {
        let _ = writeln!(summary, "threads n={n}");
    }
    for s in &searched {
        summary_of(s, &mut summary);
    }
    let winners = searched
        .iter()
        .filter(|s| s.result.is_ok() && s.bundle.is_some())
        .count();
    let pass = winners == searched.len();
    let _ = writeln!(
        summary,
        "aos-search result={} searches={} winners={winners}",
        if pass { "pass" } else { "fail" },
        searched.len()
    );
    crate::util::write_fresh(&out.join("summary.txt"), &summary).map_err(|e| {
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
    let (f, meta) = aterm_uds::regular_file::open(Path::new(path)).map_err(|e| e.to_string())?;
    if meta.len() > max {
        return Err(format!("holds more than {max} bytes"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    f.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
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
    let verdict = CandidateBundle::verify_spread(bytes, ev, &Threads(available()));
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
        let (code, report) = verify_bundle(&bytes, &path, lab_build(), || {
            Evaluator::v1(lab_build(), Instant::now())
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
            crate::util::read_regular_text(std::path::Path::new(&path))
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
#[path = "aos_search_tests.rs"]
mod tests;
