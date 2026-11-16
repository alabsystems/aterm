// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SCOPED CLAIM'S PROOF (contracts 3 and 4): a registered harness's scoped claim
//! (`grok-alab@<pid> contract=4 scope=relaunch,upgrade version=0.3.0 child=<pid>:<start_us>`,
//! [`aterm_agent::harness::contract`]), proven on the kernel's facts and the row's image rule,
//! and kept beside the session's claim apart from codex-alab's v2 proof; under contract 4 the
//! vendor process it names is kept on aterm's own kernel reads ([`keep_child`]), and what reads
//! the agent reads that child ([`agent_pid`]).
//!
//! The kernel rules are codex-alab's (`crates/aterm-gui/src/control.rs`,
//! `prove_self_claim_by`): the claim arrives on a direct local connection whose kernel peer pid is
//! `<pid>`, and `<pid>` leads the session's foreground process group. The image rule is the
//! claim's contract's ([`image_for`]): under contract 4 the registry row's (a verified build, a
//! seed row's store build, or a development link under its rule); under contract 3, which only a
//! seed id claims, the rule contract 3 always had (`docs/DESIGN-harness-repos-2026-10-24.md`
//! §8.1 item 3): `<pid>`'s executable resolves inside this user's atpkg
//! `store/<package>/<build>/` with that build complete (`<build>.ready`), or inside the checkout
//! of an `aterm pkg link <package>` dev link, whatever row the registry composed for the id. The
//! holder string alone proves nothing. A standing claim's re-proof (a fenced input's, an act's)
//! reads the kernel's facts afresh and reuses the image verdict its claimant last passed while
//! nothing that verdict was judged on moved ([`KeptImage`]).
//!
//! A proven scoped claim is never a v2 self-claim: it sits in
//! [`crate::session_timeline::SupervisorClaim::scoped`], never in `proven`, so nothing that acts
//! for codex-alab's self-claim (its record copies, its crash rule, its wall answer, its facts)
//! acts for it. What it changes is what its scope says: `status supervisor_proven=1` (the
//! claimant's own confirmation), a claim the window's host holds replaced, as a proof does, and
//! its own acts, stop, Land hold, NEEDS YOU and continuation report admitted under its proof
//! where its scope takes the duty each reports on
//! ([`crate::session_timeline::SupervisorClaim::act_proof_for`]).

use std::path::Path;
use std::sync::PoisonError;

use aterm_agent::harness::contract::{ChildWord, Claim, Row};

/// The claimant's identity a record stream's state is keyed on (pid and kernel start). The type
/// stays in [`crate::self_claim`] until that module goes; stream code names it from here.
#[cfg_attr(not(unix), allow(unused_imports))]
pub(crate) use crate::self_claim::Proven;
use aterm_uds::process::{Birth, Lineage};

/// A proven scoped claim: the process the kernel named, what it claimed, and the vendor process
/// aterm kept for it (contract 4 only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scoped {
    /// The claimant's identity (pid and kernel start), as the proof captured it.
    pub(crate) proven: crate::self_claim::Proven,
    /// The claim as read: its row, contract, scope, versions and named child.
    pub(crate) claim: Claim,
    /// The child aterm kept on its own reads ([`keep_child`]); `None` where the claim named none
    /// or the one it named failed. A claim without a kept child still stands.
    pub(crate) child: Option<KeptChild>,
}

impl Scoped {
    /// WHETHER THIS CLAIM HEARS `restart` FOR ITS CHILD'S MODEL: it holds `relaunch` (contract 3
    /// or 4), or, under contract 4, `accounts` (the duty of moving its child between logins, which
    /// a restart on another login carries). It hears an account move only where
    /// [`Self::hears_account_move`] holds too.
    pub(crate) fn hears_restart(&self) -> bool {
        use aterm_agent::harness::contract::Scope;
        self.claim.scopes.contains(Scope::Relaunch) || self.hears_account_move()
    }

    /// WHETHER THIS CLAIM IS TOLD AN ACCOUNT MOVE (`restart cause=account|account-ahead
    /// to=<label> home=<home>`): only where it was granted `accounts` under contract 4
    /// (`contract::grant`: a verified record's claim or a seed row's), because the event names the
    /// account's home, which aterm hands only to such a build. A claim holding `relaunch` without
    /// it (contract 3, whose grammar has no `accounts`, or a development link's contract 4 claim,
    /// granted every word but `accounts`) is told no account move, as its reply said it was not
    /// granted one, and no move is planned for it; it still hears a model restart.
    pub(crate) fn hears_account_move(&self) -> bool {
        use aterm_agent::harness::contract::{Scope, UNIVERSAL};
        self.claim.contract == UNIVERSAL && self.claim.scopes.contains(Scope::Accounts)
    }

    /// The process the vendor runs as: the kept child while it still reads at its birth
    /// (`birth_of`, live [`aterm_uds::process::read_birth`]), else the claimant, whose
    /// environment a harness without a kept child hands its child.
    pub(crate) fn vendor_pid(&self, birth_of: &dyn Fn(i32) -> Option<Birth>) -> u32 {
        self.child
            .filter(|kept| {
                i32::try_from(kept.pid)
                    .ok()
                    .and_then(birth_of)
                    .is_some_and(|birth| birth == kept.birth)
            })
            .map_or(self.proven.pid, |kept| kept.pid)
    }
}

/// A contract 4 claim's child as aterm keeps it: every later check compares aterm's own reading
/// ([`Self::birth`]), never the harness's number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeptChild {
    /// The child's pid.
    pub(crate) pid: u32,
    /// The start the claim named, microseconds since the Unix epoch.
    pub(crate) claimed_us: u64,
    /// aterm's own read of its start ([`aterm_uds::process::read_birth`]'s unit: wall
    /// microseconds on Apple, clock ticks after boot on Linux).
    pub(crate) birth: Birth,
}

/// The longest parent walk from a claim's child to its claimant ([`keep_child`]): a harness's
/// child sits one or two exec layers below it (a shell, a launcher), never sixteen.
pub(crate) const CHILD_WALK_MAX: usize = 16;

/// KEEP A CONTRACT 4 CLAIM'S CHILD, on aterm's own kernel reads (`read_lineage`, live
/// [`aterm_uds::process::read_lineage`]), for the claimant `claimant` (its proof just made), the
/// session's foreground group `fg` (read from the PTY master) and the claim's `word`. The child is
/// kept only when every one holds:
///
/// 1. the kernel reads it, and it is not the claimant;
/// 2. the claimant leads the foreground group (`fg` is its pid) and the child is in that group;
/// 3. it descends from the claimant, by a walk of at most [`CHILD_WALK_MAX`] parents, and reads
///    as the same process (pid and birth) after the walk;
/// 4. its start matches the claim's: exactly on macOS (the birth is `pbi_start_tvsec * 1e6 +
///    pbi_start_tvusec`, the harness's own reading), within one second plus a tick on Linux,
///    whose birth is clock ticks after boot ([`start_matches`]).
///
/// `None` otherwise; the claim stands without a child.
pub(crate) fn keep_child(
    claimant: &crate::self_claim::Proven,
    fg: i32,
    word: ChildWord,
    read_lineage: &dyn Fn(i32) -> Option<Lineage>,
) -> Option<KeptChild> {
    let pid = i32::try_from(word.pid).ok()?;
    let lineage = read_lineage(pid)?;
    if word.pid == claimant.pid || u32::try_from(lineage.identity.pid).ok() != Some(word.pid) {
        return None;
    }
    if i64::from(fg) != i64::from(claimant.pid) || i64::from(lineage.pgid) != i64::from(fg) {
        return None;
    }
    let mut parent = lineage.parent;
    let mut reached = false;
    for _ in 0..CHILD_WALK_MAX {
        if parent == claimant.pid {
            reached = true;
            break;
        }
        if parent <= 1 {
            break;
        }
        parent = read_lineage(i32::try_from(parent).ok()?)?.parent;
    }
    if !reached {
        return None;
    }
    let again = read_lineage(pid)?;
    if again.identity.pid != lineage.identity.pid || again.identity.birth != lineage.identity.birth
    {
        return None;
    }
    start_matches(lineage.identity.birth, word.start_us).then_some(KeptChild {
        pid: word.pid,
        claimed_us: word.start_us,
        birth: lineage.identity.birth,
    })
}

/// Whether aterm's `birth` of a process is the start `claimed_us` a harness read for it:
/// exactly on Apple, where both are the kernel's wall record in microseconds.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn start_matches(birth: Birth, claimed_us: u64) -> bool {
    birth.0 == claimed_us
}

/// Whether aterm's `birth` of a process (clock ticks after boot) is the start `claimed_us` a
/// harness read for it, `btime * 1e6 + starttime * 1e6 / HZ`: within one second plus a tick,
/// since `btime` is whole seconds read at another moment ([`linux_start_matches`]).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn start_matches(birth: Birth, claimed_us: u64) -> bool {
    let (Some(boot_s), Some(per_second)) = (
        aterm_uds::process::boot_s(),
        aterm_uds::process::stat_ticks_per_second(),
    ) else {
        return false;
    };
    linux_start_matches(birth.0, claimed_us, boot_s, per_second)
}

/// [`start_matches`]'s Linux arithmetic: `ticks` after a boot at `boot_s`, `per_second` ticks a
/// second, against `claimed_us`, within one second plus one tick.
#[cfg(any(test, target_os = "linux", target_os = "android"))]
pub(crate) fn linux_start_matches(
    ticks: u64,
    claimed_us: u64,
    boot_s: u64,
    per_second: u64,
) -> bool {
    if per_second == 0 {
        return false;
    }
    let ours =
        u128::from(boot_s) * 1_000_000 + u128::from(ticks) * 1_000_000 / u128::from(per_second);
    let slack = 1_000_000 + 1_000_000u128.div_ceil(u128::from(per_second));
    ours.abs_diff(u128::from(claimed_us)) <= slack
}

/// THE AGENT'S PID in `ctx`'s session whose foreground group is `group`: the live scoped claim's
/// kept child while that claim's claimant leads `group` and the child still reads at the birth
/// aterm kept, else `group` itself. What reads the agent (the engine's image gate) reads this;
/// what reads the tab's process (the claim's proof, a restart's target, the relaunch snapshot)
/// stays on the leader.
pub(crate) fn agent_pid(ctx: &crate::SessionCtx, group: i32) -> i32 {
    agent_pid_with(ctx, group, &aterm_uds::process::read_birth)
}

/// [`agent_pid`] with the kernel's birth read by `birth_of`.
pub(crate) fn agent_pid_with(
    ctx: &crate::SessionCtx,
    group: i32,
    birth_of: &dyn Fn(i32) -> Option<Birth>,
) -> i32 {
    let kept = ctx
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live_scoped(crate::metrics::now_us())
        .filter(|scoped| i64::from(scoped.proven.pid) == i64::from(group))
        .and_then(|scoped| scoped.child);
    kept.and_then(|child| {
        let pid = i32::try_from(child.pid).ok()?;
        (birth_of(pid) == Some(child.birth)).then_some(pid)
    })
    .unwrap_or(group)
}

/// The `supervisor_version=` and `supervisor_vendor=` words of `status` and `sessions`: the
/// versions a proven contract v3 claim announced
/// ([`crate::session_timeline::SessionMeta::live_scoped`],
/// `docs/DESIGN-harness-repos-2026-10-24.md` §5, §11 D48), percent-encoded; `-` for a word the
/// claim did not name, and for both under any other claim or none.
#[must_use]
pub(crate) fn wire_versions(scoped: Option<&Scoped>) -> (String, String) {
    let word = |v: Option<&str>| v.map_or_else(|| "-".to_string(), aterm_control::wire::pct_encode);
    match scoped {
        Some(s) => (
            word(s.claim.version.as_ref().map(|v| v.text.as_str())),
            word(s.claim.vendor.as_deref()),
        ),
        None => ("-".to_string(), "-".to_string()),
    }
}

/// Return the claimed pid if `facts` prove rostered `holder`'s scoped claim: the kernel's peer is
/// the pid the holder names, which leads the session's foreground group, and its image passes the
/// claim's rule (`facts.alab_image`, read by [`runs_harness_build`]).
#[must_use]
#[cfg(test)]
pub(crate) fn proves(holder: &str, facts: crate::self_claim::Facts) -> Option<u32> {
    proves_in(
        aterm_agent::harness::registry::current().rows(),
        holder,
        facts,
    )
}

fn proves_in(rows: &'static [Row], holder: &str, facts: crate::self_claim::Facts) -> Option<u32> {
    let (_, pid) = aterm_agent::harness::contract::holder_in(rows, holder)?;
    let leads = i64::from(facts.foreground) == i64::from(pid);
    (facts.peer == Some(pid) && leads && facts.alab_image).then_some(pid)
}

/// Prove rostered `holder`'s claim on `facts` ([`proves`]) and capture its kernel start.
#[must_use]
#[cfg(test)]
pub(crate) fn prove(
    holder: &str,
    facts: crate::self_claim::Facts,
) -> Option<crate::self_claim::Proven> {
    prove_in(
        aterm_agent::harness::registry::current().rows(),
        holder,
        facts,
    )
}

/// The same kernel proof and birth capture under the session's explicit catalog.
pub(crate) fn prove_in(
    rows: &'static [Row],
    holder: &str,
    facts: crate::self_claim::Facts,
) -> Option<crate::self_claim::Proven> {
    let pid = proves_in(rows, holder, facts)?;
    let start = aterm_agent::harness::restart_stamp::kernel_start(pid)?;
    Some(crate::self_claim::Proven { pid, start })
}

/// Whether `exe` is a build of `row`'s harness in `layout`, by the image rule of `layout`'s
/// registry ([`aterm_agent::harness::registry::for_layout`]): inside a signed row's verified
/// build, a seed row's ready store build, or a development link's checkout under its rule.
#[must_use]
pub(crate) fn image_in(layout: &atpkg::store::Layout, row: &Row, exe: &Path) -> bool {
    aterm_agent::harness::registry::for_layout(layout).admits_image(layout, row, exe)
}

/// Whether `exe` may run as `row`'s claimant under `contract` in `layout`: a contract 3 claim,
/// which only a seed id makes, by the rule contract 3 always had (a ready, complete store build
/// of the row's package, or its dev link's checkout:
/// [`atpkg::harness_record::admits_seed_image`]), whatever row the registry composed for the id;
/// any other by the registry row's rule ([`image_in`]).
#[must_use]
pub(crate) fn image_for(
    layout: &atpkg::store::Layout,
    row: &Row,
    contract: u32,
    exe: &Path,
) -> bool {
    if contract == aterm_agent::harness::contract::SCOPED {
        atpkg::harness_record::admits_seed_image(layout, row.package, exe)
    } else {
        image_in(layout, row, exe)
    }
}

/// [`image_for`] under `registry`, the registry the proof read, never one read again: a contract 3
/// claim by contract 3's rule ([`atpkg::harness_record::admits_seed_image`]), any other by
/// `registry`'s row rule ([`aterm_agent::harness::registry::Registry::admits_image`]). A pass that
/// records the generation it proved against judges under the registry of that generation
/// ([`aterm_agent::harness::registry::current_at`]).
#[must_use]
pub(crate) fn image_by(
    registry: &aterm_agent::harness::registry::Registry,
    layout: &atpkg::store::Layout,
    row: &Row,
    contract: u32,
    exe: &Path,
) -> bool {
    if contract == aterm_agent::harness::contract::SCOPED {
        atpkg::harness_record::admits_seed_image(layout, row.package, exe)
    } else {
        registry.admits_image(layout, row, exe)
    }
}

/// WHY A LIVE CONTRACT 4 CLAIM NO LONGER STANDS under the registry's `rows` as they read now
/// (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2): its row is gone, its row now comes from
/// elsewhere (its grant was that source's), or its claimant's image no longer passes the row's
/// rule (`admits`, live [`runs_harness_build`], which verifies the record again under the index
/// cache as it stands). `None` while it still stands, and for every contract 3 claim, whose seed
/// row and old build an update must not cut.
pub(crate) fn unproven_now(
    scoped: &Scoped,
    rows: &[Row],
    admits: &dyn Fn(&Row, u32) -> bool,
) -> Option<&'static str> {
    if scoped.claim.contract != aterm_agent::harness::contract::UNIVERSAL {
        return None;
    }
    let Some(row) = rows.iter().find(|row| row.id == scoped.claim.row.id) else {
        return Some("its registry row is gone");
    };
    if row.source != scoped.claim.row.source {
        return Some("its registry row now comes from elsewhere");
    }
    (!admits(row, scoped.proven.pid)).then_some("its image no longer passes its row's rule")
}

/// PROVE `ctx`'S LIVE CONTRACT 4 CLAIM AGAIN against `rows` ([`unproven_now`]) and drop it where it
/// fails: the claim is released (its revision rises, `meta-change field=supervisor value=-`) and
/// the log says why. What the drop said, `None` where nothing was dropped. `admits` reads files,
/// so it runs with no lock held, and the release takes only the very claim it judged.
pub(crate) fn reprove(
    ctx: &crate::SessionCtx,
    rows: &[Row],
    admits: &dyn Fn(&Row, u32) -> bool,
    now_us: u64,
) -> Option<&'static str> {
    let standing = ctx
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live_scoped(now_us)
        .cloned()?;
    let why = unproven_now(&standing, rows, admits)?;
    let dropped = crate::session_timeline::publish_release_scoped(
        ctx,
        &standing,
        now_us,
        aterm_slog::Actor::Aterm,
    )
    .published();
    if !dropped {
        return None;
    }
    aterm_log::warn!(
        "harness claim @{}: {}@{}'s contract 4 claim is dropped: {why}",
        ctx.self_id.as_str(),
        standing.claim.row.id,
        standing.proven.pid
    );
    Some(why)
}

/// The development seam that makes this aterm answer as one predating contract 3:
/// `ATERM_DEV_NO_CONTRACT3=1` in a debug or dev-seams build's environment
/// ([`aterm_types::dev_seam`]; a shipped aterm reads no such variable). `lab harness-proof`'s
/// negative control (`docs/DESIGN-harness-repos-2026-10-24.md` §9) runs its instance under
/// it, so a harness must pass through (§8.2) where no `contract=` is served.
pub(crate) const NO_CONTRACT3_SEAM: &str = "ATERM_DEV_NO_CONTRACT3";

/// The `contract=` value this aterm answers `harness policy` with ([`served_with`] of the seam,
/// read once).
#[must_use]
pub(crate) fn served() -> Option<&'static str> {
    static SERVED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SERVED
        .get_or_init(|| served_with(aterm_types::dev_seam!(NO_CONTRACT3_SEAM).as_deref()))
        .as_deref()
}

/// The `contract=` value for a seam value `seam`: the contracts this aterm serves
/// ([`aterm_agent::harness::contract::served_word`], `2,3,4`), or `None` under
/// `ATERM_DEV_NO_CONTRACT3=1` ([`NO_CONTRACT3_SEAM`]), where the reply carries no `contract=` key
/// at all, as an aterm predating contract 3 answers. Any other value serves.
#[must_use]
pub(crate) fn served_with(seam: Option<&std::ffi::OsStr>) -> Option<String> {
    (seam != Some(std::ffi::OsStr::new("1"))).then(aterm_agent::harness::contract::served_word)
}

/// Whether a `meta set supervisor <holder> <words…>` is refused as an aterm predating contract 3
/// refuses it: it serves no contract 3 (`serves` false, [`served`]) and a word names `contract=`,
/// which the v2 grammar (`[ttl=<ms>]`) never took. A v2 claim is read as before either way.
#[must_use]
pub(crate) fn refused_before_v3(serves: bool, words: &[&str]) -> bool {
    !serves && words.iter().any(|w| w.starts_with("contract="))
}

/// Whether process `pid` runs a build of `row`'s program under `contract`'s image rule
/// ([`image_for`], this user's configured atpkg prefix).
#[must_use]
pub(crate) fn runs_harness_build(row: &Row, contract: u32, pid: u32) -> bool {
    let Some(exe) = i32::try_from(pid).ok().and_then(aterm_uds::sudo::exe_path) else {
        return false;
    };
    atpkg::store::resolve_configured().is_some_and(|layout| image_for(&layout, row, contract, &exe))
}

/// [`runs_harness_build`] under `registry` ([`image_by`]): what a pass that read the registry
/// asks, so the rows it judges and the image rule it applies are one read's.
#[must_use]
pub(crate) fn runs_harness_build_in(
    registry: &aterm_agent::harness::registry::Registry,
    row: &Row,
    contract: u32,
    pid: u32,
) -> bool {
    let Some(exe) = i32::try_from(pid).ok().and_then(aterm_uds::sudo::exe_path) else {
        return false;
    };
    atpkg::store::resolve_configured()
        .is_some_and(|layout| image_by(registry, &layout, row, contract, &exe))
}

/// A claimant whose image a proof judges: the holder it claims as, its row, the contract whose
/// rule judges it, and its pid and kernel start as the proof read them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Claimant<'a> {
    /// The holder, `<id>@<pid>`.
    pub(crate) holder: &'a str,
    /// The row the holder names.
    pub(crate) row: &'a Row,
    /// The contract whose image rule judges it ([`image_by`]).
    pub(crate) contract: u32,
    /// The pid the holder names.
    pub(crate) pid: u32,
    /// Its kernel start ([`aterm_agent::harness::restart_stamp::kernel_start`]).
    pub(crate) start: &'a str,
}

/// An executable file's identity as the kernel keeps it: device, inode, length, and modification
/// and change time in nanoseconds (no user call sets the change time back).
type FileStamp = (u64, u64, u64, i128, i128);

/// `path`'s [`FileStamp`], following links; `None` where it cannot be read, and off Unix.
fn file_stamp(path: &Path) -> Option<FileStamp> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(path).ok()?;
        let nanos = |s: i64, ns: i64| i128::from(s) * 1_000_000_000 + i128::from(ns);
        Some((
            m.dev(),
            m.ino(),
            m.len(),
            nanos(m.mtime(), m.mtime_nsec()),
            nanos(m.ctime(), m.ctime_nsec()),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// WHAT A CLAIMANT'S IMAGE VERDICT WAS REACHED ON: its holder, the contract whose rule judged it,
/// its kernel start, its executable's path and file identity ([`FileStamp`]), and the generation
/// of the registry the rule was read from ([`aterm_agent::harness::registry::current_at`]). Equal
/// keys are judged alike, so a re-proof whose key is the last pass's needs no image proof
/// ([`KeptImage`]). The record's tree root is not in it: the full proof writes its record back,
/// so a key holding it would lapse at every pass; a tree changed in place elsewhere than the
/// executable is judged at the next claim, or at the next generation's re-proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageKey {
    holder: String,
    contract: u32,
    start: String,
    exe: std::path::PathBuf,
    image: FileStamp,
    generation: u64,
}

impl ImageKey {
    /// The key of `claimant` running `exe`, judged under the registry of `generation`; `None`
    /// where `exe`'s identity cannot be read, for which no verdict is kept.
    #[must_use]
    pub(crate) fn of(claimant: &Claimant<'_>, exe: &Path, generation: u64) -> Option<Self> {
        Some(Self {
            holder: claimant.holder.to_string(),
            contract: claimant.contract,
            start: claimant.start.to_string(),
            exe: exe.to_path_buf(),
            image: file_stamp(exe)?,
            generation,
        })
    }
}

/// THE IMAGE VERDICT A SESSION'S CLAIMANT LAST PASSED (HX-05's review, finding 1): what a fenced
/// input's or an act's re-proof of a standing claim reuses, so neither verifies the build's
/// signatures, hashes its tree nor reads aterm.toml again while nothing it was judged on moved
/// ([`ImageKey`]). The claim's own proof judges in full, and so does the host's re-proof of live
/// contract 4 claims at each new registry generation ([`reprove`]).
#[derive(Debug, Default)]
pub(crate) struct KeptImage(std::sync::Mutex<Option<ImageKey>>);

impl KeptImage {
    /// Whether the claimant `key` names passes its image rule, by `judge`: with `reuse` (a standing
    /// claim's re-proof) and `key` the last pass's, the kept pass stands and `judge` is not asked;
    /// otherwise `judge` is asked, a pass is kept for the next re-proof and a refusal keeps
    /// nothing. A claimant with no key (its executable's identity unread) is judged every time.
    /// `judge` reads files, so it runs with no lock held.
    pub(crate) fn passes(
        &self,
        key: Option<ImageKey>,
        reuse: bool,
        judge: impl FnOnce() -> bool,
    ) -> bool {
        let kept = || self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if reuse && key.is_some() && *kept() == key {
            return true;
        }
        let passed = judge();
        *kept() = key.filter(|_| passed);
        passed
    }
}

#[cfg(test)]
#[path = "harness_claim_tests.rs"]
mod tests;
