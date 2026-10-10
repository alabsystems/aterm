// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! MEASURING whether Spotlight indexes a directory (§9, storage hygiene): the probe
//! behind `aterm pkg noindex verify <dir>`, `aterm pkg devroot verify` and the
//! `spotlight` rows of both doctors ([`crate::devroot`]).
//!
//! Nothing here renames, links or excludes anything. Where code lives is decided by the
//! developer root ([`crate::devroot`]): `~/Developer`, which the owner adds to the one
//! exclusion Apple documents — the Search Privacy list in System Settings ▸ Spotlight,
//! which "keeps listed items from being indexed at all" and which only the owner can
//! change. This module is how aterm knows whether that gesture took, instead of assuming
//! it did.
//!
//! # What the index does, measured
//!
//! On macOS 27.0 (26A428), 2026-10-08, with `mdls` (an indexed file carries
//! `kMDItemContentType` from an importer; a file the index skips has only `kMDItemFS*`):
//!
//! * a directory whose name ends `.noindex` is NOT indexed, and neither is its subtree;
//! * a dot-directory (`~/.cargo`, `.git`) is NOT indexed, its subtree included;
//! * a `.metadata_never_index` marker file is INERT: the directory holding it is indexed;
//! * every indexed text file carries `_kMDItemPrimaryTextEmbedding`, a 1,024-byte vector,
//!   which is why a source tree is expensive to index;
//! * a scoped `mdfind -count 'kMDItemFSName == "*"'` does NOT count the index (it returned
//!   891,604 for an excluded tree and 0 for an indexed one) and is never an instrument here.
//!
//! None of those rules is a documented Apple API — which is exactly why [`verify`]
//! MEASURES instead of reading a name. [`exclusion_of`] is what a path's NAME claims, and
//! nothing decides on it except the refusal to probe through an excluded parent.
//!
//! # The three answers, and their asymmetry
//!
//! [`Verdict::Indexed`] is PROOF (the index returned the probe). [`Verdict::Excluded`] is a
//! bounded INFERENCE (a miss, relative to a control planted later that DID appear).
//! [`Verdict::Unknown`] exists so the inference is never forced: with no control there is no
//! clock, and a not-yet-indexed file is indistinguishable from an excluded one. Every
//! failure path in [`verify`] lands on `Unknown`, never on `Excluded`.
//!
//! PATHS ARE RESOLVED BEFORE ANYTHING IS MEASURED. [`verify`] canonicalizes, because a
//! probe written through `dir -> /Volumes/fast/real` is indexed at its real path while a
//! query scoped at the link's parent never sees it: both halves would agree — measured,
//! confident, and wrong — that a tree Spotlight is indexing is excluded (measured
//! 2026-09-02).
//!
//! The one test that can catch macOS changing the rules is
//! `manual_probe_round_trip_against_the_real_index` at the bottom of this file; it is
//! `#[ignore]`d because it needs a genuinely indexed location (the developer's own home).
//! Run it after an OS update.
//!
//! Non-macOS is a clean no-op: [`verify`] returns [`Verdict::NotApplicable`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Whether this build can measure Spotlight exposure at all.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// The suffix that makes a directory and its whole subtree invisible to Spotlight
/// (measured, not documented: see the module header).
pub const NOINDEX_SUFFIX: &str = ".noindex";

// ---------------------------------------------------------------------------------------
// What a NAME claims (no I/O)
// ---------------------------------------------------------------------------------------

/// Why a path is, or is not, invisible to Spotlight — read off the PATH ALONE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// Some component ends `.noindex`. The whole subtree below it is excluded.
    NoindexSuffix,
    /// Some component starts with `.`. Transitive to the whole subtree (measured
    /// 2026-09-02, again 2026-10-08 on 27.0).
    DotHidden,
    /// Nothing about the name excludes it. This is a CLAIM about the name, not a
    /// measurement of the index — only [`verify`] measures.
    Exposed,
}

/// What `path`'s NAME claims about its Spotlight visibility. Pure; never touches disk.
/// `NoindexSuffix` wins over `DotHidden` when both are present.
#[must_use]
pub fn exclusion_of(path: &Path) -> Exclusion {
    let mut dot_hidden = false;
    for c in path.components() {
        let std::path::Component::Normal(name) = c else {
            continue;
        };
        // Lossy is safe for both tests: `.` and `.noindex` are ASCII, and the U+FFFD a
        // lossy conversion can introduce is not.
        let name = name.to_string_lossy();
        if name.ends_with(NOINDEX_SUFFIX) {
            return Exclusion::NoindexSuffix;
        }
        if name.starts_with('.') {
            dot_hidden = true;
        }
    }
    if dot_hidden {
        Exclusion::DotHidden
    } else {
        Exclusion::Exposed
    }
}

// ---------------------------------------------------------------------------------------
// Verification — the empirical probe
// ---------------------------------------------------------------------------------------

/// The clocks the probe runs on.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How long to wait for the CONTROL to appear before giving up and answering `Unknown`.
    /// 20 s; the control appeared in 1.25–1.5 s on an idle machine on 2026-09-02, and the
    /// whole reason for the ceiling is the machine that is NOT idle.
    pub control_timeout: Duration,
    /// The FLOOR on the margin after the control appears, before the second candidate
    /// query. 2 s. Never the whole margin: see [`Timing::settle_after`] — on the loaded
    /// machine this module was written for, `mds` can be 15–19 s behind, and a fixed 2 s
    /// there is a tenth of the latency the run just measured.
    pub settle: Duration,
    /// How often to re-ask for the control. 250 ms.
    pub poll: Duration,
}

impl Timing {
    /// The measured defaults. See the field docs for where each number came from.
    pub const DEFAULT: Self = Self {
        control_timeout: Duration::from_secs(20),
        settle: Duration::from_secs(2),
        poll: Duration::from_millis(250),
    };

    /// The margin to wait before the SECOND candidate query, given how long the control
    /// actually took: `max(settle, waited)`.
    ///
    /// The soundness argument for `Excluded` is fsevents ordering, which this module calls
    /// a strong heuristic and not a documented contract (see [`plant`]); the margin is what
    /// buys back the slack. A CONSTANT margin makes that slack a fixed 2 s no matter how
    /// far behind `mds` is — and `waited` is the per-run MEASUREMENT of exactly that. On an
    /// idle machine the control appeared in 1.25–1.48 s (2026-09-02) and 2 s is a ~1.4x
    /// margin; on the machine this module exists for (18 rustc processes, syspolicyd at
    /// 86% CPU, `mds` grinding 2.0 TB) `waited` can legitimately be 15–19 s, where a fixed
    /// 2 s is ~0.1x. The bias is structural, not symmetric: the control is one file in a
    /// brand-new quiescent directory, while the probe is one file in a churning target dir
    /// where fsevents coalescing pushes `mds` into a directory rescan rather than a
    /// single-item import — so the control wins that race hardest exactly when a wrong
    /// answer costs the most. Scaling with `waited` keeps the margin proportional to the
    /// latency the run itself observed.
    #[must_use]
    pub fn settle_after(&self, waited: Duration) -> Duration {
        if waited > self.settle {
            waited
        } else {
            self.settle
        }
    }

    /// The worst-case wall clock of one [`verify`]: the control timeout, plus the largest
    /// margin [`Timing::settle_after`] can ask for (a control that appeared at the very
    /// last poll). What the CLI announces, so a reader is never surprised into ^C.
    #[must_use]
    pub fn worst_case(&self) -> Duration {
        self.control_timeout + self.settle_after(self.control_timeout)
    }

    /// How old a leftover plant must be before [`sweep_killed_litter`] may remove it:
    /// TWICE [`Timing::worst_case`] — about 80 s on the defaults.
    ///
    /// The guard is load-bearing, not decoration. A probe planted by a verify that is STILL
    /// RUNNING — a second `aterm pkg noindex verify` in another terminal, or the app pass
    /// and a human at once — carries the same prefix as a dead run's, and deleting it
    /// mid-flight makes that run's `mdfind` miss. A miss is exactly what [`decide`] reads as
    /// `Excluded`, so a careless sweep would MANUFACTURE the measured, confident false
    /// success this whole module exists to make impossible. Nothing younger than the longest
    /// one verify can take is touched — doubled, because the mtime this is compared against
    /// is the plant's, not the run's, and the run outlives it.
    #[must_use]
    pub fn litter_cutoff(&self) -> Duration {
        self.worst_case() * 2
    }
}

/// The refinement for a control file that never arrived: the switch is
/// [`Unmeasured::IndexingDisabled`], the read-only hold [`Unmeasured::IndexReadOnly`].
fn refine_unindexed(
    state: Option<crate::platform::IndexState>,
    scope: &Path,
) -> Option<Unmeasured> {
    use crate::platform::IndexState;
    match state? {
        IndexState::Disabled => Some(Unmeasured::IndexingDisabled(scope.to_path_buf())),
        IndexState::ReadOnly => Some(Unmeasured::IndexReadOnly(scope.to_path_buf())),
        IndexState::Enabled => None,
    }
}

/// Why exclusion could not be MEASURED. None of these ever reads as "excluded".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unmeasured {
    /// The control file never showed up in the index. Without it there is no clock, so a
    /// probe miss proves nothing. Usually: Spotlight is off for this volume, the parent is
    /// itself inside an excluded or dot-hidden ancestor, or the machine is saturated.
    ControlNeverIndexed {
        /// How long the control was waited for, in seconds.
        waited_secs: u64,
    },
    /// `mdutil -s` says indexing is off for this volume. A refinement of the message only —
    /// the verdict is `Unknown` either way.
    IndexingDisabled(PathBuf),
    /// `mdutil -s` says the volume's index is read-only: a planted probe never arrives, as
    /// under the switch, but nothing about the directory was decided. Message only.
    IndexReadOnly(PathBuf),
    /// `dir` has no parent, so there is nowhere to plant a same-volume control.
    NoParent(PathBuf),
    /// The scope both queries would be pointed at is ITSELF excluded by name, so `mdfind`
    /// would answer from a live scan rather than from the index and every hit would be a
    /// false `Indexed`. Nothing under such a parent is exposed anyway.
    ScopeExcluded(PathBuf),
    /// A probe file could not be planted or `mdfind` could not be asked.
    Io(String),
}

/// The answer. See the module header for the asymmetry between these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// MEASURED: the probe was absent from the index while a control planted AFTER it was
    /// present. A bounded inference, not proof.
    Excluded,
    /// PROOF: the index returned the probe. This directory IS being indexed.
    Indexed,
    /// Not measured, and deliberately not guessed.
    Unknown(Unmeasured),
    /// Not macOS.
    NotApplicable,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Excluded => f.write_str(
                "excluded — a probe stayed out of the index while a later control appeared",
            ),
            Self::Indexed => f.write_str("INDEXED — Spotlight is walking this tree"),
            Self::Unknown(Unmeasured::ControlNeverIndexed { waited_secs }) => write!(
                f,
                "unknown — the control was not indexed within {waited_secs}s; re-run when the \
                 machine is less busy"
            ),
            Self::Unknown(Unmeasured::IndexingDisabled(p)) => write!(
                f,
                "unknown — Spotlight indexing is off for the volume holding {}, so nothing \
                 there is indexed",
                p.display()
            ),
            Self::Unknown(Unmeasured::IndexReadOnly(p)) => write!(
                f,
                "unknown — the Spotlight index of the volume holding {} is read-only (low on \
                 space); free space, then measure again",
                p.display()
            ),
            Self::Unknown(Unmeasured::ScopeExcluded(p)) => write!(
                f,
                "unknown — the parent {} is already excluded by name (a `.noindex` or \
                 dot-hidden ancestor), so nothing below it is indexed",
                p.display()
            ),
            Self::Unknown(Unmeasured::NoParent(p)) => write!(
                f,
                "unknown — {} has no parent for a control; name a directory, not a volume \
                 root",
                p.display()
            ),
            Self::Unknown(Unmeasured::Io(e)) => write!(
                f,
                "unknown — the probe could not be run ({e}); check the directory is writable"
            ),
            Self::NotApplicable => f.write_str("not applicable — no Spotlight on this platform"),
        }
    }
}

/// The pure three-state decision, given ONLY what the queries returned — extracted so the
/// race logic is unit-tested without a live index.
///
/// * control never seen -> `Unknown(ControlNeverIndexed)`. NEVER `Excluded`.
/// * either candidate answer is a hit -> `Indexed`. A hit is proof and short-circuits.
/// * either candidate answer is `None` (mdfind could not be asked) -> `Unknown(Io)`.
/// * both candidate answers are a clean miss -> `Excluded`.
#[must_use]
pub fn decide(
    control_seen: bool,
    waited: Duration,
    candidate_first: Option<bool>,
    candidate_second: Option<bool>,
) -> Verdict {
    if !control_seen {
        return Verdict::Unknown(Unmeasured::ControlNeverIndexed {
            waited_secs: waited.as_secs(),
        });
    }
    if candidate_first == Some(true) || candidate_second == Some(true) {
        return Verdict::Indexed;
    }
    if candidate_first.is_none() || candidate_second.is_none() {
        return Verdict::Unknown(Unmeasured::Io(String::from(
            "mdfind could not be asked about the probe",
        )));
    }
    Verdict::Excluded
}

/// The prefix every probe file carries, and the second half of every control directory's
/// name. ONE spelling, because [`sweep_killed_litter`] recognizes a dead run's leftovers by
/// it: a sweep that knew a different string from the planter would either miss the litter
/// or claim a name this module never wrote.
const PROBE_PREFIX: &str = "atpkgnoindexprobe";

/// The prefix of the control DIRECTORY planted beside a candidate — the token follows it,
/// so the whole name is `atpkg-control-<token>`. PLAIN on purpose (see [`verify`]), which
/// is also why a killed run's copy is what a user sees in `git status`.
const CONTROL_DIR_PREFIX: &str = "atpkg-control-";

/// One-word probe token: `atpkgnoindexprobe` + hex nanos + hex pid. Pure, so its
/// query-safety is pinned by a test. `[a-z0-9]+` by construction, which is what lets it be
/// interpolated into an `mdfind` predicate with no quoting question, and what stops
/// Spotlight's tokenizer from splitting it.
#[must_use]
pub fn probe_token(nanos: u128, pid: u32) -> String {
    format!("{PROBE_PREFIX}{nanos:016x}{pid:08x}")
}

/// The name of the control file that goes with `token` — a DIFFERENT exact name, so the
/// `kMDItemFSName ==` query for one can never answer for the other.
fn control_file_name(token: &str) -> String {
    let mut s = String::from(token);
    s.push_str("ctl");
    s
}

/// The directory an `mdfind` query for `dir`'s contents must be SCOPED at: `dir.parent()`.
///
/// NEVER `dir` itself. MEASURED 2026-09-02 on macOS 26.6.2: with a probe file planted in
/// `~/mdx-probe-scratch/target.noindex/`,
///   `mdfind -onlyin ~/mdx-probe-scratch/target.noindex 'kMDItemFSName == "<probe>"'`
/// PRINTED THE FILE, while the same predicate scoped at `~/mdx-probe-scratch`, at `~`, and
/// unscoped all returned nothing. Pointing `-onlyin` AT an excluded directory makes `mdfind`
/// answer from a live scan rather than from the index, and a utility that scoped its query
/// the obvious way would have reported every correctly-excluded directory as `Indexed` —
/// the mirror image of the `.metadata_never_index` false success this feature exists to
/// prevent. Scoping both queries at the shared parent also makes them symmetric: the control
/// proves that exact scope is indexed.
#[must_use]
pub fn verify_scope(dir: &Path) -> Option<&Path> {
    match dir.parent() {
        // `Path::new("target").parent()` is `Some("")`, NOT `None` — per std's own
        // documentation only a path terminating in a root or a prefix gives `None`. The
        // empty path is not a directory, and MEASURED 2026-09-02 on macOS 26.6.2, with an
        // indexed file present:
        //   mdfind -onlyin ''  'kMDItemFSName == "<token>"'  ->  '' , exit 0
        //   mdfind -onlyin .   'kMDItemFSName == "<token>"'  ->  /Users/…/<token>
        // Exit 0 with empty stdout is a CLEAN MISS to `spotlight_query`, so an empty scope
        // makes even the control unfindable: `aterm pkg noindex verify target` would burn
        // the whole timeout and answer `Unknown` on a perfectly idle machine, advising a
        // re-run that can never work. `.` is the directory the empty parent MEANS.
        // [`verify`] canonicalizes before it reaches here; this keeps the pure, public
        // function right for every other caller.
        Some(p) if p.as_os_str().is_empty() => Some(Path::new(".")),
        other => other,
    }
}

/// MEASURE whether `dir` is excluded from Spotlight. See [`decide`] for the answers and the
/// module header for why a bare probe would be worthless.
///
/// `dir` is CANONICALIZED first, and everything below — the scope, the probe, the control
/// — is derived from the real path. Plants a probe file in it, then a control file in a
/// plain-named sibling directory under its parent — PROBE FIRST, CONTROL SECOND, and that
/// order is the whole mechanism (see [`plant`]). Waits for the control, then queries the
/// candidate twice with a margin between that scales with how long the control took
/// ([`Timing::settle_after`]). Both queries are scoped at the shared parent
/// ([`verify_scope`]). Every probe file and the control directory are removed by a `Drop`
/// guard armed BEFORE anything is written, so a panic, an early return, or a failure
/// halfway through planting cannot litter a user's repo — and because a `Drop` guard is
/// precisely what a SIGNAL does not run, this also SWEEPS what a killed earlier run left
/// (`^C` through the announced wait, a SIGTERM, a closed terminal tab) before planting
/// anything of its own. See [`sweep_killed_litter`].
///
/// On non-macOS returns [`Verdict::NotApplicable`] without touching the disk.
#[must_use]
pub fn verify(dir: &Path, timing: &Timing) -> Verdict {
    if !SUPPORTED {
        return Verdict::NotApplicable;
    }
    // CANONICALIZE FIRST — the probe and the query must be about the same directory.
    //
    // `plant` writes THROUGH `dir`, so with `repo/target -> /Volumes/fast/realtarget` the
    // probe's real, indexed path is under `/Volumes/fast`, while a scope taken from the
    // LINK's parent is `repo`. MEASURED 2026-09-02 on macOS 26.6.2 with exactly that shape:
    //   mdfind (unscoped)          -> /Users/…/elsewhere/realtarget/<token>   (INDEXED)
    //   mdfind -onlyin …/repo      -> ''
    // while the control, a real path under `repo`, indexed in ~1.5 s. Control seen, both
    // candidate queries a clean miss, verdict `Excluded` — a MEASURED, confident false
    // negative for a tree Spotlight was actively indexing, which is the exact laundered
    // answer this module exists to make impossible. `ln -s /Volumes/fast/target target` is
    // what someone with 2 TB of build output actually does, so this is not a corner.
    // Canonicalizing also makes a relative operand absolute, which is what keeps
    // `verify_scope` off the empty path.
    let dir = match std::fs::canonicalize(dir) {
        Ok(real) => real,
        Err(e) => return Verdict::Unknown(Unmeasured::Io(e.to_string())),
    };
    let dir = dir.as_path();
    let Some(scope) = verify_scope(dir) else {
        return Verdict::Unknown(Unmeasured::NoParent(dir.to_path_buf()));
    };
    // SWEEP WHAT A KILLED RUN LEFT, FIRST — before the scope check below, because leftovers
    // under an excluded parent are still leftovers in the user's tree, and this sweep is
    // hygiene that no verdict depends on.
    //
    // `Cleanup` (armed further down) covers every path the PROCESS TAKES. A signal is not
    // one of them: atpkg installs no handler, so the default action for SIGINT — the `^C`
    // an impatient reader presses partway through the up-to-40 s wait the CLI announces —
    // and for SIGTERM or the SIGHUP of a closed tab ends the process without unwinding, and
    // `Drop` never runs. Before this sweep NOTHING in the crate ever removed either plant
    // again, so `<target>/atpkgnoindexprobe<hex>` stayed put and the plainly-named
    // `<parent>/atpkg-control-atpkgnoindexprobe<hex>/` — the parent being the repo root in
    // the case this feature is for — read as an untracked `??` row in `git status` for good.
    let stale = timing.litter_cutoff();
    sweep_killed_litter(dir, stale);
    sweep_killed_litter(scope, stale);
    // The scope must itself be indexABLE, or both queries are meaningless in the dangerous
    // direction. The doc on `verify_scope` records the measurement: pointing `-onlyin` at
    // an excluded directory makes `mdfind` answer from a LIVE SCAN rather than from the
    // index. For `verify ~/build.noindex/target` the scope is `~/build.noindex`, so the
    // control is found instantly, the probe is found by the same live scan, and the verdict
    // is `Indexed` for a doubly-excluded tree. The control-directory NAME is already
    // guarded against this trap below; the scope must be too.
    if exclusion_of(scope) != Exclusion::Exposed {
        return Verdict::Unknown(Unmeasured::ScopeExcluded(scope.to_path_buf()));
    }
    let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos());
    let token = probe_token(nanos, std::process::id());
    let control_name = control_file_name(&token);
    let mut control_dir_name = String::from("atpkg-control-");
    control_dir_name.push_str(&token);
    // A PLAIN name: no leading dot, and it must not end `.noindex`, or the control would be
    // excluded too and the clock would never start.
    let control_dir = scope.join(control_dir_name);

    // ARM THE GUARD BEFORE ANYTHING IS WRITTEN. Both paths are known without planting, and
    // `plant` writes the probe BEFORE it creates the control directory — so a guard built
    // from `plant`'s return value does not exist on the path where the probe landed and the
    // control write failed (ENOSPC on the 2 TB machine this feature targets, or a parent
    // the user cannot write). CONFIRMED: with the parent chmod 0o500, `verify` returned
    // `Unknown(Io("Permission denied"))` and left an `atpkgnoindexprobe<hex>` file inside
    // the user's target dir forever — exactly the litter this module's doc promises against.
    // Both removals are no-ops for something that was never created.
    let _cleanup = Cleanup {
        probe: dir.join(&token),
        control_dir: control_dir.clone(),
    };
    if let Err(e) = plant(dir, &control_dir, &token) {
        return Verdict::Unknown(Unmeasured::Io(e.to_string()));
    }

    // THE RACE, and why the control exists.
    //
    // A miss from `mdfind` has two possible causes that look identical from here: the file
    // is excluded, or the indexer has simply not reached it yet. `mds` is asynchronous and
    // on a loaded machine (the machine this whole module exists for) it can be many seconds
    // behind. So the probe alone can only ever produce a guess.
    //
    // The control is the clock. It is written AFTER the probe, into a plainly-named sibling
    // under the SAME parent — same volume, same fsevents stream. When the control turns up
    // in the index, the indexer has already been offered the probe's earlier event. Only
    // then is the probe's absence evidence, and even then it is an INFERENCE, never proof:
    // hence the settle margin and the second query below, and hence `Unknown` when the
    // control never arrives at all.
    let start = Instant::now();
    let mut control_seen = false;
    while start.elapsed() < timing.control_timeout {
        if indexed(scope, &control_name) == Some(true) {
            control_seen = true;
            break;
        }
        std::thread::sleep(timing.poll);
    }
    let waited = start.elapsed();

    if !control_seen {
        // The verdict is `Unknown` either way; asking `mdutil` only refines WHICH sentence
        // the user reads, so its own failure changes nothing.
        if let Some(why) = refine_unindexed(crate::platform::spotlight_index_state(scope), scope) {
            return Verdict::Unknown(why);
        }
        return decide(false, waited, None, None);
    }

    let first = indexed(scope, &token);
    if first == Some(true) {
        // A hit is proof; there is nothing a second query could add.
        return decide(true, waited, first, None);
    }
    // The margin scales with the latency this run just measured, not with a constant that
    // was calibrated on an idle machine — see [`Timing::settle_after`].
    std::thread::sleep(timing.settle_after(waited));
    let second = indexed(scope, &token);
    decide(true, waited, first, second)
}

/// Plant the probe, then the control — IN THAT ORDER, which is why this is one function and
/// not two calls at the call site.
///
/// A miss is only evidence relative to something that WAS indexed later. Because the probe's
/// write lands on the volume's fsevents stream strictly before the control's, the control
/// becoming visible means the indexer has already been offered the probe's event. That
/// per-volume ordering is a strong heuristic, not a documented contract (fsevents coalesce
/// per directory and `mds` has more than one worker), which is what the [`Timing::settle`]
/// margin and the SECOND candidate query buy. If the heuristic ever breaks, the error lands
/// on the safe side: a late-arriving probe reads as `Indexed`, a false alarm the user
/// re-runs. The dangerous direction — a false `Excluded` — needs the probe's event to be
/// delayed past BOTH the control and the settle.
fn plant(dir: &Path, control_dir: &Path, token: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    let probe = dir.join(token);
    // Real bytes, not an empty file: an importer with nothing to import is one more way a
    // miss could mean something other than "excluded".
    std::fs::write(&probe, b"atpkg noindex probe\n")?;
    std::fs::create_dir_all(control_dir)?;
    let control = control_dir.join(control_file_name(token));
    std::fs::write(&control, b"atpkg noindex control\n")?;
    Ok((probe, control))
}

/// Removes the probe file and the whole control directory on drop, on every path the
/// process TAKES — a panic, an early return, a failure halfway through planting. A signal
/// is not one of those paths ([`sweep_killed_litter`] is what covers those).
struct Cleanup {
    /// The probe file planted inside the candidate directory.
    probe: PathBuf,
    /// The control directory this module created, removed whole.
    control_dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // Both are best-effort and both are unconditional: a probe left behind in someone's
        // repo is exactly the litter that makes a hygiene tool untrusted.
        let _ = std::fs::remove_file(&self.probe);
        let _ = std::fs::remove_dir_all(&self.control_dir);
    }
}

/// Remove what a KILLED [`verify`] left in `dir`: its `atpkgnoindexprobe…` probe file, and
/// the `atpkg-control-atpkgnoindexprobe…` directory it plants beside a candidate. Only
/// entries at least `min_age` old, only names this module itself generates, and never
/// anything reached by following a link out of `dir`. Returns how many were removed.
///
/// [`Cleanup`] is a `Drop` guard, and `Drop` is exactly what does not run when a signal
/// ends the process: atpkg installs no SIGINT/SIGTERM/SIGHUP handler, so each keeps its
/// default action, which terminates without unwinding. That is not a rare shape — [`verify`]
/// can legitimately take 40 s ([`Timing::worst_case`]) and the CLI says so up front
/// precisely because a silent wait invites `^C`. Announcing the wait makes an impatient
/// interrupt less likely; it does not make a SIGTERM, a GUI quit, or a closed terminal tab
/// stop happening. So the leftovers are swept by the NEXT run instead: by [`verify`] before
/// it plants.
///
/// `min_age` is what keeps this safe against a CONCURRENT verify — see
/// [`Timing::litter_cutoff`], where the whole argument is. Best-effort throughout: an
/// unreadable directory, a missing timestamp or a failed removal is nothing to report, and
/// every one of those cases LEAVES the entry rather than guessing.
fn sweep_killed_litter(dir: &Path, min_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut swept = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // The control DIRECTORY must carry BOTH prefixes. `atpkg-control-` alone is not a
        // claim this module can make about someone else's directory.
        let ours = name.starts_with(PROBE_PREFIX)
            || name
                .strip_prefix(CONTROL_DIR_PREFIX)
                .is_some_and(|rest| rest.starts_with(PROBE_PREFIX));
        if !ours {
            continue;
        }
        let path = entry.path();
        // `symlink_metadata`, so a link standing under one of these names is removed AS the
        // link and this sweep never deletes through it.
        let Ok(md) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let Ok(modified) = md.modified() else {
            continue;
        };
        // An mtime in the future (a clock that moved, a copied tree) gives `Err` here and
        // is KEPT: unknown age is never young enough to delete.
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < min_age {
            continue;
        }
        let removed = if md.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if removed {
            swept += 1;
        }
    }
    swept
}

/// One `mdfind` question: is a file named `filename` in the index under `scope`?
/// `None` means the question could not be asked and must never read as "no".
fn indexed(scope: &Path, filename: &str) -> Option<bool> {
    crate::platform::spotlight_query(scope, filename)
}

// ---------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    /// A synthetic root under `temp_dir()`, removed when the test ends. Nothing here reads
    /// the environment or the user's real home, and no test below queries Spotlight: on
    /// macOS `temp_dir()` is `/private/var/folders/…/T/`, which is typically not indexed at
    /// all, so an index query there would be meaningless AND flaky. The race logic is
    /// tested through [`decide`].
    fn scratch(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-noindex-{label}-"))
    }

    /// A directory with one file in it, standing in for any directory a probe measures.
    fn candidate(at: &Path) {
        std::fs::create_dir_all(at).unwrap();
        std::fs::write(at.join("Cargo.toml"), b"[package]\n").unwrap();
    }

    #[test]
    fn exclusion_reads_the_name_never_the_index() {
        assert_eq!(exclusion_of(Path::new("/a/b/target")), Exclusion::Exposed);
        assert_eq!(
            exclusion_of(Path::new("/a/b/target.noindex")),
            Exclusion::NoindexSuffix
        );
        // The measured whole-subtree row: a file six levels down inside a `.noindex`
        // directory was not indexed either (2026-09-02).
        assert_eq!(
            exclusion_of(Path::new("/a/b/target.noindex/deep/6/levels/down/x")),
            Exclusion::NoindexSuffix
        );
        // The measured transitive dot row, re-measured 2026-10-08 on macOS 27.0.
        assert_eq!(
            exclusion_of(Path::new("/a/.cargo-target-m7c/debug")),
            Exclusion::DotHidden
        );
        // A dot in the MIDDLE does nothing — the row a naive `contains('.')` gets wrong.
        assert_eq!(
            exclusion_of(Path::new("/a/d_.subdot/target")),
            Exclusion::Exposed
        );
    }

    #[test]
    fn the_query_is_scoped_at_the_parent_never_at_the_candidate() {
        // The regression test for the MEASURED false positive (2026-09-02, macOS 26.6.2):
        //   mdfind -onlyin ~/mdx-probe-scratch/target.noindex 'kMDItemFSName == "<probe>"'
        // PRINTED the file, while the same predicate scoped at ~/mdx-probe-scratch, at ~,
        // and unscoped all returned nothing. Pointing -onlyin AT an excluded directory makes
        // mdfind answer from a live scan instead of the index, so scoping at the candidate
        // would have reported every correctly-excluded directory as Indexed.
        assert_eq!(
            verify_scope(Path::new("/a/b/target.noindex")),
            Some(Path::new("/a/b"))
        );
        assert_eq!(verify_scope(Path::new("/")), None);
    }

    #[test]
    fn probe_token_is_one_query_safe_word() {
        let t = probe_token(1_756_000_000_123_456_789, 4242);
        assert!(
            t.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "the token is interpolated into an mdfind predicate with no quoting: {t}"
        );
        assert!((20..=48).contains(&t.len()), "{t} has length {}", t.len());
        for bad in ['"', ' ', '.', '-'] {
            assert!(
                !t.contains(bad),
                "{bad:?} would either need quoting or let Spotlight's tokenizer split the \
                 name: {t}"
            );
        }
        assert_ne!(
            t,
            probe_token(1_756_000_000_123_456_790, 4242),
            "two probes in the same process must not collide"
        );
    }

    #[test]
    fn decide_never_infers_exclusion_without_a_control() {
        // The whole point of the control. Without it a miss is indistinguishable from a
        // file the indexer has not reached yet.
        let v = decide(false, Duration::from_secs(20), Some(false), Some(false));
        assert!(
            matches!(
                v,
                Verdict::Unknown(Unmeasured::ControlNeverIndexed { waited_secs: 20 })
            ),
            "{v}"
        );
    }

    #[test]
    fn decide_is_asymmetric_a_hit_is_proof_a_miss_is_an_inference() {
        let d = Duration::from_secs(2);
        assert!(
            matches!(decide(true, d, Some(true), None), Verdict::Indexed),
            "a hit short-circuits"
        );
        assert!(
            matches!(decide(true, d, Some(false), Some(true)), Verdict::Indexed),
            "a late hit is still proof"
        );
        let v = decide(true, d, None, None);
        assert!(matches!(v, Verdict::Unknown(Unmeasured::Io(_))), "{v}");
        let v = decide(true, d, Some(false), None);
        assert!(
            matches!(v, Verdict::Unknown(Unmeasured::Io(_))),
            "an unaskable question is never a miss: {v}"
        );
        let v = decide(true, d, Some(false), Some(false));
        assert!(matches!(v, Verdict::Excluded), "{v}");
    }

    /// An unindexed volume is refined by its state: indexing switched off and a read-only
    /// index are different reasons, and an indexed or unreadable state refines nothing.
    #[test]
    fn the_unindexed_refinement_maps_each_volume_state() {
        use crate::platform::IndexState;
        let scope = Path::new("/Users//x");
        assert_eq!(
            refine_unindexed(Some(IndexState::Disabled), scope),
            Some(Unmeasured::IndexingDisabled(scope.to_path_buf()))
        );
        assert_eq!(
            refine_unindexed(Some(IndexState::ReadOnly), scope),
            Some(Unmeasured::IndexReadOnly(scope.to_path_buf()))
        );
        assert_eq!(refine_unindexed(Some(IndexState::Enabled), scope), None);
        assert_eq!(refine_unindexed(None, scope), None);
    }

    #[test]
    fn the_probe_is_planted_before_the_control_so_a_control_hit_bounds_the_probes_chance() {
        // The ordering IS the mechanism: the probe's write reaches the volume's fsevents
        // stream strictly before the control's, so a control that turns up in the index
        // means the indexer has already been offered the probe's event. That ordering
        // cannot be asserted at runtime inside `verify`, so it is asserted where it is
        // created. APFS timestamps are nanosecond, so `<=` holds.
        let root = scratch("plant-order");
        let candidate = root.join("target");
        std::fs::create_dir_all(&candidate).unwrap();
        let control_dir = root.join("atpkg-control-probe");
        let (probe, control) = plant(&candidate, &control_dir, "atpkgnoindexprobe0000").unwrap();
        let p = std::fs::metadata(&probe).unwrap().modified().unwrap();
        let c = std::fs::metadata(&control).unwrap().modified().unwrap();
        assert!(p <= c, "probe first, control second — always");
        assert_ne!(
            probe.file_name(),
            control.file_name(),
            "the two exact-name queries must never answer for each other"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Backdate `path`'s mtime by `by`, so an age-guarded sweep sees a leftover from a run
    /// that cannot still be running. `utimes(2)` takes a PATH, which is what makes it work
    /// on the control DIRECTORY as well as on the probe file.
    #[cfg(unix)]
    fn backdate(path: &Path, by: Duration) {
        use std::os::unix::ffi::OsStrExt as _;
        let when = std::time::SystemTime::now() - by;
        let secs = when
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let tv = libc::timeval {
            tv_sec: libc::time_t::try_from(secs).unwrap(),
            tv_usec: 0,
        };
        let times = [tv, tv];
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) }, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_sweep_removes_a_killed_runs_litter_and_never_a_live_probe() {
        // `Cleanup` is a `Drop` guard and a signal does not run `Drop`: `^C` partway through
        // the up-to-40 s wait, a SIGTERM, a closed tab — each ends the process without
        // unwinding, leaving the probe inside the target dir and a PLAIN-named
        // `atpkg-control-<token>/` in the parent, which is usually the repo root and which
        // then reads as an untracked `??` row in `git status` for good.
        let root = scratch("sweep");
        let target = root.join("target");
        candidate(&target);

        let dead = probe_token(1, 1);
        let stale_probe = target.join(&dead);
        std::fs::write(&stale_probe, b"probe").unwrap();
        let stale_control = root.join(format!("{CONTROL_DIR_PREFIX}{dead}"));
        std::fs::create_dir_all(&stale_control).unwrap();
        std::fs::write(stale_control.join(control_file_name(&dead)), b"control").unwrap();
        let older_than_any_run = Timing::DEFAULT.litter_cutoff() * 2;
        backdate(&stale_probe, older_than_any_run);
        backdate(&stale_control, older_than_any_run);

        // A CONCURRENT verify's plants, fresh on disk this instant.
        let live = probe_token(2, 2);
        let live_probe = target.join(&live);
        std::fs::write(&live_probe, b"probe").unwrap();
        let live_control = root.join(format!("{CONTROL_DIR_PREFIX}{live}"));
        std::fs::create_dir_all(&live_control).unwrap();

        // The user's own, old, and none of this module's business whatever it is named.
        let build_output = target.join("libthing.rlib");
        std::fs::write(&build_output, b"payload").unwrap();
        backdate(&build_output, older_than_any_run);
        let near_miss = root.join("atpkg-control-something-of-someone-elses");
        std::fs::create_dir_all(&near_miss).unwrap();
        backdate(&near_miss, older_than_any_run);

        let cutoff = Timing::DEFAULT.litter_cutoff();
        assert_eq!(sweep_killed_litter(&target, cutoff), 1);
        assert_eq!(sweep_killed_litter(&root, cutoff), 1);

        assert!(
            !stale_probe.exists() && !stale_control.exists(),
            "a probe and a control dir left in someone's repo are exactly the litter that \
             makes a hygiene tool untrusted"
        );
        assert!(
            live_probe.exists() && live_control.exists(),
            "deleting a CONCURRENT run's probe turns its `mdfind` miss into a measured, \
             confident `Excluded` — the laundered false success this module exists to prevent"
        );
        assert!(
            build_output.exists() && near_miss.exists(),
            "only names this module itself plants: `atpkg-control-` alone is not a claim \
             over someone else's directory"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn verify_sweeps_a_killed_runs_litter_before_it_plants_its_own() {
        // The wiring, measured on the one `verify` path that answers without asking
        // Spotlight anything: a scope excluded BY NAME returns `ScopeExcluded` at once, so
        // this test costs no `mdfind` and no 20 s wait. The sweep still has to have run —
        // leftovers under an excluded parent are still leftovers in the user's tree, which
        // is why it sits ABOVE that return and not after it.
        let root = scratch("sweep-verify");
        let scope = root.join("build.noindex");
        let target = scope.join("target");
        candidate(&target);

        let dead = probe_token(3, 3);
        let probe = target.join(&dead);
        std::fs::write(&probe, b"probe").unwrap();
        let control = scope.join(format!("{CONTROL_DIR_PREFIX}{dead}"));
        std::fs::create_dir_all(&control).unwrap();
        let older_than_any_run = Timing::DEFAULT.litter_cutoff() * 2;
        backdate(&probe, older_than_any_run);
        backdate(&control, older_than_any_run);

        let v = verify(&target, &Timing::DEFAULT);
        assert!(
            matches!(v, Verdict::Unknown(Unmeasured::ScopeExcluded(_))),
            "this test wants the path that measures nothing — any other verdict means it \
             probed the live index: {v}"
        );
        assert!(
            !probe.exists() && !control.exists(),
            "a `Drop` guard cannot run on a signal, so the next verify is what has to clear \
             the last one's litter"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cleanup_guard_removes_the_probe_even_on_an_early_return() {
        let root = scratch("cleanup");
        let candidate = root.join("target");
        std::fs::create_dir_all(&candidate).unwrap();
        let control_dir = root.join("atpkg-control-probe");
        let (probe, control) = plant(&candidate, &control_dir, "atpkgnoindexprobe0001").unwrap();
        {
            let _guard = Cleanup {
                probe: probe.clone(),
                control_dir: control_dir.clone(),
            };
        }
        assert!(
            !probe.exists(),
            "a probe left in someone's repo is exactly the litter that makes a hygiene tool \
             untrusted"
        );
        assert!(
            !control.exists() && !control_dir.exists(),
            "the control dir goes whole"
        );
        assert!(candidate.is_dir(), "and nothing else is touched");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_scope_never_answers_the_empty_path() {
        // `Path::new("target").parent()` is `Some("")`, and `mdfind -onlyin ''` is a clean
        // MISS (measured 2026-09-02), so the empty scope would make even the control
        // unfindable — `verify target` would burn the whole timeout and answer `Unknown`
        // on an idle machine, forever.
        assert_eq!(verify_scope(Path::new("target")), Some(Path::new(".")));
        assert_eq!(verify_scope(Path::new("./target")), Some(Path::new(".")));
        assert_eq!(verify_scope(Path::new(".")), Some(Path::new(".")));
        assert_eq!(
            verify_scope(Path::new("/a/b/target")),
            Some(Path::new("/a/b")),
            "an absolute path is untouched"
        );
    }

    #[test]
    fn the_settle_margin_scales_with_the_latency_the_run_measured() {
        let t = Timing::DEFAULT;
        // The idle machine: the floor governs (control seen in 1.25-1.48 s on 2026-09-02).
        assert_eq!(t.settle_after(Duration::from_millis(1400)), t.settle);
        // The machine this module exists for: 18 rustc processes and `mds` grinding 2.0 TB.
        // A fixed 2 s there is ~0.1x the latency the run just measured, and the margin is
        // the ONLY thing standing between an fsevents-ordering heuristic and a false
        // `Excluded`.
        assert_eq!(
            t.settle_after(Duration::from_secs(18)),
            Duration::from_secs(18)
        );
        assert_eq!(
            t.worst_case(),
            Duration::from_secs(40),
            "what the CLI must announce, so a reader is never surprised into ^C"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn verify_refuses_to_measure_through_an_excluded_parent() {
        // MEASURED: pointing `-onlyin` AT an excluded directory makes `mdfind` answer from
        // a live scan rather than from the index, so the probe would be "found" and the
        // verdict would be `Indexed` for a doubly-excluded tree. No probe is planted here at
        // all, so this is fast and touches nothing.
        let root = scratch("verify-excluded-scope");
        let target = root.join("build.noindex/target");
        candidate(&target);
        let v = verify(&target, &Timing::DEFAULT);
        let Verdict::Unknown(Unmeasured::ScopeExcluded(p)) = &v else {
            panic!("an excluded scope can only answer Unknown: {v}");
        };
        assert_eq!(
            p,
            &std::fs::canonicalize(root.join("build.noindex")).unwrap()
        );
        assert!(
            !v.to_string().contains("INDEXED"),
            "the one answer this must never give: {v}"
        );
        assert_eq!(
            std::fs::read_dir(&target).unwrap().count(),
            1,
            "nothing was planted — only the candidate's own file is in there"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_plant_that_fails_halfway_still_leaves_no_probe_behind() {
        // `plant` writes the PROBE first and the control second, so a guard built from its
        // return value does not exist on the path where the probe landed and the control
        // write failed — ENOSPC on the 2 TB machine this targets, or an unwritable parent.
        // The probe is then permanent litter inside the user's target dir, which is
        // exactly what this module's doc promises against.
        if crate::platform::our_uid() == 0 {
            return; // root bypasses the mode bits this test depends on
        }
        let root = scratch("plant-halfway");
        let target = root.join("target");
        candidate(&target);
        // r-x: the probe can still be written INTO `target`, but the control directory
        // cannot be created beside it.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();

        let v = verify(&target, &Timing::DEFAULT);
        assert!(
            matches!(v, Verdict::Unknown(Unmeasured::Io(_))),
            "a probe that could not be run is never a measurement — and any OTHER Unknown \
             here means this test never reached the plant it is about: {v}"
        );
        let litter: Vec<PathBuf> = std::fs::read_dir(&target)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("atpkgnoindexprobe"))
            })
            .collect();
        assert!(
            litter.is_empty(),
            "a probe left in someone's repo is exactly the litter that makes a hygiene \
             tool untrusted: {litter:?}"
        );

        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn every_entry_point_is_an_honest_no_op() {
        const { assert!(!SUPPORTED) };
        let root = scratch("noop");
        let target = root.join("target");
        candidate(&target);

        assert_eq!(verify(&target, &Timing::DEFAULT), Verdict::NotApplicable);
        assert_eq!(
            std::fs::read_dir(&target).unwrap().count(),
            1,
            "a no-op plants nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The ONLY test that can catch macOS changing the `.noindex` behaviour this whole
    /// module rests on — `.noindex` is observed on this machine, not a documented API. It is
    /// `#[ignore]`d because it needs a genuinely indexed location (the invoking user's real
    /// home; `temp_dir()` is typically not indexed at all) and takes seconds. Run it after
    /// an OS update:
    /// `targo --unverified test -p atpkg --lib manual_probe_round_trip -- --ignored --nocapture`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual probe: needs a live Spotlight index and writes under the invoking user's home"]
    fn manual_probe_round_trip_against_the_real_index() {
        let Some(home) = aterm_types::dirs::home_dir() else {
            return;
        };
        let root = home.join(format!("atpkg-noindex-manual-{}", std::process::id()));
        let plain = root.join("plain");
        let hidden = root.join("hidden.noindex");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&hidden).unwrap();

        let v = verify(&plain, &Timing::DEFAULT);
        assert!(
            matches!(v, Verdict::Indexed),
            "a plainly-named directory under an indexed home IS indexed: {v}"
        );
        let v = verify(&hidden, &Timing::DEFAULT);
        assert!(
            matches!(v, Verdict::Excluded),
            "a .noindex directory is not — if this fails, macOS changed the behaviour this \
             module measures: {v}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
