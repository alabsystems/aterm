// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE GATE DECIDED ABOUT ONE TREE, written where a tool can read it in
//! milliseconds.
//!
//! WHO READS IT, TODAY (2026-09-26): the next run's differential verdict and
//! the release cutter, both INLINE. A run is judged against main's receipt for
//! its base ([`crate::differential`]): a red that receipt lists with the same
//! failure is inherited, not blocking. `crates/aterm-release/src/gates.rs`'s
//! `receipt_report` walks the commit a cut builds back to the newest one a
//! receipt vouches for and states the ungated range — and the reds that
//! commit inherited — in the cut's transcript, before the ledger claim. The
//! gate lives in the tool being run, which is where the owner put every
//! quality gate: "I DONT WANT HOOKS! NO HOOKS NO CI" (2026-07-06). No git hook
//! reads a receipt; there is none.
//!
//! WHY RECEIPTS EXIST (2026-09-17). aterm has no CI by owner decision: the
//! merge contract is `tools/verify.sh` passing locally before a slice
//! enters main. A `.githooks/pre-push`, re-added on 2026-07-16 without the
//! owner's sign-off, had been ADVISORY since 2026-08-24, and receipts were
//! first written so that hook could gate again by CHECKING A RESULT instead
//! of running the gate. The hook was deleted on 2026-09-25, restoring the
//! mandate; the receipts stayed, because what they record — the tree a run
//! verified (`identity::TreeState`) and whether it discharged the whole merge
//! contract ([`crate::verdict::Verdict::claims_merge_contract`]), keyed by the
//! commit and its tree — is exactly what a reader after the fact needs, and it
//! costs two small files per run.
//!
//! WHAT A RECEIPT IS NOT. It is a record the gate wrote, not evidence anyone
//! can check — the same standing the snapshot marker has, and for the same
//! reason (`crate::snapshot::marker_state`): whoever can write a file can write
//! one. It records the accident it is about — a commit no gate ever ran on —
//! and claims nothing against someone editing their own state directory.
//!
//! ONLY A CLEAN TREE GETS ONE. A run over HEAD plus uncommitted work verified
//! bytes no commit holds, so it records nothing: its receipt could only ever
//! say "not gated", and keyed by HEAD it would stand where HEAD's own receipt
//! belongs.
//!
//! ONE RECEIPT STORE PER REPOSITORY: receipts live under the git COMMON dir
//! ([`dir`]), which every worktree of a repository shares, so a PASS written
//! from the worktree an agent gated in reaches every other checkout — the
//! release cutter's cut tree among them.
//!
//! FILED UNDER THE COMMIT AND UNDER ITS TREE (2026-09-26). A run's receipt is
//! written twice: as `<commit>`, and as [`tree_key`]`(<commit>^{tree})`. What
//! a run verifies is bytes, and the bytes are the tree: a reworded commit
//! message (`git commit --amend`, a `Co-Authored-By` line added after the
//! run), a rebase or a squash that lands on the same tree, or a cherry-pick
//! that reproduces it is a NEW commit over bytes a run already judged, and
//! keyed by commit alone its receipt vouched for nothing. A reader looks up
//! the commit first and then its tree (the cutter's `gates::receipt_report`
//! does, and says when a tree answered). The receipt still records the commit
//! the run was on (`head`), because not every gate is a pure function of the
//! tree — the citation gate reads history — so a tree receipt from another
//! commit is a statement about the bytes, and the reader names that commit.
//!
//! WHAT RAN IT (2026-09-26). The receipt records the compiler
//! (`toolchain <stage2 bin dir> trustc <commit-hash>`) and the spec checkers
//! the tests ran (`checkers ty = <path> (<tier>, <--version>); …`,
//! [`crate::checkers`]), so a verdict names the programs that reached it.
//! Both are additive keys of format 2: an older reader ignores them, and a
//! receipt an older gate wrote parses with them empty. So is the build
//! environment it took from its caller (`build-env <VAR="value"> …` or
//! `build-env none`, 2026-09-27, third review): `RUSTFLAGS`, a wrapper, a
//! profile override or a target runner make the same compiler build or run
//! other code, and a base must have been made under the same
//! ([`crate::differential::tools_differ`]). Every `RUST*` and `CARGO*`
//! variable but a few inert ones is recorded, and a build script's native
//! toolchain's ([`crate::differential::build_env_records`], fourth review).
//! And so are the cargo config FILES its builds read (`build-config
//! <role>=<git blob id> …` or `build-config none`, 2026-09-28,
//! [`crate::build_config`]): the repository's own `.cargo/config.toml`, every
//! ancestor directory's, `$CARGO_HOME`'s and what they include — a
//! `rustflags` or a profile in any of them builds other code exactly as the
//! variable would. Roles, not paths, so two checkouts with the same files
//! agree; a receipt without the line (an older gate's, or a run whose config
//! could not be read) serves no run as a base, and the release cutter's
//! MEASURE check requires it to be the cut tree's own config and nothing else.
//!
//! WHAT FAILED, ITEMIZED (2026-09-26). A receipt lists every failure the run
//! found — `failures <n>`, then one `fail <msg-hash> <since-commit>
//! <since-epoch> <id>` line per failed test or failing row — so a later run
//! can be judged against it ([`crate::differential`]): a red a branch's base
//! already had, with the same failure, is INHERITED and named, not blocking.
//! `<since-…>` is when main first went red on it, carried from receipt to
//! receipt, which is what the differential's age cap reads. The fixed fields
//! come first and the id last, because an id is free text (a doctest's name
//! has spaces). A run judged against a base names it (`base <commit>`) and
//! each failure it judged inherited (`inherited <id>`); a `--baseline` run on
//! main says so (`baseline yes`); a run judged against a NEAREST base (the
//! opt-in `--nearest-base`, [`crate::nearest`]) says so too (`base-mode
//! nearest <commit>`), and a run by the exact rule writes no such line. All
//! additive: an older reader ignores them,
//! and a receipt with no `failures` line predates the list, so it lists
//! nothing a run could be judged against — never "main was green".
//!
//! WHAT A RUN COULD NOT SEE (2026-09-27, second review). A run that could not
//! itemize everything — a stage skipped or unable to run, a test log that
//! could not account for its failures, any finding that cannot be read —
//! also lists the reds its since chain held that it did not itemize, with
//! when main first went red on each: `hidden <n>`, then one `hidden-fail
//! <msg-hash> <since-commit> <since-epoch> <id>` line per red. Never a red a
//! branch is judged against (this run did not see it); only where the next
//! run carries a since from, so one baseline that could not see a red does
//! not restart its clock under the age cap
//! ([`crate::differential::hidden_failures`]). Additive, and whole or nothing
//! like the failure list.
//!
//! THE MACHINE'S LOAD (2026-09-26). One `load <start> <end> <cores> <title>`
//! line per stage — the one-minute load average when it started and ended —
//! so a red read after the fact says whether the machine that found it was
//! busy ([`crate::ladder::StageLoad`]). Additive, and a record only: no
//! reader decides anything from it.
//!
//! A MEASURE RECEIPT IS FILED APART (2026-09-26). A run that ran the MEASURE
//! tier ([`crate::plan::MEASURE_TIER`]: `--measure`, `--full`) records whether
//! it MEASURED the tree — `measured yes` only when every MEASURE stage ran over
//! the whole tree and was green with nothing skipped — and files its receipt
//! under [`measure_key`]s too: `measure-<commit>` and `measure-tree-<tree>`. A
//! `--measure` run files there ONLY, since it ran no stage of the merge
//! contract and must never stand where a merge receipt (or main's list of
//! reds) belongs; a `--full` run files under both. The release cutter
//! (`aterm-release` `gates::measure_report`) requires `measured yes` under the
//! cut tree's measure key, from a run whose `toolchain` line is the compiler
//! the cut builds with (2026-09-27), before it claims a build number. Under
//! the measure keys a receipt that measured is never replaced by one that did
//! not ([`measure_strength`]), so a later red take cannot unmake a green one
//! and a merge-contract run never touches either.
//!
//! A WEAKER RECEIPT NEVER REPLACES A STRONGER ONE ([`write`], [`strength`]),
//! under either key. The store is shared, so without this a `--changed` or
//! `--scope` run, or a flaky FAIL, on a commit (or a tree) that already
//! carries a merge-contract receipt — from any worktree sitting at it —
//! destroyed the one receipt that vouches for it. A whole-tree PASS on these
//! exact bytes is a fact no later run unmakes. Since 2026-09-26 a NARROWED
//! receipt does not replace a whole-tree one that failed either: that receipt
//! is main's list of reds when the commit is main's, the base a branch is
//! judged against, and a narrowed run looked at too little to replace it.
//! Among receipts of one strength, the newest wins.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Where receipts live: `<git common dir>/`[`RECEIPT_DIR`].
pub const RECEIPT_DIR: &str = "aterm-verify/receipts";

/// The file-name prefix of a receipt filed under its TREE ([`tree_key`]). A
/// commit id never starts with it, so the two keys share one directory. The
/// release cutter's reader (`aterm-release` `gates::RECEIPT_TREE_PREFIX`)
/// mirrors it.
pub const TREE_KEY_PREFIX: &str = "tree-";

/// The name a receipt is filed under for the tree `tree`.
#[must_use]
pub fn tree_key(tree: &str) -> String {
    format!("{TREE_KEY_PREFIX}{tree}")
}

/// The file-name prefix of a MEASURE receipt ([`measure_key`]). A commit id
/// never starts with it, and neither does [`TREE_KEY_PREFIX`], so the three
/// families share one directory. The release cutter's reader (`aterm-release`
/// `gates::RECEIPT_MEASURE_PREFIX`) mirrors it.
pub const MEASURE_KEY_PREFIX: &str = "measure-";

/// The name a MEASURE receipt is filed under for `key` — a commit, or a
/// [`tree_key`]: `measure-<commit>`, `measure-tree-<tree>`.
#[must_use]
pub fn measure_key(key: &str) -> String {
    format!("{MEASURE_KEY_PREFIX}{key}")
}

/// The first line of every receipt. A reader that does not recognise it must
/// treat the file as no receipt at all, never as a permissive one.
///
/// Format 2 (2026-09-23) dropped format 1's `tree` state — only a clean tree
/// gets a receipt — and its `base`. The `tree` and `base` lines format 2 has
/// carried since 2026-09-26 are other, additive keys: the commit's tree, and
/// the commit a run was judged against. A format-1 file is not a receipt: it
/// vouches for nothing, and the cutter's reader (`aterm-release`
/// `gates::receipt_field`) checks this line before any other.
pub const MAGIC: &str = "aterm-verify receipt 2";

/// How many receipt files are kept — two per run, one under the commit and
/// one under its tree (four for a `--full` run, which is also a MEASURE
/// receipt), so about a thousand runs. One store serves every worktree of
/// the repository, so this covers days of gating; a receipt is a few hundred
/// bytes.
pub const KEPT: usize = 2000;

/// One run's verdict about one commit and its tree.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Receipt {
    /// The commit the run verified, on a clean tree.
    pub head: String,
    /// `<head>^{tree}`, the second key the receipt is filed under
    /// ([`tree_key`]). `None` when git could not name it — the receipt then
    /// stands under the commit alone — and in a receipt an older gate wrote.
    pub tree: Option<String>,
    /// `fast`, `measure` or `full` — the mode the run was ([`crate::Mode`]).
    pub mode: String,
    /// `workspace`, `crate:<name>`, or `changed:<base>`
    /// ([`crate::scope::Scope::receipt_word`]; a receipt written before
    /// 2026-09-26 says a bare `changed`).
    pub scope: String,
    /// `PASS`, `FAIL` or `COULD-NOT-RUN`.
    pub verdict: String,
    /// Did this run discharge the WHOLE merge contract? The one predicate a
    /// commit is counted as gated on, and it is [`crate::verdict::claims`]'s,
    /// not a second opinion: whole tree, nothing skipped, nothing that could
    /// not run, nothing NEW failed (every red INHERITED from the base's
    /// receipt, named in `inherited`).
    pub merge_contract: bool,
    /// Did this run MEASURE the tree? `Some(true)` — `measured yes` — only
    /// when it ran every stage of the MEASURE tier over the whole tree and each
    /// was green with nothing skipped, and nothing moved under the run;
    /// `Some(false)` for a run of the tier that did not; `None` — no line — for
    /// a run that did not run the tier, and in a receipt an older gate wrote.
    /// What the release cutter requires, under [`measure_key`].
    pub measured: Option<bool>,
    /// What this run SKIPPED, in the verdict's own words (at most a handful,
    /// then a count) — never part of the decision, which `merge_contract`
    /// already carries, but the difference between "the gate refused you" and
    /// "the gate could not present on this machine", which is what an operator
    /// on a headless box needs to read.
    pub skipped: String,
    /// The compiler the run used: `<stage2 bin dir> trustc <commit-hash>`.
    /// Empty in a receipt an older gate wrote.
    pub toolchain: String,
    /// The spec checkers the tests ran, as the ladder's `verify: checkers`
    /// line named them ([`crate::checkers::Checkers::summary`]). Empty in a
    /// receipt an older gate wrote.
    pub checkers: String,
    /// The compile and test-run configuration the run took from its
    /// environment (`RUSTFLAGS`, `RUSTC_WRAPPER`, `CARGO_PROFILE_*`, a target
    /// runner, …: [`crate::differential::build_env`]), `none` when it took
    /// none — 2026-09-27, third review: the same compiler under other flags
    /// builds other code. `None` in a receipt an older gate wrote.
    pub build_env: Option<String>,
    /// The cargo config files the run's builds read — the repository's own,
    /// every ancestor directory's, `$CARGO_HOME`'s and what they include — as
    /// `<role>=<git blob id>` entries, or `none`
    /// ([`crate::build_config::record`], 2026-09-28: a `rustflags` in
    /// `~/.cargo/config.toml` builds other code exactly as `RUSTFLAGS` does).
    /// `None` in a receipt an older gate wrote, and for a run whose config
    /// files could not be read: such a receipt serves no run as a base.
    pub build_config: Option<String>,
    /// Every failure the run found, itemized ([`crate::ladder::Finding`]):
    /// `None` in a receipt written before the list existed (no `failures`
    /// line), which therefore can never serve as a base.
    pub failures: Option<Vec<Failure>>,
    /// The commit whose receipt the run was judged against, when it was
    /// ([`crate::differential`]).
    pub base: Option<String>,
    /// `Some(base)` when that base was a NEAREST one — an ancestor of the
    /// merge-base, not the merge-base itself — judged by the opt-in
    /// `--nearest-base` rule ([`crate::nearest`], 2026-09-28): `base-mode
    /// nearest <commit>`. No line (`None`) under the exact rule, so a
    /// default run's receipt is what it always was. A record for the reader;
    /// the release cutter reads such a receipt exactly as any other.
    pub base_nearest: Option<String>,
    /// The ids of the failures judged INHERITED from that base — red there
    /// with the same failure, inside the age cap. What `merge_contract`
    /// excused, named.
    pub inherited: Vec<String>,
    /// A `--baseline` run: main's own reds, published for branches to be
    /// judged against.
    pub baseline: bool,
    /// The reds this run's since chain held that it could not itemize, with
    /// when main first went red on each
    /// ([`crate::differential::hidden_failures`]): carried to the next run's
    /// since chain, never judged against. Empty when the run saw everything,
    /// and in a receipt an older gate wrote.
    pub hidden: Vec<Failure>,
    /// The machine's one-minute load at each stage's start and end, with the
    /// stage's title (`load <start> <end> <cores> <title>`, 2026-09-26) — so a
    /// red read after the fact says whether its machine was busy. A record,
    /// never part of any decision; empty in a receipt an older gate wrote.
    pub loads: Vec<(crate::ladder::StageLoad, String)>,
    /// Seconds since the epoch, for the operator — and, through `failures`'
    /// since-epochs, for the differential's age cap; never for anything else.
    pub when: u64,
}

/// `load <start> <end> <cores> <title>`, read back: the loads as the
/// two-decimal text [`crate::ladder::StageLoad::text`] writes.
fn parse_load(rest: &str) -> Option<(crate::ladder::StageLoad, String)> {
    let mut f = rest.splitn(4, ' ');
    let (start, end, cores, title) = (f.next()?, f.next()?, f.next()?, f.next()?);
    let centi = |t: &str| -> Option<u32> {
        let (whole, frac) = t.split_once('.')?;
        (frac.len() == 2 && frac.bytes().all(|b| b.is_ascii_digit())).then_some(())?;
        whole
            .parse::<u32>()
            .ok()?
            .checked_mul(100)?
            .checked_add(frac.parse().ok()?)
    };
    Some((
        crate::ladder::StageLoad {
            start: centi(start)?,
            end: centi(end)?,
            cores: cores.parse().ok()?,
        },
        title.to_string(),
    ))
}

/// One failure a receipt lists: `fail <hash> <since> <since_when> <id>`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Failure {
    /// [`crate::ladder::Finding::id`].
    pub id: String,
    /// [`crate::ladder::Finding::hash`]: 16 hex digits.
    pub hash: String,
    /// The commit whose run first recorded this failure, carried forward from
    /// the base receipt while it stays the same failure — "red on main since".
    pub since: String,
    /// When that run recorded it, in seconds since the epoch.
    pub since_when: u64,
}

/// A value on one line: a newline would forge a key.
fn one_line(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

/// `fail <hash> <since> <since_when> <id>`, read back; `None` for anything
/// malformed, which makes the whole file no receipt.
fn parse_failure(rest: &str) -> Option<Failure> {
    let mut f = rest.splitn(4, ' ');
    let (hash, since, since_when, id) = (f.next()?, f.next()?, f.next()?, f.next()?);
    let well_formed = hash.len() == 16
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && !since.is_empty()
        && !id.is_empty();
    if !well_formed {
        return None;
    }
    Some(Failure {
        id: id.to_string(),
        hash: hash.to_string(),
        since: since.to_string(),
        since_when: since_when.parse().ok()?,
    })
}

impl Receipt {
    /// The file a reader reads. One `key value` per line, so `grep` is enough and
    /// no reader needs a parser.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::from(MAGIC);
        s.push('\n');
        // EVERY value on one line (2026-09-27, third review): the scope carries
        // the `--base` a `--changed` run was given, verbatim, and a newline in
        // it wrote `merge-contract yes` — or `scope workspace` — as a line of
        // its own, which the release cutter's reader (first line wins) and this
        // one (last line wins) then read two different ways.
        let _ = writeln!(s, "head {}", one_line(&self.head));
        if let Some(tree) = &self.tree {
            let _ = writeln!(s, "tree {}", one_line(tree));
        }
        let _ = writeln!(s, "mode {}", one_line(&self.mode));
        let _ = writeln!(s, "scope {}", one_line(&self.scope));
        let _ = writeln!(s, "verdict {}", one_line(&self.verdict));
        let _ = writeln!(
            s,
            "merge-contract {}",
            if self.merge_contract { "yes" } else { "no" }
        );
        if let Some(measured) = self.measured {
            let _ = writeln!(s, "measured {}", if measured { "yes" } else { "no" });
        }
        let _ = writeln!(s, "skipped {}", one_line(&self.skipped));
        // One line each, whatever they hold: a newline would forge a key.
        for (key, value) in [("toolchain", &self.toolchain), ("checkers", &self.checkers)] {
            if !value.is_empty() {
                let _ = writeln!(s, "{key} {}", one_line(value));
            }
        }
        if let Some(env) = &self.build_env {
            let _ = writeln!(s, "build-env {}", one_line(env));
        }
        if let Some(config) = &self.build_config {
            let _ = writeln!(s, "build-config {}", one_line(config));
        }
        if let Some(failures) = &self.failures {
            let _ = writeln!(s, "failures {}", failures.len());
            for f in failures {
                let _ = writeln!(
                    s,
                    "fail {} {} {} {}",
                    f.hash,
                    f.since,
                    f.since_when,
                    one_line(&f.id)
                );
            }
        }
        if !self.hidden.is_empty() {
            let _ = writeln!(s, "hidden {}", self.hidden.len());
            for f in &self.hidden {
                let _ = writeln!(
                    s,
                    "hidden-fail {} {} {} {}",
                    f.hash,
                    f.since,
                    f.since_when,
                    one_line(&f.id)
                );
            }
        }
        if let Some(base) = &self.base {
            let _ = writeln!(s, "base {}", one_line(base));
        }
        if let Some(nearest) = &self.base_nearest {
            let _ = writeln!(s, "base-mode nearest {}", one_line(nearest));
        }
        for id in &self.inherited {
            let _ = writeln!(s, "inherited {}", one_line(id));
        }
        if self.baseline {
            s.push_str("baseline yes\n");
        }
        for (load, title) in &self.loads {
            let _ = writeln!(
                s,
                "load {} {} {} {}",
                crate::ladder::StageLoad::text(load.start),
                crate::ladder::StageLoad::text(load.end),
                load.cores,
                one_line(title)
            );
        }
        let _ = writeln!(s, "when {}", self.when);
        s
    }

    /// Read one back. `None` for anything that is not a receipt this version
    /// wrote — an unknown format is NOT a receipt, so it can never vouch for a
    /// commit.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut get = std::collections::BTreeMap::new();
        let (mut fails, mut inherited, mut loads) = (Vec::new(), Vec::new(), Vec::new());
        let mut hidden = Vec::new();
        for l in lines {
            if let Some((k, v)) = l.split_once(' ') {
                match k {
                    "fail" => fails.push(parse_failure(v)?),
                    "hidden-fail" => hidden.push(parse_failure(v)?),
                    "inherited" => inherited.push(v.to_string()),
                    "load" => loads.push(parse_load(v)?),
                    _ => {
                        get.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }
        // The count is what makes the list whole: a file cut short, or with a
        // line missing, is no receipt — a list shorter than the run's would
        // turn one of its reds into "not red on main".
        let failures = match get.get("failures") {
            None if fails.is_empty() => None,
            None => return None,
            Some(n) if n.parse::<usize>().ok()? == fails.len() => Some(fails),
            Some(_) => return None,
        };
        // Whole or nothing, like the failure list: a hidden red dropped would
        // restart its clock.
        match get.get("hidden") {
            None if hidden.is_empty() => {}
            Some(n) if n.parse::<usize>().ok()? == hidden.len() => {}
            _ => return None,
        }
        Some(Self {
            head: get.get("head")?.clone(),
            tree: get.get("tree").cloned(),
            mode: get.get("mode")?.clone(),
            scope: get.get("scope")?.clone(),
            verdict: get.get("verdict")?.clone(),
            merge_contract: get.get("merge-contract").is_some_and(|v| v == "yes"),
            measured: get.get("measured").map(|v| v == "yes"),
            skipped: get.get("skipped").cloned().unwrap_or_default(),
            toolchain: get.get("toolchain").cloned().unwrap_or_default(),
            checkers: get.get("checkers").cloned().unwrap_or_default(),
            build_env: get.get("build-env").cloned(),
            build_config: get.get("build-config").cloned(),
            failures,
            base: get.get("base").cloned(),
            base_nearest: get
                .get("base-mode")
                .and_then(|v| v.strip_prefix("nearest "))
                .map(str::to_string),
            inherited,
            baseline: get.get("baseline").is_some_and(|v| v == "yes"),
            hidden,
            loads,
            when: get.get("when").and_then(|w| w.parse().ok()).unwrap_or(0),
        })
    }

    /// Did this run run the merge contract's LAND tier? Every mode's but
    /// `--measure`'s. Only such a receipt is filed under the commit and tree
    /// keys a merge-contract reader looks up.
    #[must_use]
    pub fn lands(&self) -> bool {
        self.mode != "measure"
    }
}

/// `<git common dir of root>/aterm-verify/receipts` — the one receipt store
/// every worktree of the repository shares, and the one the release cutter's
/// receipt report reads (it asks git the same question).
///
/// # Errors
/// When git cannot say where `root`'s common dir is: not a repository, or git
/// failing. No receipt is written then, which leaves the commit counted as
/// ungated.
pub fn dir(root: &Path) -> std::io::Result<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()?;
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || common.is_empty() {
        return Err(std::io::Error::other(format!(
            "git cannot name the common git dir of {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(PathBuf::from(common).join(RECEIPT_DIR))
}

/// `<commit>^{tree}` as git names it in `root`'s repository, or `None` when
/// git cannot — the receipt is then filed under the commit alone.
#[must_use]
pub fn tree_of(root: &Path, commit: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{commit}^{{tree}}"))
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !tree.is_empty() && tree.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(tree)
}

/// What [`write`] did under the COMMIT key (the tree key follows the same
/// rule, silently: the ladder names the commit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    /// This receipt now stands at the path.
    Stored(PathBuf),
    /// The commit already carries a stronger receipt at the path — one that
    /// discharged the contract where this did not, or a whole-tree one where
    /// this run was narrowed ([`strength`]) — so it stayed.
    KeptWholeTree(PathBuf),
}

/// How much a receipt says about its commit, for [`write`]'s rule: `2` it
/// discharged the merge contract, `1` a whole-tree run that did not (its
/// failure list is what a later run is judged against), `0` a narrowed run.
#[must_use]
pub fn strength(r: &Receipt) -> u8 {
    if r.merge_contract {
        2
    } else if r.scope == "workspace" {
        1
    } else {
        0
    }
}

/// The same rule under the MEASURE keys ([`measure_key`]): `2` it measured
/// the tree (`measured yes`), `1` a whole-tree run of the tier that did not,
/// `0` a narrowed one.
#[must_use]
pub fn measure_strength(r: &Receipt) -> u8 {
    if r.measured == Some(true) {
        2
    } else if r.scope == "workspace" {
        1
    } else {
        0
    }
}

/// Write `r` under `root`: a run of the LAND tier ([`Receipt::lands`]) under
/// the commit it is about and, when `r` names one, under its tree
/// ([`tree_key`]); a run of the MEASURE tier (`measured` set) under the
/// [`measure_key`] of each — every key unless it already holds a stronger
/// receipt ([`strength`], [`measure_strength`]). The answer is about the
/// commit key of the first family written: the merge-contract one when the run
/// landed, else the MEASURE one.
///
/// The checks and the replaces happen under an exclusive lock on
/// `<store>.lock`, so two runs finishing together cannot interleave a weaker
/// write past a check.
///
/// Best effort in one direction only: a receipt that cannot be written costs
/// the commit its record — the cutter then counts it as ungated — never this
/// run its verdict. The gate says so on
/// stderr rather than failing, because a read-only state directory is an
/// operator's problem with their disk and not a finding about their change.
pub fn write(root: &Path, r: &Receipt) -> std::io::Result<Written> {
    let dir = dir(root)?;
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.with_extension("lock"))?;
    lock.lock()?;
    prune(&dir);
    let keys = |family: fn(&str) -> String| {
        std::iter::once(family(&r.head)).chain(r.tree.as_deref().map(|t| family(&tree_key(t))))
    };
    let mut first = None;
    if r.lands() {
        for key in keys(str::to_string) {
            let w = file_under(&dir, &key, r, strength)?;
            first.get_or_insert(w);
        }
    }
    if r.measured.is_some() {
        for key in keys(measure_key) {
            let w = file_under(&dir, &key, r, measure_strength)?;
            first.get_or_insert(w);
        }
    }
    first.ok_or_else(|| {
        std::io::Error::other(format!(
            "a `{}` receipt that ran neither tier is filed nowhere",
            r.mode
        ))
    })
}

/// Write `r` as `dir/name` unless a stronger receipt (by `rank`) stands
/// there. The caller holds the store's lock.
fn file_under(
    dir: &Path,
    name: &str,
    r: &Receipt,
    rank: fn(&Receipt) -> u8,
) -> std::io::Result<Written> {
    let path = dir.join(name);
    if std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| Receipt::parse(&text))
        .is_some_and(|standing| rank(&standing) > rank(r))
    {
        return Ok(Written::KeptWholeTree(path));
    }
    // Whole-file replace: a half-written receipt that still parsed would be the
    // one failure mode that matters here.
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, r.render())?;
    std::fs::rename(&tmp, &path)?;
    Ok(Written::Stored(path))
}

/// Keep the newest [`KEPT`] receipts. Housekeeping only: every failure here is
/// ignored, because a full directory must never cost a run its verdict.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if kept.len() < KEPT {
        return;
    }
    kept.sort_unstable();
    let drop_n = kept.len() + 1 - KEPT;
    for (_, p) in kept.into_iter().take(drop_n) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn green() -> Receipt {
        Receipt {
            head: "a".repeat(40),
            mode: "fast".into(),
            scope: "workspace".into(),
            verdict: "PASS".into(),
            merge_contract: true,
            skipped: "none".into(),
            when: 1_758_000_000,
            ..Receipt::default()
        }
    }

    /// A whole-tree receipt and a narrowed one both round-trip, with and
    /// without the tree, toolchain and checker lines; a newline in a recorded
    /// value cannot forge a key.
    #[test]
    fn a_receipt_round_trips() {
        let r = green();
        assert_eq!(Receipt::parse(&r.render()), Some(r.clone()));
        let changed = Receipt {
            scope: "changed:main".into(),
            merge_contract: false,
            ..green()
        };
        assert_eq!(Receipt::parse(&changed.render()), Some(changed));
        let full = Receipt {
            tree: Some("b".repeat(40)),
            toolchain: "/store/trust/9192/bin trustc 43f8b339f".into(),
            checkers: "ty = /store/ty/3007/bin/ty (atpkg store, ty 0.15.0); trust-ir = absent; \
                       ay = absent"
                .into(),
            ..green()
        };
        assert_eq!(Receipt::parse(&full.render()), Some(full.clone()));
        for measured in [Some(true), Some(false)] {
            let r = Receipt {
                mode: "measure".into(),
                merge_contract: false,
                measured,
                ..green()
            };
            let text = r.render();
            assert!(
                text.contains(if measured == Some(true) {
                    "\nmeasured yes\n"
                } else {
                    "\nmeasured no\n"
                }),
                "{text}"
            );
            assert_eq!(Receipt::parse(&text), Some(r));
        }
        assert!(
            !green().render().contains("measured"),
            "a run that did not run the tier says nothing about it"
        );
        let forged = Receipt {
            merge_contract: false,
            checkers: "ty = x\nmerge-contract yes".into(),
            ..green()
        };
        let text = forged.render();
        assert!(!text.contains("\nmerge-contract yes\n"), "{text}");
        assert_eq!(
            Receipt::parse(&text).map(|r| r.merge_contract),
            Some(false),
            "{text}"
        );
    }

    /// THE FAILURE LIST ROUND-TRIPS, and it is whole or it is nothing. A
    /// receipt lists each failure with its hash and since, the base it was
    /// judged against, what it inherited and whether it is a baseline; an id
    /// with spaces (a doctest's) and one with a newline in it survive as one
    /// line. A list shorter or longer than its count, or a malformed `fail`
    /// line, makes the file no receipt — a shortened list would turn a red
    /// into "not red on main". The controls: a green run lists `failures 0`,
    /// and a receipt from before the list has `failures: None`, never an
    /// empty list.
    #[test]
    fn the_failure_list_round_trips_and_is_whole_or_nothing() {
        let f = |id: &str, hash: &str, when| Failure {
            id: id.into(),
            hash: hash.into(),
            since: "c".repeat(40),
            since_when: when,
        };
        let judged = Receipt {
            failures: Some(vec![
                f(
                    "-p x --test probe -- index_probe_two",
                    "0123456789abcdef",
                    5,
                ),
                f(
                    "-p x --doc -- src/lib.rs - foo (line 12)",
                    "fedcba9876543210",
                    6,
                ),
                f("tippy\nmerge-contract yes", "00000000000000ff", 7),
            ]),
            base: Some("d".repeat(40)),
            inherited: vec!["-p x --test probe -- index_probe_two".into()],
            baseline: false,
            ..green()
        };
        let text = judged.render();
        assert!(text.contains("\nfailures 3\n"), "{text}");
        assert!(
            text.contains(&format!(
                "\nfail 0123456789abcdef {} 5 -p x --test probe -- index_probe_two\n",
                "c".repeat(40)
            )),
            "{text}"
        );
        assert!(!text.contains("\nmerge-contract yes\nfail"), "{text}");
        let back = Receipt::parse(&text).expect("it parses");
        assert_eq!(back.failures.as_ref().map(Vec::len), Some(3));
        assert_eq!(
            back.failures.as_ref().expect("listed")[1].id,
            "-p x --doc -- src/lib.rs - foo (line 12)"
        );
        assert_eq!(
            back.failures.as_ref().expect("listed")[2].id,
            "tippy merge-contract yes"
        );
        assert_eq!(
            (back.base, back.inherited, back.baseline),
            (judged.base.clone(), judged.inherited.clone(), false)
        );
        let baseline = Receipt {
            baseline: true,
            failures: Some(Vec::new()),
            ..green()
        };
        let text = baseline.render();
        assert!(text.contains("\nfailures 0\nbaseline yes\n"), "{text}");
        assert_eq!(Receipt::parse(&text), Some(baseline));
        assert_eq!(
            Receipt::parse(&green().render()).map(|r| r.failures),
            Some(None)
        );

        let good = judged.render();
        for broken in [
            good.replace("failures 3", "failures 2"),
            good.replace("failures 3", "failures 4"),
            good.replace("failures 3\n", ""),
            good.replace("fail 0123456789abcdef", "fail 0123"),
            good.replace(" 5 -p x", " five -p x"),
        ] {
            assert_eq!(Receipt::parse(&broken), None, "{broken}");
        }
    }

    /// THE LOAD LINES ROUND-TRIP (2026-09-26): one per stage, titles with
    /// spaces intact and never a second key, the loads as the two-decimal
    /// text they were read as — and a load line that is not one makes the
    /// file no receipt, like any other line this version writes.
    #[test]
    fn the_stage_loads_round_trip_and_a_torn_one_is_no_receipt() {
        use crate::ladder::StageLoad;
        let r = Receipt {
            loads: vec![
                (
                    StageLoad {
                        start: 7482,
                        end: 8010,
                        cores: 14,
                    },
                    "test (--workspace)".into(),
                ),
                (
                    StageLoad {
                        start: 5,
                        end: 100,
                        cores: 14,
                    },
                    "grep guards\nmerge-contract yes".into(),
                ),
            ],
            ..green()
        };
        let text = r.render();
        assert!(
            text.contains("\nload 74.82 80.10 14 test (--workspace)\n"),
            "{text}"
        );
        assert!(
            text.contains("\nload 0.05 1.00 14 grep guards merge-contract yes\n"),
            "{text}"
        );
        let back = Receipt::parse(&text).expect("parses");
        assert_eq!(back.loads[0], r.loads[0]);
        assert_eq!(back.loads[1].1, "grep guards merge-contract yes");
        assert_eq!(
            text.matches("\nmerge-contract ").count(),
            1,
            "a title never forges a key: {text}"
        );
        assert_eq!(back.merge_contract, r.merge_contract);
        for broken in [
            text.replace("load 74.82 80.10 14", "load 74.8 80.10 14"),
            text.replace("load 74.82 80.10 14", "load 74.82 80.10 many"),
            text.replace("load 74.82 80.10 14 test (--workspace)", "load 74.82 80.10"),
        ] {
            assert_eq!(Receipt::parse(&broken), None, "{broken}");
        }
        assert!(
            Receipt::parse(&green().render())
                .expect("parses")
                .loads
                .is_empty()
        );
    }

    /// A RECEIPT AN OLDER GATE WROTE STILL READS: format 2 before the tree,
    /// toolchain and checker lines existed parses, with them absent.
    #[test]
    fn a_format_2_receipt_without_the_newer_lines_still_parses() {
        let old = format!(
            "{MAGIC}\nhead x\nmode fast\nscope changed\nverdict PASS\nmerge-contract no\n\
             skipped none\nwhen 7\n"
        );
        let r = Receipt::parse(&old).expect("a format-2 receipt");
        assert_eq!(
            (r.tree, r.toolchain, r.checkers, r.scope, r.when),
            (None, String::new(), String::new(), "changed".to_string(), 7)
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=gate",
                "-c",
                "user.email=gate@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs")
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A scratch repository with one commit: `(scratch dir, checkout)`.
    fn repo(tag: &str) -> (PathBuf, PathBuf) {
        let tmp = crate::mktemp_dir(tag).expect("mktemp");
        let main = tmp.join("main");
        std::fs::create_dir_all(&main).expect("mkdir");
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "one"]);
        (tmp, main)
    }

    fn rev(dir: &Path, what: &str) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", what])
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git rev-parse {what}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn stored(w: Written) -> PathBuf {
        match w {
            Written::Stored(path) => path,
            Written::KeptWholeTree(path) => panic!("kept instead of stored: {}", path.display()),
        }
    }

    /// ONE STORE PER REPOSITORY: a receipt written from a linked worktree lands
    /// where the main checkout reads, because both resolve the same git common
    /// dir. The negative control is the per-checkout path this replaced, which
    /// two worktrees never shared.
    #[test]
    fn every_worktree_of_a_repository_shares_one_receipt_store() {
        let (tmp, main) = repo("atv-receipt-common");
        let linked = tmp.join("linked");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().expect("utf-8"),
                "-b",
                "side",
            ],
        );
        let from_linked = dir(&linked).expect("the linked worktree's store");
        assert_eq!(from_linked, dir(&main).expect("the main checkout's store"));
        let written = stored(write(&linked, &green()).expect("written from the linked worktree"));
        assert!(
            written.starts_with(dir(&main).expect("store")),
            "{}",
            written.display()
        );
        assert!(
            !written.starts_with(&linked),
            "the negative control: a per-checkout store would sit inside the worktree"
        );
        assert!(
            dir(&tmp.join("nowhere")).is_err(),
            "no repository, no store"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// AN UNREADABLE RECEIPT IS NO RECEIPT. The one direction this must never
    /// fail in: a file a reader cannot understand vouches for nothing, never
    /// for a pass.
    #[test]
    fn anything_that_is_not_this_format_parses_as_nothing() {
        let body = "head x\nmode fast\nscope workspace\nverdict PASS\nmerge-contract yes\n\
                    skipped none\nwhen 1\n";
        for text in [
            String::new(),
            // Format 1, whole and passing: an older gate's, so no receipt.
            format!("aterm-verify receipt 1\nhead x\ntree clean\n{}", &body[7..]),
            format!("aterm-verify receipt 3\n{body}"),
            body.to_string(),
            format!("{MAGIC}\nhead x\n"),
        ] {
            assert_eq!(Receipt::parse(&text), None, "{text:?}");
        }
        assert!(
            Receipt::parse(&format!("{MAGIC}\n{body}")).is_some(),
            "the control: the same body under this format's magic is a receipt"
        );
    }

    /// A WEAKER RECEIPT NEVER REPLACES A WHOLE-TREE ONE. The store is shared
    /// and keyed by commit, so a `--changed` run, a `--scope` run or a flaky
    /// FAIL at a fully receipted commit — from any worktree at that commit —
    /// would otherwise destroy the receipt that admits it. The negative
    /// controls are the replacements that must still happen: last writer wins
    /// among receipts that admit nothing, a whole-tree receipt replaces a
    /// narrowed one, and a whole-tree receipt refreshes itself.
    #[test]
    fn a_weaker_receipt_never_replaces_a_whole_tree_one() {
        let (tmp, main) = repo("atv-receipt-keep");
        let read = || {
            let path = dir(&main).expect("store").join("a".repeat(40));
            Receipt::parse(&std::fs::read_to_string(path).expect("a receipt stands"))
                .expect("it parses")
        };
        let changed = Receipt {
            scope: "changed".into(),
            merge_contract: false,
            when: 2,
            ..green()
        };
        let scoped = Receipt {
            scope: "crate:aterm-grid".into(),
            merge_contract: false,
            when: 3,
            ..green()
        };
        let failed = Receipt {
            verdict: "FAIL".into(),
            merge_contract: false,
            when: 4,
            ..green()
        };

        // Nothing admits yet: the newest narrowed run wins.
        stored(write(&main, &changed).expect("write"));
        stored(write(&main, &scoped).expect("write"));
        assert_eq!(
            read(),
            scoped,
            "last writer wins among receipts that admit nothing"
        );

        // A whole-tree receipt replaces a narrowed one…
        stored(write(&main, &green()).expect("write"));
        assert_eq!(read(), green());
        // …and nothing weaker replaces it.
        for weaker in [&changed, &scoped, &failed] {
            assert!(
                matches!(
                    write(&main, weaker).expect("write"),
                    Written::KeptWholeTree(_)
                ),
                "{weaker:?}"
            );
            assert_eq!(
                read(),
                green(),
                "{weaker:?} replaced the whole-tree receipt"
            );
        }
        // A newer whole-tree receipt refreshes it.
        let refreshed = Receipt { when: 5, ..green() };
        stored(write(&main, &refreshed).expect("write"));
        assert_eq!(read(), refreshed);

        // A WHOLE-TREE FAIL IS MAIN'S LIST OF REDS (2026-09-26): once it
        // stands, a narrowed run does not replace it — a later run is judged
        // against that list — while a newer whole-tree run, failing or
        // passing, does.
        let (tmp2, other) = repo("atv-receipt-keep-fail");
        let read_other = || {
            let path = dir(&other).expect("store").join("a".repeat(40));
            Receipt::parse(&std::fs::read_to_string(path).expect("a receipt stands"))
                .expect("it parses")
        };
        stored(write(&other, &failed).expect("write"));
        for narrowed in [&changed, &scoped] {
            assert!(
                matches!(
                    write(&other, narrowed).expect("write"),
                    Written::KeptWholeTree(_)
                ),
                "{narrowed:?}"
            );
            assert_eq!(read_other(), failed, "{narrowed:?} replaced main's reds");
        }
        let failed_again = Receipt {
            when: 6,
            ..failed.clone()
        };
        stored(write(&other, &failed_again).expect("write"));
        assert_eq!(read_other(), failed_again);
        stored(write(&other, &green()).expect("write"));
        assert_eq!(read_other(), green());
        assert_eq!(
            [&changed, &failed, &green()].map(strength),
            [0, 1, 2],
            "narrowed < whole-tree < merge contract"
        );
        std::fs::remove_dir_all(&tmp2).ok();
        // The lock sits beside the store, never among the receipts.
        let store = dir(&main).expect("store");
        assert!(store.with_extension("lock").is_file());
        let names: Vec<String> = std::fs::read_dir(&store)
            .expect("store")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a".repeat(40)]);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A MEASURE RECEIPT IS FILED APART, AND NOTHING THAT DID NOT MEASURE
    /// REPLACES ONE (2026-09-26). A `--measure` run files under the measure
    /// keys only — never where the commit's merge receipt stands; a `--full`
    /// run files under both families; a merge-contract run touches neither
    /// measure key. Under them, `measured yes` is never replaced by a run that
    /// did not measure (a later red take), while a newer measurement refreshes
    /// it. The negative control: the same writes under the merge keys would
    /// have let the `--measure` run replace a whole-tree FAIL — main's list of
    /// reds — since the two are the same strength there.
    #[test]
    fn a_measure_receipt_is_filed_apart_and_only_a_measurement_replaces_one() {
        let (tmp, main) = repo("atv-receipt-measure");
        let (head, tree) = (rev(&main, "HEAD"), rev(&main, "HEAD^{tree}"));
        let store = dir(&main).expect("store");
        let read = |name: &str| {
            Receipt::parse(&std::fs::read_to_string(store.join(name)).expect("a receipt stands"))
                .expect("it parses")
        };
        let names = || {
            let mut n: Vec<String> = std::fs::read_dir(&store)
                .expect("store")
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            n.sort();
            n
        };
        let at = |r: Receipt| Receipt {
            head: head.clone(),
            tree: Some(tree.clone()),
            ..r
        };
        let red_main = at(Receipt {
            verdict: "FAIL".into(),
            merge_contract: false,
            failures: Some(Vec::new()),
            when: 1,
            ..green()
        });
        stored(write(&main, &red_main).expect("write"));
        assert_eq!(names(), [head.clone(), tree_key(&tree)]);

        let measured = at(Receipt {
            mode: "measure".into(),
            merge_contract: false,
            measured: Some(true),
            when: 2,
            ..green()
        });
        let path = stored(write(&main, &measured).expect("write"));
        assert_eq!(path, store.join(measure_key(&head)));
        let mut want = vec![
            head.clone(),
            measure_key(&head),
            measure_key(&tree_key(&tree)),
            tree_key(&tree),
        ];
        want.sort();
        assert_eq!(names(), want);
        assert_eq!(
            read(&head),
            red_main,
            "main's reds stand under the merge key"
        );
        assert_eq!(read(&measure_key(&tree_key(&tree))), measured);
        assert_eq!(
            [&red_main, &measured].map(strength),
            [1, 1],
            "the negative control: under the merge rule the --measure run would have \
             replaced main's reds"
        );

        // A --full run whose MEASURE tier went red: its merge receipt files,
        // and the measurement stands.
        let full_red = at(Receipt {
            mode: "full".into(),
            verdict: "FAIL".into(),
            merge_contract: false,
            measured: Some(false),
            failures: Some(Vec::new()),
            when: 3,
            ..green()
        });
        stored(write(&main, &full_red).expect("write"));
        assert_eq!(read(&head), full_red);
        assert_eq!(read(&measure_key(&head)), measured);
        assert_eq!(read(&measure_key(&tree_key(&tree))), measured);
        // A merge-contract run touches no measure key.
        let fast = at(Receipt { when: 4, ..green() });
        stored(write(&main, &fast).expect("write"));
        assert_eq!(read(&measure_key(&head)), measured);
        // A newer measurement — a green --full — refreshes both families.
        let full_green = at(Receipt {
            mode: "full".into(),
            measured: Some(true),
            when: 5,
            ..green()
        });
        stored(write(&main, &full_green).expect("write"));
        assert_eq!(read(&measure_key(&head)), full_green);
        assert_eq!(read(&head), full_green);
        assert_eq!(
            [&measured, &full_red, &fast].map(measure_strength),
            [2, 1, 1]
        );
        assert!(
            write(
                &main,
                &Receipt {
                    mode: "measure".into(),
                    measured: None,
                    ..green()
                }
            )
            .is_err(),
            "a receipt of neither tier is filed nowhere, and says so"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// FILED UNDER ITS TREE TOO. A receipt for HEAD is also filed under
    /// `HEAD^{tree}`, so the commits that carry the same bytes under another
    /// id — a reworded amend, a rebase or cherry-pick onto the same tree —
    /// find it by their tree, while their own commit key stays empty. The
    /// weaker-never-replaces rule holds per key: a FAIL on the reworded commit
    /// files its own commit key but leaves the tree's whole-tree PASS standing.
    /// The negative control is a commit whose tree differs: nothing is filed
    /// under its tree.
    #[test]
    fn a_receipt_is_filed_under_its_tree_so_a_reworded_commit_finds_it() {
        let (tmp, main) = repo("atv-receipt-tree");
        std::fs::write(main.join("f"), "bytes\n").expect("write");
        git(&main, &["add", "f"]);
        git(&main, &["commit", "-q", "-m", "two"]);
        let (head, tree) = (rev(&main, "HEAD"), rev(&main, "HEAD^{tree}"));
        assert_eq!(tree_of(&main, &head).as_deref(), Some(tree.as_str()));
        assert_eq!(tree_of(&main, &"f".repeat(40)), None, "no such commit");
        let pass = Receipt {
            head: head.clone(),
            tree: tree_of(&main, &head),
            ..green()
        };
        stored(write(&main, &pass).expect("write"));
        let store = dir(&main).expect("store");
        let by_tree = || {
            Receipt::parse(
                &std::fs::read_to_string(store.join(tree_key(&tree))).expect("a tree receipt"),
            )
            .expect("it parses")
        };
        assert_eq!(by_tree(), pass);

        git(&main, &["commit", "-q", "--amend", "-m", "two, reworded"]);
        let reworded = rev(&main, "HEAD");
        assert_ne!(reworded, head);
        assert_eq!(tree_of(&main, &reworded).as_deref(), Some(tree.as_str()));
        assert!(
            !store.join(&reworded).exists(),
            "no receipt under the new id"
        );
        assert_eq!(
            by_tree().head,
            head,
            "the tree's receipt names the run's commit"
        );

        let failed = Receipt {
            head: reworded.clone(),
            tree: tree_of(&main, &reworded),
            verdict: "FAIL".into(),
            merge_contract: false,
            ..green()
        };
        stored(write(&main, &failed).expect("write"));
        assert_eq!(
            by_tree(),
            pass,
            "a FAIL replaced the tree's whole-tree PASS"
        );
        assert!(
            store.join(&reworded).is_file(),
            "its own commit key is filed"
        );

        // The control: other bytes, another tree, nothing filed under it.
        std::fs::write(main.join("f"), "other bytes\n").expect("write");
        git(&main, &["commit", "-q", "-am", "three"]);
        let other = rev(&main, "HEAD^{tree}");
        assert_ne!(other, tree);
        assert!(!store.join(tree_key(&other)).exists());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
