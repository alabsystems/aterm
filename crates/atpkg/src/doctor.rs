// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `atpkg doctor` health surface (§15) — a no-network, no-mutation diagnostic over
//! atpkg's own store/ops/sig/status primitives.
//!
//! Structural breakage (a broken bin shim, an active build whose store tree vanished, a
//! fish-breaking stray `.sh`, a world-writable login-sourced dir) is a PROBLEM → nonzero
//! exit. Everything advisory (bin not yet on PATH, a frozen-looking index, a foreign
//! sysroot wiring) stays a WARNING → exit 0. One kind of warning also withholds the
//! word "healthy": a managed tool that cannot run (today, tippy refused by its own Trust
//! bundle — a `trustc` that is a symbolic link, or a `bin/rustc` that is a separate file
//! from `trustc` with no exec root routing the shims past it), or one doctor cannot show
//! to run (an exec root that differs from its build, a build it cannot read), is not a
//! structural fault in the store, and not health either, so the report ends "not healthy"
//! — counting each kind under its own words — and still exits 0, with the warn naming its
//! own fix. It reads no unverified index/manifest
//! (verify-before-parse): its freshness surface reads atpkg's OWN `status.toml` + the
//! durable [`crate::sig::Floor`].

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::store::Layout;

const GIB: u64 = 1 << 30;

/// Whether a recorded program `state` describes a FAULT this machine should act on.
///
/// The five fault prefixes are the ones the install/update paths actually write. Two were
/// missing until 2026-08-20 round-11, and both absences were the mirror image of the
/// stray-row bug fixed the same day: that one made doctor report a fault where none
/// existed, these made it MISS ones that did.
///
/// * `aborted: <phase>` (cli.rs, the `TxnOutcome::Aborted` arm) — a coherence-group
///   transaction killed mid-flight, precisely the state in which a tuple's members may
///   disagree with each other. Recorded against the FAILED member only, and on a group
///   disk-shortfall that member is an arbitrary one of the tuple, so the row names where
///   the transaction stopped rather than what is individually wrong.
/// * `tombstoned: pin yanked/below floor` — the worse of the two, because it is silent
///   everywhere else. A yanked pin replaces the program's working shims with failing
///   stubs that print "was yanked/revoked" ([`crate::activate::install_tombstone_shim`]);
///   the broken-shim scan skips tombstones BY DESIGN, and a tombstoned program drops out
///   of `active_builds` entirely. So doctor saw no broken shim, no active build, and no
///   recognized fault, and pronounced "healthy" on a machine whose compiler had become a
///   stub. Both clear themselves: a later successful update rewrites the row and replaces
///   the tombstone shim with a real symlink.
///
/// Deliberately NOT a fault: `active`, `dev-linked (skipped)` (a §13 hard-skip the user
/// asked for), `held: …` (a tuple whose new pin is not published for this target), and any
/// future informational state. Allow-by-prefix, so an unrecognized state reads as benign
/// rather than as a failure.
fn is_problem_state(state: &str) -> bool {
    state.starts_with("error:")
        || state.starts_with("unavailable:")
        || state.starts_with("blocked:")
        || state.starts_with("aborted:")
        || state.starts_with("tombstoned:")
}

/// Where the per-problem listing should START — or `None` when no verdict owns it.
///
/// The listing must hang off the branch that actually printed a verdict, which is why this
/// is a function rather than an expression at the loop. Deriving it from `store_empty`
/// alone was wrong in both directions on a DECLINED store: with an empty store it skipped
/// problem #1 that no branch had named (silently losing the only finding, since
/// `*toolset*: unavailable: …` is the normal single row on a Mac the index does not serve),
/// and with a populated store it printed every problem as an orphan line under "the ALab
/// toolset was removed on this machine" — lines describing a verdict nobody gave.
///
/// A declined store lists nothing: the decline IS the verdict, and a stale fault row
/// describes a toolset the user deliberately removed. Whether such a row should also flip
/// the exit code is a separate product question, deliberately not answered here.
fn problem_listing_start(declined: bool, store_empty: bool, problems: usize) -> Option<usize> {
    if declined || problems == 0 {
        None
    } else if store_empty {
        // That verdict names its first reason inline; listing resumes after it.
        Some(1)
    } else {
        Some(0)
    }
}

/// The programs this machine WANTS but does not have, split by whether the removed
/// ledger explains the absence: `(unexplained, on_purpose)`, each alphabetical.
///
/// "Wants" is [`crate::cli::wanted_programs`] — the signed default set narrowed by
/// `[packages].include`/`exclude` plus opted-in extras — so this asks the same question
/// the install lanes ask, and cannot drift from them into a second opinion.
///
/// The split is the whole point. A program on the removed ledger is a decision, and
/// reporting it as a problem would be the manager arguing with the user. A program
/// missing with NO ledger entry is the state nothing else in this report can see: no
/// shim, no `status.toml` row, no recorded fault — invisible to every check that reads
/// the record rather than the index.
///
/// Offline and fail-quiet: with no cached index there is no trustworthy answer about
/// what SHOULD be installed, and inventing one would turn an unreachable network into a
/// pile of phantom missing programs.
fn missing_against_index(
    layout: &Layout,
    installed: &std::collections::BTreeMap<String, u64>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let Some(index) = crate::cli::cached_index(layout) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    split_missing(
        &crate::cli::wanted_programs(layout, &index, crate::config::cached()),
        installed,
        &layout.removed_programs(),
        &crate::linkmode::linked_programs(layout)
            .into_iter()
            .collect(),
    )
}

/// The decision [`missing_against_index`] makes, without the I/O: which wanted programs
/// are absent, and which of those the removed ledger accounts for.
///
/// Pure so it can be tested against the exact shape of the incident that motivated it,
/// which is otherwise reachable only by standing up a signed index, a registry and a
/// store.
fn split_missing(
    wanted: &std::collections::BTreeSet<String>,
    installed: &std::collections::BTreeMap<String, u64>,
    removed: &std::collections::BTreeSet<String>,
    linked: &std::collections::BTreeSet<String>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut unexplained = Vec::new();
    let mut on_purpose = Vec::new();
    let mut dev_linked = Vec::new();
    for program in wanted {
        if installed.contains_key(program) {
            continue;
        }
        // A DEV-LINKED program is PRESENT — its shims are in the managed bin/ and they
        // run — it just has no store build for `installed` to count. It outranks the
        // removed ledger: this box carried a `# deliberate` removal of trust from
        // 2026-08-27, and after `aterm pkg link trust <stage2>` doctor still said
        // "removed on purpose — no unattended pass reinstalls it" about a toolchain that
        // was running every build on the machine. The link is the newer decision.
        if linked.contains(program) {
            dev_linked.push(program.clone());
        } else if removed.contains(program) {
            on_purpose.push(program.clone());
        } else {
            unexplained.push(program.clone());
        }
    }
    (unexplained, on_purpose, dev_linked)
}

/// EVERY recorded fault, formatted `"<program>: <state>"`, in program order — `BTreeMap`
/// order, i.e. alphabetical by program name.
///
/// Pulled out of [`run_with`] so it can be tested for what it actually promises. This used
/// to be a `.find()` inline, which returned at most ONE finding: a second failing program
/// stayed invisible until the first was repaired, and since `status.toml` is a `BTreeMap`,
/// which one you were shown was alphabetical accident. A diagnostic that reveals its
/// findings one per repair cycle is a guessing game.
///
/// Stray rows (keys beginning with `-`) are excluded — see the caller: they cannot be
/// programs at all, and counting them condemned healthy toolsets.
pub(crate) fn recorded_problems(status: Option<&crate::Status>) -> Vec<String> {
    status
        .map(|s| {
            s.programs
                .iter()
                .filter(|(name, _)| !name.starts_with('-'))
                .filter(|(_, p)| is_problem_state(&p.state))
                .map(|(name, p)| format!("{name}: {}", p.state))
                .collect()
        })
        .unwrap_or_default()
}

/// The doctor line for a bundle whose executables carry `com.apple.provenance` — the
/// count, one example, the CAUSE when the store recorded one (`recorded`: the
/// `<build>.tracked-install` record a tracked installer left when its untracked lane
/// could not run, [`crate::store::tracked_install_record`]), what it breaks, and the
/// cure. Pure, so the words are pinned by a test that mints a synthetic attribute rather
/// than the one it cannot.
pub(crate) fn provenance_bundle_line(
    program: &str,
    build: u64,
    bin: &Path,
    scan: &crate::provenance::Scan,
    recorded: Option<&str>,
) -> String {
    let example = scan
        .carriers
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cause = match recorded {
        // The RECORD's own reason, verbatim — never a mechanism this line picks. Two
        // outcomes write the record (`install::decide_tracked_stage`): an in-process
        // stage, and a KEPT lane tree that came back tagged. Naming the first as the
        // cause made the second read "staged in-process … because the untracked lane
        // ran", a sentence contradicting its own evidence.
        Some(why) => format!(" — recorded cause: {why}"),
        None => String::new(),
    };
    format!(
        "warn — {program} build {build} carries com.apple.provenance on {} of {} file(s) in {} \
         (e.g. {example}){cause} — {}. fix: {}",
        scan.carriers.len(),
        scan.total,
        bin.display(),
        crate::provenance::WHAT_IT_BREAKS,
        crate::provenance::REMEDY.replace("<program>", program),
    )
}

/// The doctor line for a build that carries a tracked-install record but whose `bin/`
/// scan finds no tagged file: the record is stale (a re-stage cleared the tag but not,
/// somehow, the record) and says so rather than accusing a clean bundle.
pub(crate) fn provenance_stale_record_line(
    program: &str,
    build: u64,
    bin: &Path,
    recorded: &str,
) -> String {
    format!(
        "warn — {program} build {build} carries a tracked-install record ({recorded}) but no \
         file in {} carries com.apple.provenance — a stale record; `aterm pkg uninstall \
         {program} && aterm pkg install {program}` re-stages and clears it",
        bin.display()
    )
}

/// The doctor line for tagged shims in the managed `bin/`: a shim is exec'd as a script,
/// so the tag on it tracks the tool it forwards to whatever the bundle carries.
pub(crate) fn provenance_shim_line(bin_dir: &Path, scan: &crate::provenance::Scan) -> String {
    let example = scan
        .carriers
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "warn — {} of {} shim(s) in {} carry com.apple.provenance (e.g. {example}) — a shim is \
         a `#!/bin/sh` script the kernel execs, so a tagged shim makes the tool it forwards to \
         provenance-tracked even when the bundle behind it is clean (its output is tagged, and \
         a release cut built through it is refused after the claim). fix: `aterm pkg repair` \
         re-lays them — this atpkg lays shims through an untracked launchd job when it \
         measures itself as tracked, so any shell will do; the cutter itself resolves the \
         bundle's bin/ directly and never goes through a shim",
        scan.carriers.len(),
        scan.total,
        bin_dir.display(),
    )
}

/// What the WORKSPACE the operator is standing in demands of the installed Trust
/// toolchain, as decided by the installed `targo` itself (`targo locate-project
/// --workspace` parses the manifest, compiles nothing, and refuses a `[trust]` policy
/// key it does not know).
///
/// This is the check that names the 2026-09-10 failure in one line. The `ty` repo
/// gained `[trust] compiler_timeout_secs` on 2026-09-09 from a machine building on a
/// LOCAL Trust seal newer than anything atpkg had published; every atpkg-managed
/// machine then failed to open the workspace with targo, with an "unknown field"
/// error that reads like a typo and says nothing about which toolchain is behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspacePolicy {
    /// The installed targo accepts the workspace's `[trust]` policy.
    Accepted,
    /// The installed targo refuses a policy key it does not know: the workspace
    /// needs a NEWER Trust build than atpkg has published.
    UnknownField { field: String },
    /// The probe could not run (no targo in the store, spawn failure); reported, never
    /// counted as a problem.
    ProbeFailed { why: String },
}

/// The workspace probe's result, bound to the directory it was taken in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePolicyProbe {
    pub workspace: PathBuf,
    pub policy: WorkspacePolicy,
}

/// rustup's `trust` channel resolving OUTSIDE the atpkg store: a locally built or
/// sealed toolchain. Not wrong in itself — a publisher machine builds Trust — but
/// anything committed against a feature only that toolchain has will not build on an
/// atpkg-managed machine until the seal is PUBLISHED. Naming it is what lets the
/// operator see, on the machine that seals, that the seal is still local only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSealProbe {
    pub link_target: PathBuf,
    /// `trustc -Vv`'s commit hash (first 10 characters), or "unknown".
    pub trustc: String,
}

/// The environment-dependent probes `run` takes and `run_with` only reports, so the
/// reporting surface stays testable without spawning targo or reading rustup state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probes {
    pub workspace: Option<WorkspacePolicyProbe>,
    pub local_seal: Option<LocalSealProbe>,
    /// The extended attribute the bundle/shim scan looks for —
    /// [`crate::provenance::PROVENANCE_XATTR`] in production. Injected because that name
    /// cannot be minted by a test (`xattr -w com.apple.provenance` is refused), so the
    /// scan is exercised with a `user.*` attribute through the very `listxattr` path.
    pub provenance_attr: &'static str,
}

impl Default for Probes {
    fn default() -> Self {
        Self {
            workspace: None,
            local_seal: None,
            provenance_attr: crate::provenance::PROVENANCE_XATTR,
        }
    }
}

/// The publisher-side act that cures an `UnknownField` verdict: publish the newer
/// Trust coherence group from the seal the workspace was committed against.
pub const PUBLISH_RUSTC_GROUP: &str = "tools/atpkg-publish-rustc-group.sh";

/// Find the nearest ancestor of `start` (inclusive) whose `Cargo.toml` carries a
/// `[trust]` table. `None` when no manifest on the way up declares one.
fn workspace_with_trust_table(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && text.lines().any(|l| l.trim() == "[trust]")
        {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// The field name out of targo's refusal, e.g. ``unknown field `compiler_timeout_secs`,
/// expected one of …`` → `compiler_timeout_secs`.
fn unknown_field_in(stderr: &str) -> Option<String> {
    let marker = "unknown field `";
    let start = stderr.find(marker)? + marker.len();
    let rest = &stderr[start..];
    let end = rest.find('`')?;
    let field = &rest[..end];
    (!field.is_empty()).then(|| field.to_string())
}

/// Ask the INSTALLED targo whether it accepts the workspace's `[trust]` policy.
/// `locate-project --workspace` parses the manifest and compiles nothing.
fn probe_workspace_policy(layout: &Layout, cwd: &Path) -> Option<WorkspacePolicyProbe> {
    let workspace = workspace_with_trust_table(cwd)?;
    let trust_build = crate::ops::active_builds(layout)
        .into_iter()
        .find(|(p, _)| p == "trust")
        .map(|(_, b)| b)?;
    let targo = layout
        .build_dir("trust", trust_build)
        .join("bin")
        .join("targo");
    if !targo.is_file() {
        return Some(WorkspacePolicyProbe {
            workspace,
            policy: WorkspacePolicy::ProbeFailed {
                why: format!("no targo at {}", targo.display()),
            },
        });
    }
    let probe = std::process::Command::new(&targo)
        .args(["locate-project", "--workspace"])
        .current_dir(&workspace)
        .env_remove("RUSTFLAGS")
        .output();
    let policy = match probe {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            match unknown_field_in(&stderr) {
                Some(field) => WorkspacePolicy::UnknownField { field },
                None if out.status.success() => WorkspacePolicy::Accepted,
                None => WorkspacePolicy::ProbeFailed {
                    why: stderr.lines().next().unwrap_or("targo failed").to_string(),
                },
            }
        }
        Err(e) => WorkspacePolicy::ProbeFailed { why: e.to_string() },
    };
    Some(WorkspacePolicyProbe { workspace, policy })
}

/// Where rustup's `trust` channel points, when that is NOT inside the atpkg store. The
/// rustup home is the one rustup itself resolves — `$RUSTUP_HOME`, else `<home>/.rustup`
/// ([`crate::seam::rustup_home_with`]) — so a relocated home is probed, not skipped.
fn probe_local_seal(layout: &Layout, home: Option<&Path>) -> Option<LocalSealProbe> {
    let link = crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)?
        .join("toolchains")
        .join("trust");
    let target = std::fs::read_link(&link).ok()?;
    let target = if target.is_absolute() {
        target
    } else {
        link.parent()?.join(target)
    };
    let canonical_target = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
    let canonical_prefix =
        std::fs::canonicalize(&layout.prefix).unwrap_or_else(|_| layout.prefix.clone());
    if canonical_target.starts_with(&canonical_prefix) {
        return None;
    }
    let trustc = target.join("bin").join("trustc");
    let commit = std::process::Command::new(&trustc)
        .arg("-Vv")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("commit-hash: ")
                    .map(|c| c.chars().take(10).collect::<String>())
            })
        })
        .unwrap_or_else(|| "unknown".to_string());
    Some(LocalSealProbe {
        link_target: target,
        trustc: commit,
    })
}

/// Run the health surface, printing the report. Returns `true` iff there were NO structural
/// problems (`main` maps `false` → exit 1). Reads the real environment (home + PATH + clock
/// + the `[packages]` config account + the token chain's SOURCE label — never the token).
#[must_use]
pub fn run(layout: &Layout, prefix: &str) -> bool {
    let home = aterm_types::dirs::home_dir();
    let path = std::env::var_os("PATH");
    let cfg_account = crate::config::cached().account().map(str::to_string);
    // Which source supplies a GitHub token (§5.1 private-repo aid): `$ATPKG_TOKEN`,
    // else aterm-update-core's chain. Only the LABEL is surfaced.
    let (_token, token_source) = crate::cli::resolve_pkg_token(layout);
    let probes = Probes {
        workspace: std::env::current_dir()
            .ok()
            .and_then(|cwd| probe_workspace_policy(layout, &cwd)),
        local_seal: probe_local_seal(layout, home.as_deref()),
        provenance_attr: crate::provenance::PROVENANCE_XATTR,
    };
    run_with(
        layout,
        home.as_deref(),
        path.as_deref(),
        crate::flow::now_unix(),
        cfg_account.as_deref(),
        token_source.as_deref(),
        prefix,
        &probes,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// The testable core: `home`, the `PATH` value, `now`, the `[packages].account`
/// config override, and the resolved token-source LABEL are injected so the surface
/// can be exercised against a synthetic environment without mutating the process env
/// (or spawning the keychain/`gh` probes).
#[must_use]
#[allow(clippy::too_many_arguments)] // the injected environment, plus the speaker and its streams
pub fn run_with(
    layout: &Layout,
    home: Option<&Path>,
    path_var: Option<&OsStr>,
    now: i64,
    cfg_account: Option<&str>,
    token_source: Option<&str>,
    prefix: &str,
    probes: &Probes,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> bool {
    // THE SPEAKER IS THE VERB THE USER TYPED. `status` is an alias for this same report,
    // and it used to answer every line as "doctor:" — so a user could not tell which verb
    // they had run, and a script keying on the prefix keyed on the wrong verb. Same
    // checks, same exit codes; only the signature matches the invocation.
    let p = prefix;
    let mut fails = 0usize;

    // (0) WHICH atpkg IS SPEAKING. Every line below is only as good as the binary printing
    // it, and that binary is NOT self-evident: `atpkg` ships inside the aterm app, so a
    // machine can easily have several — an installed /Applications copy, a dev `dist/`
    // build, a `target/release` one — and `which atpkg` picks whichever PATH happens to
    // reach first.
    //
    // This is not hypothetical. Measured 2026-08-20: a stale in-bundle atpkg answered
    // "trust is not installed" for a store that a current atpkg verified as
    // "build 6459 OK (matches signed tree_root)". A wrong answer from a manager about its
    // own store is the most expensive kind of wrong, because the natural next step is to
    // reinstall something that was never broken. Naming the speaker makes that verifiable
    // in one line instead of an afternoon.
    let _ = writeln!(
        out,
        "{p}: this atpkg is {} at {}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown path>".to_string())
    );
    report_aterm_posture(layout, p, out);

    // (1) TRUST ROOT + INDEX SOURCE + TOKEN SOURCE.
    let _ = writeln!(
        out,
        "{p}: index source github.com/{}",
        crate::resolve_account(cfg_account).slug()
    );
    if crate::manager_enabled() {
        // The root is the PAPER MASTER — the same anchor the app updater uses, not a
        // package-specific key. Naming it here is what lets an operator answer "which
        // trust root is this build on?" without reading source.
        let _ = writeln!(
            out,
            "{p}: ok — paper master pinned (fingerprint {}, {} key(s))",
            crate::root_key_fingerprint(),
            crate::PKG_TRUST_ANCHORS.len()
        );
    } else {
        let _ = writeln!(
            out,
            "{p}: warn — disabled/inert (no paper master compiled in \
             (pins::PAPER_MASTER_PUBKEYS is empty), or ATPKG_DISABLE set) — this build \
             installs nothing"
        );
    }
    // Loud token provenance (never the token itself): which source of the
    // `$ATPKG_TOKEN` → aterm-update-core chain (env → keychain → 0600 file →
    // `$GITHUB_TOKEN`/`$GH_TOKEN` → `gh auth token`) supplied a credential.
    match token_source {
        Some(src) => {
            let _ = writeln!(
                out,
                "{p}: ok — GitHub token from {src} (used for index/pkg fetches; never printed)"
            );
        }
        None => {
            let _ = writeln!(
                out,
                "{p}: ok — no GitHub token in use (anonymous API: fine for public repos, \
             rate-limited; `$ATPKG_TOKEN` is always honoured, and a repointed account or a \
             private `[packages.links]` override also consults the app updater's chain — \
             `$ATERM_UPDATE_TOKEN`, keychain, 0600 file, `$GITHUB_TOKEN`/`$GH_TOKEN`, \
             `gh auth token`)"
            );
        }
    }

    // (2) PREFIX / STORE.
    if layout.prefix.is_dir() {
        let _ = writeln!(out, "{p}: ok — prefix {}", layout.prefix.display());
    } else {
        let _ = writeln!(
            out,
            "{p}: warn — prefix {} does not exist yet (nothing installed)",
            layout.prefix.display()
        );
    }

    // (3) PATH WIRING.
    let bin_dir = layout.bin_dir();
    let on_path = path_var
        .map(|p| std::env::split_paths(p).any(|d| d == bin_dir))
        .unwrap_or(false);
    if on_path {
        let _ = writeln!(out, "{p}: ok — managed bin/ is on PATH");
    } else {
        // The bin path used to appear TWICE on this line — once as the subject, once
        // inside the export — doubling the longest token in the whole report. The
        // copy-pasteable export is the copy that earns its bytes; "managed bin/" matches
        // the ok-branch's name for the same thing.
        let _ = writeln!(
            out,
            "{p}: warn — managed bin/ is not on PATH here; an aterm shell — and any rc \
             file carrying atpkg's block (the rc lines below) — sources \
             ~/.aterm/shell.d (which APPENDS it), or add: {}",
            manual_path_hint(&bin_dir)
        );
    }

    // (3b) REROUTE (philosophy §4, `crate::reroute`): per row, the stub's state; then
    // ORDER, not presence. The failure this exists to name was an order — measured
    // 2026-09-07, `~/.cargo/bin` sat at PATH position 17, ahead of everything managed,
    // and a bare `cargo` in a session ran upstream Rust silently. Everything here is
    // advisory (warn, exit 0): outside an aterm session the reroute dir is not on PATH
    // BY DESIGN (the prepend is session-scoped, never machine-wide), and a missing stub
    // is re-laid at the next spawn. Doctor names; it never lays.
    let reroute_dir = layout.reroute_dir();
    for (name, state) in crate::reroute::states(layout) {
        match state {
            crate::reroute::StubState::Laid => {
                let _ = writeln!(out, "{p}: ok — reroute stub {name} laid");
            }
            crate::reroute::StubState::Missing if cfg!(windows) => {
                let _ = writeln!(
                    out,
                    "{p}: note — reroute stub {name} not laid: Windows lays no reroute stubs yet (TARGET)"
                );
            }
            crate::reroute::StubState::Missing => {
                let _ = writeln!(
                    out,
                    "{p}: warn — reroute stub {name} missing (an aterm session lays it at \
                     spawn; `aterm pkg repair` re-lays it; a recorded decline lays none)"
                );
            }
            crate::reroute::StubState::Foreign => {
                let _ = writeln!(
                    out,
                    "{p}: warn — {} is not ours (foreign file; never touched)",
                    crate::reroute::stub_path(layout, name).display()
                );
            }
        }
    }
    let path_entries: Vec<std::path::PathBuf> = path_var
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let reroute_at = path_entries.iter().position(|d| *d == reroute_dir);
    let upstream_at = upstream_index_on_path(&path_entries, &layout.prefix, "cargo");
    match (reroute_at, upstream_at) {
        (None, _) => {
            let _ = writeln!(
                out,
                "{p}: note — reroute dir not on this PATH (expected outside an aterm session)"
            );
        }
        (Some(i), Some(j)) if i < j => {
            let _ = writeln!(
                out,
                "{p}: ok — reroute dir precedes upstream cargo (PATH[{i}] < PATH[{j}])"
            );
        }
        (Some(i), Some(j)) => {
            let _ = writeln!(
                out,
                "{p}: warn — upstream cargo at PATH[{j}] precedes the reroute dir at PATH[{i}]; \
                 inside an aterm session the spawn seam and shell integration put it first"
            );
        }
        (Some(i), None) => {
            let _ = writeln!(
                out,
                "{p}: ok — reroute dir on PATH (PATH[{i}]); no upstream cargo on this PATH"
            );
        }
    }

    // (4) BROKEN SHIM SCAN of bin/ — a shim whose forward target is GONE (a dangling
    // symlink on Unix; on Windows a `.cmd` forwarding to a missing exe, which no symlink
    // scan could ever catch). `resolve_shim` reads the target cross-platform; a tombstone
    // (deliberately target-less) yields `None` and is never flagged.
    //
    // A SHIM WE COULD NOT STAT IS NOT A BROKEN SHIM. This asked `Path::exists()`, which
    // is `fs::metadata(..).is_ok()` and so answers `false` for a target that is there
    // and merely unreadable — EACCES on a parent directory, macOS privacy consent's
    // EPERM, EIO. Every one of those printed FAIL and pushed the exit code non-zero
    // over a shim that runs, and `tools/install.sh` hands that exit code to every
    // failed-seed installer as THE diagnostic to trust. The implied remedy, reinstall,
    // repairs nothing — the same expensive wrong the speaker line at (0) exists to make
    // verifiable.
    //
    // The blindness is not swallowed either: silently passing would make the check
    // claim a health it did not observe, which is the same defect pointed the other
    // way. It is REPORTED, as a warn that names the cause — and a warn, not a fault,
    // because nothing here is evidence of one.
    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for e in entries.flatten() {
            let shim = e.path();
            let Some(target) = crate::platform::resolve_shim(&shim) else {
                continue;
            };
            match crate::store::presence(&target) {
                crate::store::Presence::Present => {}
                crate::store::Presence::Absent => {
                    fails += 1;
                    let _ = writeln!(err, "{p}: FAIL — broken bin shim {}", shim.display());
                }
                crate::store::Presence::Unknown(why) => {
                    let _ = writeln!(
                        out,
                        "{p}: warn — bin shim {} could not be checked ({why}); its target                          {} is unreadable by this user, which is a permission to look, not                          a broken shim",
                        shim.display(),
                        target.display()
                    );
                }
            }
        }
    }

    // (5) ACTIVE-BUILD STORE INTEGRITY.
    let active = crate::ops::active_builds(layout);
    // The first program whose store tree is missing/incomplete — remembered so the
    // verdict tail can name ONE structural repair (`install <program>`) instead of a menu.
    let mut next_install_program: Option<String> = None;
    for (program, build) in &active {
        let bd = layout.build_dir(program, *build);
        if !bd.is_dir() || !crate::store::build_is_complete(&bd) {
            fails += 1;
            let _ = writeln!(
                err,
                "{p}: FAIL — active {program} build {build} store missing/incomplete"
            );
            if next_install_program.is_none() {
                next_install_program = Some(program.clone());
            }
        }
    }
    let _ = writeln!(out, "{p}: ok — {} program(s) active", active.len());

    // (5a) THE INSTALLER ITSELF, when it is a browser download. A quarantined app is
    // provenance-tracked in EVERY invocation — a job launchd spawns from it included,
    // which is the escape the untracked lane leans on — so its installs go through the
    // lane's copy plan and its own executable is the carrier. Named here because every
    // surface said "a probe file it wrote came back tagged" and none said why
    // (2026-09-14). A note, not a warning: the lane handles it.
    if let Some(carrier) = std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::canonicalize(exe).ok())
        .and_then(|exe| crate::provenance::quarantined_carrier(&exe))
    {
        let _ = writeln!(
            out,
            "{p}: note — {} carries com.apple.quarantine (a browser download): every \
             process this app starts is provenance-tracked, so its installs and shims go \
             through the untracked launchd lane; an app laid down by aterm's own updater \
             carries no such tag",
            carrier.display()
        );
    }

    // (5b) PROVENANCE-TAGGED EXECUTABLES AND SHIMS (macOS).
    //
    // macOS stamps `com.apple.provenance` on every file a provenance-tracked process
    // writes, and a process is tracked when its executable carries the tag or its parent
    // does (measured 2026-09-12 — `crate::provenance`). So a bundle seeded from a tracked
    // shell — an agent started from a tagged `claude`, a shell inside a tracked aterm.app —
    // carries the tag on `bin/trustc`, every object file and proof snapshot a release cut
    // builds with it inherits the tag, and tools/proof_snapshot.py refuses them AFTER the
    // ledger claim: v0.83.0 burned a build number on `trust/8590`. The shims in the managed
    // `bin/` are checked too: a shim is a `#!/bin/sh` script the kernel execs, and a tagged
    // one tracks the tool it forwards to even when the bundle behind it is clean.
    //
    // A WARNING, never a PROBLEM: every tool still runs — only a release cut cannot use it
    // — and doctor does not measure whether THIS process is tracked (that takes a probe
    // write, and doctor never mutates); it reports what is on disk and names the cure.
    if cfg!(target_os = "macos") {
        for (program, build) in &active {
            let build_dir = layout.build_dir(program, *build);
            let bin = build_dir.join("bin");
            // `bin/` and `lib/` both: a tagged dylib tracks the process that loads it
            // (measured 2026-09-15 — `trustc` loads the bundle's `libstd`/`librustc_driver`),
            // so a clean `bin/` over a tagged `lib/` is a tracked compiler all the same.
            let mut scan = crate::provenance::tagged_files_in(&bin, probes.provenance_attr);
            let lib = crate::provenance::tagged_files_under(
                &build_dir.join("lib"),
                probes.provenance_attr,
            );
            scan.total += lib.total;
            scan.carriers.extend(lib.carriers);
            // The CAUSE, when the store recorded one: a tracked installer that staged
            // this build in-process because its untracked lane could not run wrote
            // `<build>.tracked-install` beside it, and the line names it rather than
            // leaving the operator to guess which shell seeded what.
            let recorded = crate::store::tracked_install_record(&build_dir);
            if scan.carriers.is_empty() {
                if let Some(why) = &recorded {
                    let _ = writeln!(
                        out,
                        "{p}: {}",
                        provenance_stale_record_line(program, *build, &bin, why)
                    );
                }
                continue;
            }
            let _ = writeln!(
                out,
                "{p}: {}",
                provenance_bundle_line(program, *build, &bin, &scan, recorded.as_deref())
            );
        }
        // Every directory of executables atpkg lays, not `bin/` alone: the `agents/`
        // twin is what actually runs `claude`/`codex` (first on every PATH) and is laid
        // by its own lane job, and the reroute stubs run every upstream `cargo`/`rustc`
        // typed in a session — a tagged file in either tracks the tool exactly as a
        // tagged `bin/` shim does, and until 2026-09-14 neither was looked at.
        for dir in [
            layout.bin_dir(),
            layout.agents_dir(),
            crate::reroute::dir(layout),
        ] {
            let shims = crate::provenance::tagged_files_in(&dir, probes.provenance_attr);
            if !shims.carriers.is_empty() {
                let _ = writeln!(out, "{p}: {}", provenance_shim_line(&dir, &shims));
            }
        }
    }

    // (5c) SOLVERS THE `trust` BUNDLE PINS PRIVATELY.
    //
    // trustc resolves its SMT solver as a SIBLING of its own executable — see
    // `sibling_solver_candidate` / `resolve_ay_solver_identity_from_candidates`
    // in trust's `compiler/rustc_mir_transform/src/trust_verify.rs`: an explicit
    // `AY_PATH` wins, otherwise the copy inside the bundle's own `bin/`, and
    // there is NO PATH fallback at all.
    //
    // The pin itself is deliberate and should not be "fixed" away: trust
    // snapshots and fingerprints the solver binary so a proof records which
    // solver produced it, and a solver that floated with whatever atpkg last
    // installed would make proof identity float with it.
    //
    // What was wrong is that NOTHING said so. atpkg dutifully kept `ay` current
    // while every Trust build went on using a copy three minor versions behind,
    // and the only way to find out was to compare two `--version` strings
    // nobody had a reason to compare. Measured 2026-08-20 on a clean v0.44.0
    // install: bundle `ay 0.10.0`, `ty 0.12.0`, `clean 0.1.0` against managed
    // `0.13.0`, `0.13.0`, `0.2.0`.
    //
    // So: reported, never failed. A pin by design is not a problem; a pin
    // nobody can see is.
    if let Some(trust_build) = active
        .iter()
        .find(|(p, _)| p.as_str() == "trust")
        .map(|(_, b)| *b)
    {
        let bundle_bin = layout.build_dir("trust", trust_build).join("bin");
        for (program, managed_build) in &active {
            if program.as_str() == "trust" {
                continue;
            }
            let pinned = bundle_bin.join(program);
            if !pinned.is_file() {
                continue;
            }
            let pinned_version = probe_version(&pinned);
            let managed_version = probe_version(
                &layout
                    .build_dir(program, *managed_build)
                    .join("bin")
                    .join(program),
            );
            // Equal versions are the healthy case. An unanswered probe on
            // either side is NOT evidence of divergence, so it stays silent
            // rather than reporting "pinned unknown vs managed 0.13.0" — a
            // line that reads like a finding and carries none.
            if pinned_version == managed_version
                || pinned_version == "unknown"
                || managed_version == "unknown"
            {
                continue;
            }
            let override_hint = if program.as_str() == "ay" {
                " (override: AY_PATH)"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "{p}: note — Trust builds use the {program} pinned inside the trust bundle \
                 ({pinned_version}), not the managed {program} {managed_version} \
                 (build {managed_build}){override_hint}"
            );
        }
    }

    // (5f) tippy's compiler is a plain file, and the rustup view is a clone of the build.
    //
    // History, because the check that stood here until 2026-09-14 was about a file
    // that no longer ships. The bundle carried `rustc` as a second COPY of `trustc` for
    // rustup's sake, tippy preferred that copy and demanded it be byte-identical to
    // `trustc`, and macOS makes that impossible: an ad-hoc signature bakes the file's
    // own name into itself (measured on 8571/8589/8595 — 2,428 differing bytes, every
    // one inside the signature blob). So this report compared the two modulo their
    // signatures to say whether tippy would refuse. Trust now ships its tools under
    // Trust's names only, tippy runs `trustc` directly, and the stock names live in
    // atpkg's rustup view ([`crate::seam::refresh_view`]) as copy-on-write clones of the
    // Trust tools — so a bundle published from 2026-09-14 on has no two files to compare.
    //
    // The bundles already INSTALLED do. 8571, 8589, 8590 and 8595 still ship `bin/rustc`
    // as a separately signed copy, their tippy (before trust `e79c1142a5`) still refuses
    // it — and that removed comparison was then the only thing that could see it, so
    // doctor said "healthy" over a PATH tippy that stopped at a setup error on every run
    // (measured on 8595, 2026-09-15). The store is never modified; atpkg lays a per-build
    // exec root — a copy-on-write clone of the build where `rustc` holds `trustc`'s bytes —
    // and routes the trust shims through it ([`crate::compat`]). What this check says is
    // file facts read with `lstat` and byte compares, never a run of tippy, whose
    // ancestor-directory checks refuse intermittently and would make the verdict flap.
    //
    // (a) `trustc` must be a plain file: tippy (and tippy-driver) refuse a symbolic
    //     link for the selected compiler, measured. It withholds "healthy" while tippy
    //     is in the bundle to be refused.
    // (b) For every recorded seam, each stock name rustup resolves must be a clone of the
    //     store's Trust tool holding its bytes — or `cargo +trust` / `rustc +trust` run
    //     something other than the managed toolchain. A view laid as hard links before
    //     clones is not one. A mismatch is a warn that withholds "healthy", and
    //     `aterm pkg repair` rebuilds the view.
    // (c) When the active build [`crate::compat::needs_root`] (`rustc` neither `trustc`'s
    //     inode nor its bytes, beside a tippy): an exec root that is a clone of the build
    //     at `Deep` depth, with every trust shim in `bin/` carrying the guard through it,
    //     is a NOTE and the report stays healthy. No root, a root that differs from the
    //     build, or a shim that does not route is a warn that withholds "healthy" — both
    //     inode numbers in the line, so a reader can `stat` the claim — with the one fix,
    //     `aterm pkg repair`, which lays the root at `Deep` and re-renders every shim.
    //     Behind a link or a file at `compat` or `compat/trust` repair refuses on every
    //     run, so that warn names the path and says to remove it first.
    // (d) A root under `<prefix>/compat/trust` no build needs — its build reclaimed (its
    //     clones keep the reclaimed blocks allocated), its build needing none, or not a
    //     directory at all — is an advisory warn naming `aterm pkg gc`, whose closing
    //     sweep removes exactly these ([`crate::compat::strays`] is that sweep's own
    //     classification). It leaves a healthy report healthy: no tool fails over it.
    // (e) A build that needs no root — every Trust-names-only bundle, a pack whose `rustc`
    //     is a hard link of `trustc`, a byte-identical copy — gets no (c) line at all.
    // (f) A build whose `bin/rustc` or `bin/trustc` cannot be READ is not proof of either:
    //     a warn that withholds "healthy", naming the failed read, because nothing here can
    //     vouch for PATH tippy — and the passes leave any root standing for it alone.
    //
    // A warn that says "cannot run" counts in `tools_cannot_run`; one that says "may not
    // run" or "cannot tell" counts in `tools_unproven`, so the closing line never claims a
    // tool cannot run where the line it summarises only failed to show that it can (a
    // reviewer read "cannot run" under the (f) line while PATH tippy ran from its root,
    // 2026-09-15).
    let mut tools_cannot_run = 0usize;
    let mut tools_unproven = 0usize;
    if let Some(trust_build) = active.get("trust").copied() {
        let bin = layout.build_dir("trust", trust_build).join("bin");
        let trustc = bin.join("trustc");
        let refused = match std::fs::symlink_metadata(&trustc) {
            Ok(m) if m.file_type().is_symlink() => Some("a symbolic link"),
            Ok(m) if !m.is_file() => Some("not a regular file"),
            // Absent: the bundle is missing its compiler, which the shim and
            // integrity checks above already name; nothing tippy-specific to add.
            _ => None,
        };
        if let Some(what) = refused
            && bin.join("tippy").is_file()
        {
            tools_cannot_run += 1;
            let _ = writeln!(
                out,
                "{p}: warn — trust build {trust_build}: tippy cannot run — {} is {what}, and \
                 tippy requires the selected compiler trustc to be a plain file, so every tippy \
                 run stops at a setup error (the bundle's fix: {TRUST_COMPILER_FIX})",
                trustc.display()
            );
        }
        view_stock_names_check(out, p, layout, Some(bin.clone()), &mut tools_cannot_run);
        match crate::compat::inspect(layout, trust_build) {
            Some(Ok(inspection)) => {
                let (line, claim) = exec_root_line(p, trust_build, &inspection);
                match claim {
                    ToolClaim::Runs => {}
                    ToolClaim::CannotRun => tools_cannot_run += 1,
                    ToolClaim::Unproven => tools_unproven += 1,
                }
                let _ = writeln!(out, "{line}");
            }
            Some(Err(why)) => {
                tools_unproven += 1;
                let _ = writeln!(out, "{}", exec_root_unread_line(p, trust_build, &why));
            }
            None => {}
        }
    }
    // A dev-linked trust still has a seam when no shim in `bin/` names a store build: under
    // a link every trust shim resolves into the checkout, so `active` has no trust build
    // even when the store holds one. Its view is checked too — against the checkout, or
    // against the store's `current` build under a dev-link the seam refused.
    if !active.contains_key("trust") {
        let current = crate::seam::store_current(layout);
        let store_bin = std::fs::read_link(&current)
            .ok()
            .map(|raw| {
                if raw.is_absolute() {
                    raw
                } else {
                    current.parent().map_or(raw.clone(), |d| d.join(raw))
                }
            })
            .map(|build| build.join("bin"))
            .filter(|bin| bin.is_dir());
        view_stock_names_check(out, p, layout, store_bin, &mut tools_cannot_run);
    }
    for (path, why) in crate::compat::strays(layout) {
        let _ = writeln!(out, "{}", stray_root_line(p, &path, why));
    }

    // (5b) LIVE-BUILD WITNESS. `gc` reclaims a program's superseded builds only when the
    // authoritative `store/<program>/current` symlink and the derived `bin/` shim view name
    // the SAME build; where they don't it abstains rather than guess (guessing is what
    // deleted live trees). Abstention is silent by nature — the program simply accumulates
    // builds forever — so this is the surface that makes it visible. A genuine disagreement
    // is STRUCTURAL: whichever view is stale, some tool on PATH is running a build activation
    // does not select. A merely-absent witness is not breakage, so it warns.
    // (5d) THE WORKSPACE THE OPERATOR IS STANDING IN vs THE INSTALLED TRUST.
    //
    // atpkg keeps the toolchain current with what is PUBLISHED; it cannot keep a
    // repository from depending on a Trust build that was never published. When that
    // happens the symptom is targo refusing the workspace with an "unknown field"
    // error that looks like a manifest typo. The installed targo decides the verdict
    // here, and the line says what is actually behind: the toolchain, on the
    // publisher's side, not this machine.
    let mut next_publish = false;
    if let Some(ws) = &probes.workspace {
        let trust_build = active
            .iter()
            .find(|(p, _)| p.as_str() == "trust")
            .map(|(_, b)| b.to_string())
            .unwrap_or_else(|| "?".to_string());
        match &ws.policy {
            WorkspacePolicy::Accepted => {
                let _ = writeln!(
                    out,
                    "{p}: ok — workspace {} declares a [trust] policy the installed trust \
                     build {trust_build} accepts",
                    ws.workspace.display()
                );
            }
            WorkspacePolicy::UnknownField { field } => {
                fails += 1;
                next_publish = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — workspace {} declares a [trust] policy key the installed \
                     trust build {trust_build} does not know: `{field}`. Whoever committed \
                     it built on a NEWER Trust seal that atpkg has not published, so this \
                     workspace does not open with targo on any atpkg-managed machine. Cure \
                     (a rostered publisher, on the machine holding that seal): \
                     {PUBLISH_RUSTC_GROUP}; then here: aterm pkg update trust",
                    ws.workspace.display()
                );
            }
            WorkspacePolicy::ProbeFailed { why } => {
                let _ = writeln!(
                    out,
                    "{p}: warn — could not ask the installed targo about workspace {} \
                     ({why})",
                    ws.workspace.display()
                );
            }
        }
    }

    // (5e) A LOCAL SEAL BEHIND rustup's `trust` CHANNEL. Correct on a publisher
    // machine; the warning exists so that machine can see the seal is still local
    // only — the other half of (5d), seen from the side that causes it.
    if let Some(seal) = &probes.local_seal {
        let _ = writeln!(
            out,
            "{p}: warn — rustup's trust channel resolves to a LOCAL toolchain, not the \
             atpkg store: {} (trustc {}). Commits made against features only it has \
             will not build on atpkg-managed machines until that seal is published \
             ({PUBLISH_RUSTC_GROUP})",
            seal.link_target.display(),
            seal.trustc
        );
    }

    let live = crate::gc::live_builds(layout);
    // A shim/channel divergence's repair is a re-run of `update` — remembered for the
    // verdict tail's single `next` act.
    let mut next_update_divergence = false;
    for d in live.diverged() {
        match &d.reason {
            crate::gc::Diverged::ChannelShimMismatch {
                channel_says,
                shims_say,
            } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: the channel selects build {channel_says} but its bin/ \
                     shims run build {shims_say} (re-run `aterm pkg update {}`: it installs \
                     the pinned build, or re-points the links when the shims already run it)",
                    d.program, d.program
                );
            }
            crate::gc::Diverged::ShimsDisagree { builds } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: its bin/ shims are split across builds {} — one \
                     program's tools must all point into one build (run `aterm pkg repair` \
                     to re-lay every shim from one build, then `aterm pkg update {}` to \
                     re-point the links)",
                    d.program,
                    build_list(builds),
                    d.program
                );
            }
            crate::gc::Diverged::ChannelsDisagree { builds } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: two channel `current` links select different builds \
                     {} and it has no `store/{}/current` of its own to break the tie — run \
                     `aterm pkg update {}` to write one",
                    d.program,
                    build_list(builds),
                    d.program,
                    d.program
                );
            }
            crate::gc::Diverged::NoLiveWitness { shims_say } => {
                let _ = writeln!(
                    out,
                    "{p}: warn — {}: build {shims_say} is on PATH but no `current` link \
                     selects it, so gc keeps every superseded {} build. Run \
                     `aterm pkg update {}` to write the link and clear this.",
                    d.program, d.program, d.program
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "{p}: ok — {} program(s) with a proven live build",
        live.len()
    );

    // (6) SHELL HOOKS + FISH-SAFETY.
    if let Some(home) = home {
        let aterm = home.join(".aterm");
        let shell_d = aterm.join("shell.d");
        // Probe the dialect the interactive shell on THIS platform actually sources: `.ps1`
        // (PowerShell) on Windows, `.zsh` elsewhere. An install writes the whole set, so a
        // present platform-native hook means PATH wiring is in place.
        let native_hook = format!("{}.{}", crate::hooks::HOOK_BASENAME, native_hook_ext());
        if shell_d.join(&native_hook).is_file() {
            let _ = writeln!(out, "{p}: ok — shell.d hooks present");
        } else {
            let _ = writeln!(
                out,
                "{p}: warn — shell.d hooks not generated yet (an install writes them)"
            );
        }
        if let Ok(entries) = std::fs::read_dir(&shell_d) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().ends_with(".sh") {
                    fails += 1;
                    let _ = writeln!(
                        err,
                        "{p}: FAIL — shell.d/{}: a POSIX .sh breaks fish — remove it",
                        e.file_name().to_string_lossy()
                    );
                }
            }
        }
        // (6b) rc wiring — which shells outside aterm reach the managed `bin/`, and for the
        // ones that do not, why. One line per rc file that exists (atpkg never creates one).
        // Without it the three states a user can be in — wired, opted out, and skipped at
        // the consent fence — are indistinguishable from outside.
        //
        // Doctor must not invent a fault from evidence it did not measure. `rc_wiring` reads
        // each file's content for a `/.aterm/shell.d/` line, and also the two bash login
        // profiles install.sh can elect that atpkg has no row for
        // (`hooks::INSTALL_SH_PROFILES`), so a Mac wired by install.sh is not called
        // unwired. It still reads six named files, not every file a shell can start from, so
        // when none of them sources shell.d the note names the six (`hooks::rc_files_read`)
        // instead of calling the machine aterm-only.
        #[cfg(unix)]
        {
            let wiring = crate::hooks::rc_wiring(home);
            let names = |want: crate::hooks::RcState| -> Vec<String> {
                wiring
                    .iter()
                    .filter(|(_, s)| *s == want)
                    .map(|(rc, _)| format!("~/{rc}"))
                    .collect()
            };
            let wired = names(crate::hooks::RcState::Wired);
            let elsewhere = names(crate::hooks::RcState::SourcedElsewhere);
            if !wired.is_empty() {
                let _ = writeln!(
                    out,
                    "{p}: ok — {} sources ~/.aterm/shell.d (atpkg's marker block; delete \
                     the block to opt out — it stays deleted until `aterm pkg repair`)",
                    wired.join(", ")
                );
            }
            if !elsewhere.is_empty() {
                let _ = writeln!(
                    out,
                    "{p}: ok — {} sources ~/.aterm/shell.d via a line that is not atpkg's \
                     block (tools/install.sh's, or your own)",
                    elsewhere.join(", ")
                );
            }
            for (rc, state) in &wiring {
                match state {
                    crate::hooks::RcState::Wired | crate::hooks::RcState::SourcedElsewhere => {}
                    crate::hooks::RcState::OptedOut => {
                        let _ = writeln!(
                            out,
                            "{p}: note — ~/{rc}: atpkg's block was deleted — opted out, \
                             left alone by every install and update pass (`aterm pkg \
                             repair` lays it again)"
                        );
                    }
                    crate::hooks::RcState::ConsentFenced => {
                        let _ = writeln!(
                            out,
                            "{p}: note — ~/{rc} resolves under a folder macOS guards with \
                             a consent dialog, so no atpkg pass opens it — it is left \
                             unwired; source ~/.aterm/shell.d from it yourself, or reach \
                             the tools with `aterm <tool>`"
                        );
                    }
                    crate::hooks::RcState::Unwired => {
                        let _ = writeln!(
                            out,
                            "{p}: warn — ~/{rc} does not source ~/.aterm/shell.d (an \
                             install pass or `aterm pkg repair` appends atpkg's block)"
                        );
                    }
                }
            }
            if wired.is_empty() && elsewhere.is_empty() {
                let checked: Vec<String> = crate::hooks::rc_files_read()
                    .map(|rc| format!("~/{rc}"))
                    .collect();
                let _ = writeln!(
                    out,
                    "{p}: note — none of {} sources ~/.aterm/shell.d, so a shell outside \
                     aterm that starts from only those files does not reach the managed \
                     tools; use `aterm <tool>` there, or: {}",
                    checked.join(", "),
                    manual_path_hint(&bin_dir)
                );
            }
        }
        // Privacy of the login-sourced dirs (READ-ONLY — doctor never chmods).
        for dir in [&aterm, &shell_d] {
            if let Ok(m) = std::fs::symlink_metadata(dir)
                && m.file_type().is_dir()
                && !crate::platform::dir_meta_is_private(&m)
            {
                fails += 1;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {} is group/other-writable (login shells source it)",
                    dir.display()
                );
            }
        }
    }

    // (7) DISK HEADROOM.
    match crate::freespace::available_bytes(&layout.prefix) {
        Some(free) if free < 5 * GIB => {
            let _ = writeln!(
                out,
                "{p}: warn — only {} free (a toolchain update needs ~2.5x its artifact size)",
                crate::cost::human_bytes(free)
            );
        }
        Some(free) => {
            let _ = writeln!(out, "{p}: ok — {} free", crate::cost::human_bytes(free));
        }
        None => {
            let _ = writeln!(out, "{p}: warn — could not query free space");
        }
    }

    // (7b) SPOTLIGHT EXPOSURE OF RUST BUILD OUTPUT.
    //
    // On 2026-09-01 01:54 WindowServer's main thread blocked 40 s in a synchronous TCC
    // preflight waiting on tccd; tccd was TH_UNINT on an APFS volume rwlock contended by
    // 18 rustc processes, syspolicyd and `mds` grinding 2.0 TB of Rust build output across
    // 127 target dirs. The watchdog killed WindowServer and the GUI session died.
    // Spotlight indexing of build output is one of the two amplifiers that made a busy
    // build fatal, and it is the one a per-directory rename removes.
    //
    // A WARNING, never a PROBLEM: nothing here is structurally broken, the scan is a
    // NAME-based heuristic over a depth-limited walk, and doctor must not invent a fault
    // from evidence it did not measure (`is_problem_state`'s allow-by-prefix rule, above).
    // And doctor NEVER probes: measuring exclusion means writing a probe file into the
    // user's tree, and doctor does not mutate. It reports the exposure and names the verb
    // that measures it.
    //
    // The depth under $HOME is the zero-config compromise, and it is
    // `noindex::DOCTOR_DEPTH`'s to state — five since 2026-09-15, because three reached
    // `~/<repo>/target` but not a cargo workspace's `~/<repo>/crates/<member>/target`, and
    // this very machine had four such dirs (400 MiB) that every surface called clean. Read
    // that constant's note for the measurement. Dot-directories are pruned, and that is
    // sound rather than merely cheap: their whole subtree is already excluded (measured
    // 2026-09-02). So are the six folders macOS guards with a consent dialog and `Library`
    // (`noindex::SKIP_DIRS`) — a walk that raised aterm's name on a privacy modal at the
    // top of an unattended pass is the one cost this scan may never impose, which is why
    // the line below says which places were not walked. Anything deeper than the constant
    // is the deliberate `aterm pkg noindex scan <root>`, which the line below names.
    //
    // No `cfg` here by design: `noindex::scan` returns an empty, `complete` scan on
    // non-macOS, so the whole block prints nothing there — the same discipline as
    // `store.rs`'s unconditional `platform::exclude_from_backup`.
    if let Some(home) = home {
        let found = crate::noindex::scan(
            home,
            crate::noindex::DOCTOR_DEPTH,
            &crate::noindex::Budget::DOCTOR,
        );
        let exposed: Vec<&crate::noindex::Target> = found.exposed().collect();
        if exposed.is_empty() && !found.targets.is_empty() {
            // The provenance caveat, carried by EVERY other surface that prints this claim
            // (`noindex scan`'s `hidden` legend, the manual): `exclusion_of` reads the NAME,
            // and `.noindex` is behaviour observed on this machine, not a documented Apple
            // API. A bare "ok — 127 hidden" would be this feature's own version of the 189
            // inert `.metadata_never_index` markers the day macOS changes that behaviour —
            // and doctor, which never probes, is the last surface that should sound sure.
            let _ = writeln!(
                out,
                "{p}: ok — {} cargo target dir(s) under {} are hidden from Spotlight — that \
                 is what the NAMES say; `aterm pkg noindex verify <dir>` measures it",
                found.targets.len(),
                home.display()
            );
        } else if !exposed.is_empty() {
            let size = crate::noindex::size_of_all(&exposed, &crate::noindex::Budget::DOCTOR_SIZE);
            // WHICH of them the unattended pass can reach. The pass renames only a
            // target dir beside a Cargo.toml (`noindex::apply_under`, `require_repo`): a
            // FREE-STANDING one — `$HOME/trust/build/bootstrap`, `$HOME/trust-verify-scratch/
            // ay-check` — is pointed at by an env var or a build system the pass cannot
            // re-point, so it is skipped on every pass, silently, and this line counted
            // it as "the next update pass migrates" forever. The 2026-09-13 reading of an
            // "8 of 44" that had not moved in a day was exactly that: all eight
            // free-standing. Say which are which, and what moves the free ones — a count
            // that never falls otherwise reads as a pass that is not doing its job.
            let (in_repo, free): (Vec<&crate::noindex::Target>, Vec<&crate::noindex::Target>) =
                exposed
                    .iter()
                    .copied()
                    .partition(|t| crate::noindex::repo_of(&t.path).is_some());
            let reach = spotlight_reach(in_repo.len(), &free);
            let _ = writeln!(
                out,
                "{p}: warn — {} of {} cargo target dir(s) under {} are indexed by Spotlight \
                 ({}) — `mds` grinding build output was one of the two amplifiers behind the \
                 2026-09-01 WindowServer watchdog kill; {reach} — a git checkout keeps a \
                 `target` symlink and its .cargo/config.toml untouched, the new name (and \
                 the link, when the ignore entry is directory-only) excluded via \
                 .git/info/exclude ([machine] spotlight_noindex, default on){now}; `aterm pkg \
                 noindex` lists them, `aterm pkg noindex verify <dir>` measures one",
                exposed.len(),
                found.targets.len(),
                home.display(),
                crate::noindex::human_size(&size),
                // THE REMEDY ONLY WHEN IT REACHES SOMETHING. `--all` walks with
                // `require_repo`, so in the all-free case the sentence above has just
                // finished explaining that it skips every one of these — and offering it
                // anyway is a guaranteed no-op that leaves the warning standing, which
                // reads as a tool that does not work.
                now = if in_repo.is_empty() {
                    String::new()
                } else {
                    " — now: `aterm pkg machine apply` (or `aterm pkg noindex apply --all`, \
                     which walks with the verb's larger budget)"
                        .to_string()
                }
            );
        }
        if !found.complete {
            let _ = writeln!(
                out,
                "{p}: note — the Spotlight-exposure scan did not cover all of {}; \
                 `aterm pkg noindex scan <root>` covers a tree it did not reach",
                home.display()
            );
        }
    }

    // (7b) UNIVERSAL CONTROL (macOS): the other machine setting the pass applies per
    // this report (R5). Read through `defaults -currentHost`, never written here; the
    // line carries the revert when it is off and the opt-out when it is about to be.
    if cfg!(target_os = "macos") {
        let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
        let policy = crate::config::cached_machine().universal_control();
        let _ = writeln!(out, "{p}: {}", state.doctor_line(policy));
    }

    // (8) INDEX FREEZE / AGE (no unverified parse — atpkg's OWN diagnostics only).
    // The clock is `last_success_at`, stamped by a pass that resolved the index and RAN
    // TO ITS END — clean, or with member failures each recorded in its own row (the
    // update lane stamps both exits since 2026-09-14, so one failing member cannot keep
    // every read-only verb saying "no update check has run yet"). It is never
    // `updated_at`, which every writer moves: a machine whose passes all failed for a
    // month used to read "0 day(s) since the last successful update" off the failure
    // row's own timestamp (2026-09-10 audit). The line therefore says "completed pass",
    // not "successful update": on an Intel Mac whose every pass ended in a member
    // failure it read "0 day(s) since the last successful update" (2026-09-15), which
    // is not what the stamp measures; the member rows below say what failed.
    if let Some(status) = crate::status::read(layout) {
        if status.last_success_at.trim().is_empty() {
            let _ = writeln!(
                out,
                "{p}: warn — no successful update pass recorded yet (status last written {}) — \
                 packages cannot be updated until the first pass completes (run: aterm pkg \
                 update)",
                if status.updated_at.is_empty() {
                    "never"
                } else {
                    status.updated_at.as_str()
                }
            );
        } else {
            match index_age_days(&status.last_success_at, now) {
                Some(days) if days > 30 => {
                    let _ = writeln!(
                        out,
                        "{p}: warn — {days} day(s) since the last completed update pass ({}) — \
                         this machine has been off, or the app has not run",
                        status.last_success_at
                    );
                }
                Some(days) => {
                    let _ = writeln!(
                        out,
                        "{p}: ok — {days} day(s) since the last completed update pass"
                    );
                }
                None => {
                    let _ = writeln!(out, "{p}: warn — could not parse the last-success time");
                }
            }
            // THE TWO QUESTIONS THAT CLOCK COULD NOT ANSWER (2026-09-15): has the signed
            // index LISTING been reached lately (a pass served from the §14 cache is a
            // completed pass that learned nothing), and has the index itself MOVED
            // (the publisher's pulse). Both read the freshness fields the pass end
            // stamps from what the resolve measured; a record from before they existed
            // is silent here rather than accused.
            let reached_days = (!status.last_index_reached_at.is_empty())
                .then(|| index_age_days(&status.last_index_reached_at, now))
                .flatten();
            match reached_days {
                Some(days) if days > INDEX_UNREACHED_WARN_DAYS => {
                    let _ = writeln!(
                        out,
                        "{p}: warn — the signed index listing has not been reached for {days} \
                         day(s) (last: {}) — every pass since ran on the cached index and \
                         cannot see a newer pin: offline, rate-limited (the anonymous GitHub \
                         API shares ~60 requests/hour per address) or a proxy in the way; \
                         `aterm pkg update` prints the transport's reason",
                        status.last_index_reached_at
                    );
                }
                _ => {}
            }
            if !status.index_build_changed_at.is_empty()
                && let Some(days) = index_age_days(&status.index_build_changed_at, now)
                && days > INDEX_FROZEN_WARN_DAYS
                && reached_days.is_some_and(|d| d <= INDEX_UNREACHED_WARN_DAYS)
            {
                let _ = writeln!(
                    out,
                    "{p}: warn — index build {} has not changed in {days} day(s) while the \
                     listing was reachable — publishing looks frozen: the vendor lane's launchd \
                     job, its lock or its token upstream (a pinned claude stays at its version \
                     until the index moves)",
                    status.last_index_build
                );
            }
        }
    } else {
        let _ = writeln!(
            out,
            "{p}: warn — no status.toml yet (no update has run) — packages cannot be updated \
             until the first pass completes (run: aterm pkg update)"
        );
    }
    // The build floor is printed WITH the generation that recorded it, because that pair
    // is the actual gate: a floor stamped with an older generation is re-based by the next
    // master-signed one rather than obeyed (`sig::BuildFloor`), so a reader who saw only
    // the number could not tell a binding floor from an inherited one.
    let build_floor = crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    };
    let _ = writeln!(
        out,
        "{p}: last-trusted index_build {} (recorded under roster_seq {})",
        build_floor.index_build, build_floor.roster_seq
    );
    // The SECOND durable ratchet, shown beside the first because they answer different
    // questions and move independently: `index_build` is how far the toolchain index has
    // advanced, `roster_seq` is which generation of the machine roster this store has
    // accepted. A roster floor that is stuck while machines have been minted or revoked
    // means this store has not seen a publish since, which is worth being able to see.
    let _ = writeln!(
        out,
        "{p}: last-trusted roster_seq {}",
        crate::sig::Floor::new(layout.roster_floor()).current()
    );

    // (9) RUSTUP + RELOCATABILITY. Two questions, kept apart: is there a rustup on
    // this machine at all (PATH, then `$CARGO_HOME/bin`, then `~/.cargo/bin` — the
    // binary an app-spawned pass may not have on ITS PATH), and does it answer inside
    // the probe ceiling. On an Intel Mac under a self-update plus a seed pass (2026-09-15)
    // the 5 s `--version` probe missed a rustup 1.29.1 that answers in 79 ms idle, and
    // the report said "rustup not found" — false — and skipped the seam audit, which
    // needs no rustup binary at all.
    let rustup = rustup_binary(home);
    let rustup_answers = rustup.as_deref().is_some_and(rustup_present);
    if rustup.is_none() {
        let _ = writeln!(
            out,
            "{p}: warn — rustup not found on PATH, in $CARGO_HOME/bin or in ~/.cargo/bin \
             (self-contained bundles are portable)"
        );
    } else if !rustup_answers {
        let _ = writeln!(
            out,
            "{p}: warn — rustup at {} did not answer `--version` within {} s (a wedged shim, \
             or a toolchain fetch in flight); the `trust` seam is checked below without it",
            rustup
                .as_deref()
                .map_or_else(String::new, |r| r.display().to_string()),
            PROBE_TIMEOUT.as_secs()
        );
    }
    if let Some(rustup_home) =
        crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)
        && layout.program_current("trust").join("bin").is_dir()
        && let Some(line) = seam_line(
            &crate::seam::status(layout, &rustup_home, crate::seam::DEFAULT_SEAM),
            layout,
        )
    {
        // THE ENTRY EXISTS AND IS NOT OURS. `rustup which cargo --toolchain trust`
        // succeeds on m21 — the link resolves — so the check below called a
        // `~/.rustup/toolchains/trust -> $HOME/trust/build/host/stage2` (a from-source
        // dev build, Jul 19) healthy for seven weeks while the managed 6808 sat
        // unused by every `cargo +trust` (2026-09-10 audit). Doctor SAYS it and names
        // the one command; it re-points nothing — an entry under `~/.rustup` that
        // aterm did not lay is the user's, by the seam module's own rule.
        let _ = writeln!(out, "{p}: {line}");
    } else if rustup_answers
        && layout.program_current("trust").join("bin").is_dir()
        && !rustup_trust_channel_resolves(rustup.as_deref().unwrap_or(Path::new("rustup")))
    {
        // The managed Trust toolchain is HERE, but rustup does not know it, so
        // `cargo +trust` and a `rust-toolchain.toml` pinning `channel = "trust"`
        // both fail with `'rustc' is not installed for the custom toolchain
        // 'trust'` — the message that reads as a blocked machine and is not one.
        // Owner, 2026-09-08: the Trust toolchain is to be "very strongly
        // encouraged by the aterm system itself"; a store that is invisible to
        // the tool every Rust project resolves through is the opposite of that.
        // The fix is one link, and it names the store's `current` symlink so a
        // later `atpkg update` moves the channel with it.
        let _ = writeln!(
            out,
            "{p}: warn — rustup has NO `trust` channel, so `cargo +trust` and a project pinning \
             `channel = \"trust\"` fail here with `'rustc' is not installed for the custom \
             toolchain 'trust'` — NOT a blocked machine: `targo`/`trustc` in the managed bin \
             keep working. fix: rustup toolchain link trust {}",
            layout.program_current("trust").display()
        );
    }

    // (10) THE QUESTION A USER ACTUALLY CAME HERE WITH: do I have the toolchain?
    //
    // Everything above audits the STRUCTURE of the store — shims, floors, PATH, disk.
    // All of it passes vacuously on a machine that received nothing at all, so
    // `doctor` cheerfully printed "healthy" to the one person most in need of an
    // answer: someone whose toolchain never arrived, looking at the command the docs
    // point them to. A diagnostic that reports health over an empty store closes off
    // the only self-service path to understanding.
    let installed = crate::ops::active_builds(layout);
    let status = crate::status::read(layout);
    // A recorded key beginning with `-` cannot ever be a program: no shim can be created
    // under one and the signed index cannot name one. Such a row is a STRAY — the residue
    // of a mistyped `atpkg install --help`, which used to resolve the flag as a name and
    // persist the failure forever. Counting it as a missing member let one typo report a
    // fully healthy ten-program toolset as incomplete, and — because `tools/install.sh`
    // hands `pkg doctor` to every failed-seed installer as THE diagnostic — took the exit
    // code down with it, outside this repo (2026-08-20 round-10 audit).
    let strays: Vec<&String> = status
        .as_ref()
        .map(|s| s.programs.keys().filter(|k| k.starts_with('-')).collect())
        .unwrap_or_default();
    for stray in &strays {
        let _ = writeln!(
            out,
            "{p}: warn — the record holds a stray row for {stray:?}, which cannot be a \
             program name (it is a command-line flag, left by a mistyped `atpkg install`). \
             No program is missing because of it; the next successful `aterm pkg update` \
             clears it"
        );
    }
    // (10a) SYSTEM-SATISFIED MEMBERS. A member the signed index marks `system = "<bin>"`
    // that the pass found on PATH outside the prefix is deliberately NOT managed here —
    // its row says so in the canonical words (`system: <path> — not managed by aterm`),
    // and it is neither missing nor a fault. Re-check the recorded path: a binary that
    // has since gone away is the one state worth a line, because the next pass will
    // install the member through its artifact and a user may wonder why.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            let Some(path) = crate::state::system_path(&row.state) else {
                continue;
            };
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                let _ = writeln!(
                    out,
                    "{p}: ok — {program}: {} (remove that copy to have atpkg manage {program})",
                    row.state
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: recorded as `{}`, but that copy is gone — system \
                     copy gone: the next `aterm pkg update` reinstalls the managed copy",
                    row.state
                );
            }
        }
        // (10b) MEMBERS WAITING ON ELEVATION (`needs admin — run: aterm pkg install
        // <name>`): not a fault — the unattended pass cannot elevate — but the one line
        // that tells the user which act is theirs.
        for (program, row) in &s.programs {
            if row.state.starts_with(crate::state::NEEDS_ADMIN_PREFIX) {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: {} (in a terminal; sudo asks there)",
                    row.state
                );
            }
        }
        // (10c) MEMBERS OBTAINED THROUGH ANOTHER PROTOCOL (`installed via <protocol>:
        // <path>` — Homebrew's pkg, Apple's Command Line Tools): proven by the recorded
        // `provides` path, re-checked here the way a system copy is. Gone ⇒ the next
        // pass records `needs admin` and the explicit door reinstalls it.
        for (program, row) in &s.programs {
            let Some((protocol, path)) = crate::state::installed_via_path(&row.state) else {
                continue;
            };
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                let _ = writeln!(
                    out,
                    "{p}: ok — {program}: {} (kept current by {protocol}, not by aterm)",
                    row.state
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: recorded as `{}`, but that path is gone — the \
                     next pass records it as needing admin; reinstall: aterm pkg install \
                     {program}",
                    row.state
                );
            }
        }
    }
    // (10e) MEMBERS BLOCKED BY A REQUIREMENT (`blocked by <dep>: <dep state>`, §17.10):
    // a DEFERRED state, never a fault — the pass retries every six hours, and the
    // dependency's own row (quoted in the state) says whose act unblocks it. The
    // explicit door resolves both, in order.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            if let Some((dep, _)) = crate::state::blocked_by(&row.state) {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: {} (installs once {dep} is; `aterm pkg install \
                     {program}` does both, in order)",
                    row.state
                );
            }
        }
    }
    // (10g) MEMBERS HELD ON THEIR CURRENT BUILD (`held: …`, [`crate::state::held_unpublished`]):
    // the update lane's row for an INSTALLED member whose coherence group's new pin is
    // not published for this target. A DEFERRED state, never a fault: nothing was
    // downloaded or staged, the group stays whole on the builds it has, and the pass
    // that finds the build published moves it. Said here as `ok`, quoting the row, so
    // the frozen tuple is VISIBLE — without this line the only trace of it was its
    // siblings' `pinned by index <old>` rows, which read as stale (measured on an
    // Intel Mac, 2026-09-15: trust-cg/-ir/-vc "pinned by index 15" beside index 32).
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            if row.state.starts_with(crate::state::HELD_PREFIX) {
                let _ = writeln!(
                    out,
                    "{p}: ok — {program}: {} (nothing to do here; the next pass moves the \
                     group once the index publishes for this target)",
                    row.state
                );
            }
        }
    }
    // (10d) SHADOWED MANAGED MEMBERS (design S5). For every managed member and every tool
    // it exposes, a foreign executable of that name EARLIER on PATH than the managed
    // bin/ is what actually runs — silently ahead of the build the index pins. Probed
    // LIVE against this process's PATH, never trusted from the record: a warning, never a
    // fault, and never "fixed" here — the user owns PATH.
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        let shadow = crate::ops::active_tools(layout, program, *build)
            .into_iter()
            .find_map(|tool| {
                crate::vendor::shadowing_binary_on_path(&layout.prefix, tool.as_str(), path_var)
                    .map(|path| (tool, path))
            });
        if let Some((tool, path)) = shadow {
            // An AGENT PROGRAM whose `agents/` twin is laid and current (2026-09-16):
            // aterm's copy is what every tab runs (owner decision 2026-09-10), so this
            // is THIS shell's state — a shell that has not run the hook — and the
            // remedy is in place, CHECKED for this machine (`cli::shell_remedy_command`:
            // the hook sourced where it stands, `. ~/.aterm/shell.d/00-atpkg.zsh`; a PATH
            // line only where no hook file exists; never `exec $SHELL`, which drops an
            // aterm tab's shell integration — measured 2026-09-16). Never
            // "open a new tab", never "remove or reorder that copy" (owner, 2026-09-16:
            // "all the latest and best MUST WORK IN THE SAME TAB").
            if let Some(row) =
                crate::cli::agent_shell_shadow(layout, program, *build, &path, path_var)
            {
                let _ = writeln!(out, "{p}: warn — {program}: {row}");
                continue;
            }
            // The canonical state in the canonical words, then — when `alab-<tool>` is
            // laid — the one trailing sentence that names the way to the managed copy
            // without touching PATH (`cli::alias_fix`; never part of the state).
            let fix = crate::cli::alias_fix(layout, &tool, program, path_var)
                .map(|f| format!(" — {f}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{p}: warn — {program}: {} (not the pinned build; atpkg never edits PATH — \
                 remove or reorder that copy if you want the managed one){fix}",
                crate::state::shadowed(*build, &path)
            );
        }
    }
    // (10f) MANAGED MEMBERS WHOSE SHIM EXPORTS AN ENVIRONMENT (design S7): a vendor tool
    // whose signed manifest declared `shim_env` runs, through the managed shim, with its
    // own updater off — `self-update off (DISABLE_AUTOUPDATER=1)`. Read off the shim as
    // laid (the thing that runs), printed as ONE trailing sentence after the canonical
    // row, never inside it; withheld when a foreign copy shadows the shim (the env never
    // reaches a system copy — that member's line is (10d)'s warn). An `ok`, never a fault.
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        let tools = crate::ops::active_tools(layout, program, *build);
        let Some(fix) = tools
            .iter()
            .find_map(|t| crate::cli::shim_env_fix(&layout.shim(t)))
        else {
            continue;
        };
        if tools.iter().any(|t| {
            crate::vendor::shadowing_binary_on_path(&layout.prefix, t.as_str(), path_var).is_some()
        }) {
            continue;
        }
        let state = status
            .as_ref()
            .and_then(|s| s.programs.get(program))
            .filter(|r| crate::state::managed_pin(&r.state).is_some())
            .map_or_else(
                || crate::state::managed(*build, build_floor.index_build),
                |r| r.state.clone(),
            );
        let _ = writeln!(
            out,
            "{p}: ok — {program}: {state} — {fix} (updates arrive with the ALab index)"
        );
    }
    // EVERY problem, not the first one. This scan used to `.find()`, so a second failing
    // program was invisible until the first was fixed — a diagnostic that reveals its
    // findings one per repair cycle is not triage, it is a guessing game, and `status.toml`
    // is a `BTreeMap` so which one won was alphabetical accident.
    let recorded_problems = recorded_problems(status.as_ref());
    let declined = layout.declined().is_file();
    // (11) COMPLETENESS AGAINST THE SIGNED INDEX — the check that was missing.
    //
    // Everything above audits what the RECORD mentions: shims for installed programs,
    // rows in `status.toml`, faults the last pass wrote down. A program the machine
    // wants and never attempted has none of those — no shim, no row, no fault — so it
    // was invisible to every check, and `doctor` reported "healthy" while counting the
    // programs that DID arrive. That is not a hypothetical: a leaky test wrote `trust`
    // (the compiler) into a real machine's removed ledger, which suppressed its stub and
    // — because a coherence group only activates once one member is installed — took the
    // whole `rustc` tuple with it. Ten programs were served, six were installed, and this
    // report said "healthy — 6 ALab program(s) active" for eight days while every ALab
    // repo on that machine failed to compile.
    //
    // The index is the only thing that knows what SHOULD be here, so ask it. Offline and
    // best-effort by construction (`cached_index`): an unreachable index must never
    // invent a missing program.
    let (missing_unexplained, missing_on_purpose, dev_linked) =
        missing_against_index(layout, &installed);
    let mut toolset_problem = false;
    if declined {
        // Intended emptiness. Say so, so it does not read as a fault.
        let _ = writeln!(
            out,
            "{p}: the ALab toolset was removed on this machine (`aterm pkg install \
             --default-set` reinstalls it)"
        );
    } else if installed.is_empty() {
        toolset_problem = true;
        match recorded_problems.first() {
            Some(why) => {
                let _ = writeln!(out, "{p}: PROBLEM — no ALab programs are installed ({why})");
            }
            // No per-program row survives an ENVIRONMENTAL failure any more — an
            // unreachable index says nothing about any particular program — so the
            // aggregate sentence is now the only place the reason lives. Preferring it to
            // the generic hint is what keeps "why did nothing arrive?" answerable offline.
            None => match status
                .as_ref()
                .map(|s| s.outcome.as_str())
                .filter(|o| !o.is_empty())
            {
                Some(outcome) => {
                    let _ = writeln!(
                        out,
                        "{p}: PROBLEM — no ALab programs are installed (last attempt: {outcome})"
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "{p}: PROBLEM — no ALab programs are installed. Fix: aterm pkg install \
                     --default-set (installs the whole ALab toolset; if no build is \
                     published for this machine it names the reason and exits 2)"
                    );
                }
            },
        }
    } else if !recorded_problems.is_empty() {
        // Something IS installed, but the record carries live failures — a partial first
        // run, a blocked disk, a member with no build for this triple, an aborted
        // coherence-group transaction.
        toolset_problem = true;
        // No COUNT here. The tail already prints "found N problem(s)" on its own arithmetic
        // (structural failures + the toolset condition as one), and a verdict line carrying
        // a different N would contradict it in the same report — the tail is the line a
        // human reads last and a script would grep. The problems are listed immediately
        // below, so the number is there to be read.
        let _ = writeln!(
            out,
            "{p}: PROBLEM — the toolset is incomplete; {} program(s) active",
            installed.len()
        );
    } else if !missing_unexplained.is_empty() {
        // The machine wants these, does not have them, and NOTHING recorded a reason.
        // Distinct from the branch above: that one reports failures the record already
        // names, this one reports absences the record is silent about.
        toolset_problem = true;
        let _ = writeln!(
            out,
            "{p}: PROBLEM — the toolset is incomplete: {} of {} program(s) the signed \
             index serves are not installed ({})",
            missing_unexplained.len(),
            installed.len()
                + missing_unexplained.len()
                + missing_on_purpose.len()
                + dev_linked.len(),
            missing_unexplained.join(", ")
        );
    } else {
        let _ = writeln!(out, "{p}: {} ALab program(s) active", installed.len());
    }
    // A DELIBERATE removal is not a fault — but it must be VISIBLE. The ledger that
    // records it is a file no user reads, and its effect (no stub, no unattended
    // reinstall, and a coherence group that never activates) is indistinguishable from
    // the program never having existed. Say it plainly, and name the way back.
    for program in &dev_linked {
        let from = crate::linkmode::linked_checkout(layout, program)
            .map(|c| c.display().to_string())
            .unwrap_or_else(|| "a checkout".to_string());
        let _ = writeln!(
            out,
            "{p}: ok — {program}: dev-linked from {from} (not the index build; `aterm pkg unlink \
             {program}` returns it to the index)"
        );
    }
    for program in &missing_on_purpose {
        let _ = writeln!(
            out,
            "{p}: ok — {program}: removed on purpose — no unattended pass reinstalls it \
             (aterm pkg install {program} brings it back)"
        );
    }
    if let Some(start) =
        problem_listing_start(declined, installed.is_empty(), recorded_problems.len())
    {
        for why in recorded_problems.iter().skip(start) {
            let _ = writeln!(out, "{p}:   {why}");
        }
    }

    if fails == 0 && !toolset_problem && tools_cannot_run == 0 && tools_unproven == 0 {
        let _ = writeln!(out, "{p}: healthy");
        true
    } else if fails == 0 && !toolset_problem {
        // Nothing structural — but a managed tool that cannot run, or that doctor cannot
        // show to run, is not health, and this line is the one a reader takes away. The
        // warn above carries its own remedy, so no `next` line: the tail's rule for
        // failures with an inline remedy.
        let _ = writeln!(
            out,
            "{p}: not healthy — {}; none is a structural problem, so the exit code stays 0",
            withheld_summary(tools_cannot_run, tools_unproven)
        );
        true
    } else {
        let total = fails + usize::from(toolset_problem);
        let _ = writeln!(out, "{p}: found {total} problem(s)");
        // THE ONE NEXT ACT — a single command, never a menu: a report that ends in a pile
        // of problems and three suggestions teaches a first-hour user to close the
        // terminal. Priority: install the missing SET (an empty store has exactly one
        // fix), then `update` (a successful pass rewrites every recorded fault row and
        // re-flips diverged shims), then the structural repair of one named program.
        // Failures with their remedy already inline (a stray .sh — "remove it") add no
        // line here rather than a second, vaguer act.
        let next = if toolset_problem && installed.is_empty() {
            Some(String::from("aterm pkg install --default-set"))
        } else if next_publish {
            Some(format!(
                "publish the newer Trust coherence group ({PUBLISH_RUSTC_GROUP} on a \
                 rostered machine holding the seal), then aterm pkg update trust"
            ))
        } else if (!recorded_problems.is_empty() && !declined) || next_update_divergence {
            Some(String::from("aterm pkg update"))
        } else {
            next_install_program.map(|program| format!("aterm pkg install {program}"))
        };
        if let Some(act) = next {
            let _ = writeln!(out, "{p}: next — {act}");
        }
        false
    }
}

/// The bare version token from `<bin> --version`, for comparing two copies of
/// the same program at a glance.
///
/// Takes the SECOND whitespace token (`ay 0.13.0+build.8174.…` -> `0.13.0…`)
/// and drops any `+build…`/commit/date suffix, because the question this
/// answers is "are these the same release", not "which exact commit".
///
/// Best-effort by construction: a binary that will not run, will not answer, or
/// answers in some other shape reports `unknown` and the caller stays quiet
/// about it. `doctor` must never fail because a probe failed — the probe is a
/// convenience, and the store integrity checks above are the real verdict.
fn probe_version(bin: &Path) -> String {
    const UNKNOWN: &str = "unknown";
    let Some(out) = output_bounded(std::process::Command::new(bin).arg("--version")) else {
        return UNKNOWN.to_string();
    };
    if !out.status.success() {
        return UNKNOWN.to_string();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().next() else {
        return UNKNOWN.to_string();
    };
    let Some(token) = line.split_whitespace().nth(1) else {
        return UNKNOWN.to_string();
    };
    token.split('+').next().unwrap_or(token).to_string()
}

/// What the bundle has to ship for tippy to run, as (5f) words it. tippy runs the
/// compiler under its own name and refuses a symbolic link there, measured; the
/// `rustc` alias it once also demanded is gone with the alias.
const TRUST_COMPILER_FIX: &str = "ship trustc as one signed plain file, never a symbolic link";

/// The fix every (5f)(c) warn names but the blocked one: `repair` reconciles the exec roots
/// at `Deep` depth (`cli::repair_store` → `compat::reconcile`), which lays or rebuilds the
/// root and re-renders every trust shim through it, and exits 1 when it cannot. Behind a
/// link or a file at `compat` or `compat/trust` it cannot, on any run, so that warn names
/// [`crate::compat::Blocked::fix`] instead.
const EXEC_ROOT_FIX: &str = "`aterm pkg repair` lays the exec root (a copy-on-write clone — the \
                             store is never modified) and routes the shims";

/// What a doctor warn about a managed tool claims, which decides the words the closing
/// "not healthy" line counts it under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolClaim {
    /// Nothing withheld: the (5f)(c) note.
    Runs,
    /// The warn says the tool cannot run.
    CannotRun,
    /// The warn says the tool may not run, or that doctor cannot tell.
    Unproven,
}

/// The counted clause of the closing "not healthy" line: `cannot` warns that name a
/// managed tool that cannot run and `unproven` warns that name one that may not run or
/// cannot be checked, each under its own words.
fn withheld_summary(cannot: usize, unproven: usize) -> String {
    const UNPROVEN: &str = "may not run or cannot be checked";
    match (cannot, unproven) {
        (c, 0) => format!("{c} warning(s) above name a managed tool that cannot run"),
        (0, u) => format!("{u} warning(s) above name a managed tool that {UNPROVEN}"),
        (c, u) => format!(
            "{c} warning(s) above name a managed tool that cannot run, and {u} name one that \
             {UNPROVEN}"
        ),
    }
}

/// The (5f)(c) line for an active trust build that [`crate::compat::needs_root`], and
/// what it claims ([`ToolClaim`]): a tool that cannot run, or one it cannot show to run
/// (both warns that withhold "healthy"), rather than a note. Pure over the inspection, so
/// every wording is testable without a store.
///
/// * Root absent: every trust shim's guard is false and runs the store path, where the
///   build's tippy refuses — PATH tippy cannot run.
/// * Root differs from the build: a shim whose own file differs runs the store path, and
///   the rest run a tree that is not the build — tippy MAY not run, and nothing vouches
///   for what does; the root, the store build and the first path that differs are each
///   named as what they are.
/// * Root matches, some shims unrouted: those run the store path — tippy cannot run
///   through them; the count and one name (`tippy` when it is among them) are given.
/// * Root matches, every shim routed: the note.
/// * `compat` or `compat/trust` not a real directory ([`crate::compat::Blocked`]): no
///   root is laid or rendered through it, and `aterm pkg repair` refuses too, so the fix
///   is to remove what stands there first. tippy cannot run when no shim carries a route;
///   one routed earlier may still run through the link, which atpkg does not vouch for,
///   so then it MAY not.
fn exec_root_line(p: &str, build: u64, i: &crate::compat::Inspection) -> (String, ToolClaim) {
    use crate::compat::RootState;
    let root = i.root.display();
    let copy = format!(
        "bin/rustc (inode {}) is a separate file from bin/trustc (inode {}; {} byte(s) differ)",
        i.rustc_ino, i.trustc_ino, i.differing
    );
    let n = i.shims.len();
    match (&i.state, i.unrouted.is_empty()) {
        (RootState::Blocked(blocked), _) => (
            format!(
                "{p}: warn — trust build {build}: PATH tippy {} run — {copy}, which this \
                 build's tippy refuses, and {}; fix: {}",
                if i.unrouted.len() == n {
                    "cannot"
                } else {
                    "may not"
                },
                blocked.what(),
                blocked.fix()
            ),
            if i.unrouted.len() == n {
                ToolClaim::CannotRun
            } else {
                ToolClaim::Unproven
            },
        ),
        (RootState::Matches, true) => (
            format!(
                "{p}: note — trust build {build}: {copy}, which its tippy refuses; its {n} trust \
                 shim(s) run each tool from {root}, where rustc holds trustc's bytes (a \
                 copy-on-write clone; the store is untouched) — removed with the build"
            ),
            ToolClaim::Runs,
        ),
        (RootState::Absent, _) => (
            format!(
                "{p}: warn — trust build {build}: PATH tippy cannot run — {copy}, which this \
                 build's tippy refuses, and no exec root stands at {root}; fix: {EXEC_ROOT_FIX}"
            ),
            ToolClaim::CannotRun,
        ),
        (RootState::Differs(at), _) => (
            format!(
                "{p}: warn — trust build {build}: PATH tippy may not run — {copy}, which this \
                 build's tippy refuses, and the exec root {root} is not the store build {} \
                 file for file (first difference: {}); fix: {EXEC_ROOT_FIX}",
                i.build_dir.display(),
                at.display()
            ),
            ToolClaim::Unproven,
        ),
        (RootState::Matches, false) => {
            let example = i
                .unrouted
                .iter()
                .find(|name| name.as_str() == "tippy")
                .unwrap_or(&i.unrouted[0]);
            (
                format!(
                    "{p}: warn — trust build {build}: PATH tippy cannot run through {} of {n} \
                     trust shim(s) (e.g. {example}) — {copy}, which this build's tippy refuses, \
                     and those shims do not route through the exec root {root}; fix: \
                     {EXEC_ROOT_FIX}",
                    i.unrouted.len()
                ),
                ToolClaim::CannotRun,
            )
        }
    }
}

/// The (5f)(f) line for an active trust build whose need for an exec root could not be
/// read ([`crate::compat::inspect`]'s `Err`, which names the path and the error): a warn
/// that withholds "healthy". No re-lay makes a store file readable, so `repair` is not
/// offered; the line points at `aterm pkg verify trust`, which checks every file of the
/// build against its signed tree.
fn exec_root_unread_line(p: &str, build: u64, why: &str) -> String {
    format!(
        "{p}: warn — trust build {build}: cannot tell whether PATH tippy can run — {why}; any \
         exec root standing for the build is left as it is; fix: make that store file readable \
         again (`aterm pkg verify trust` checks the build against its signed tree)"
    )
}

/// The (5f)(d) line for an entry under `<prefix>/compat/trust` that no build needs — an
/// advisory warn: the next `gc` removes it ([`crate::compat::sweep`] takes exactly what
/// [`crate::compat::strays`] classifies), and no tool fails over it.
fn stray_root_line(p: &str, path: &Path, why: crate::compat::Stray) -> String {
    use crate::compat::Stray;
    let at = path.display();
    let what = match why {
        Stray::BuildGone(n) => format!(
            "exec root {at} outlives trust build {n}, which is no longer in the store, and its \
             clones keep that build's reclaimed blocks allocated"
        ),
        Stray::NeedsNone(n) => format!(
            "exec root {at} stands for trust build {n}, which needs none (its bin/rustc is not \
             a separate file from bin/trustc that its tippy refuses)"
        ),
        Stray::NotADirectory => format!(
            "{at} is not a directory, and atpkg lays only exec root directories under that name"
        ),
    };
    format!("{p}: warn — {what}; fix: `aterm pkg gc` removes it")
}

/// Whether the view file `at` presents the store's Trust tool `store`: a clone of it
/// ([`crate::clone::is_clone_of`] — length, mode and time, not inode, so the hard link a
/// view held before clones does not present it) holding its bytes, since the attributes
/// alone cannot tell it from a bundle's separately signed copy. Writes nothing.
fn presents(store: &Path, at: &Path) -> bool {
    crate::clone::is_clone_of(store, at) && matches!(crate::clone::same_bytes(store, at), Ok(true))
}

/// Render a divergence's contested build numbers for the report line.
fn build_list(builds: &[u64]) -> String {
    builds
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whole days since `updated_at` (RFC3339), or `None` if it cannot be parsed.
/// How long the signed index listing may go unreached before `doctor` says so: past
/// this, every pass has been a cached one — the vendor lane publishes daily-ish, so
/// three days of cache is three days a newer claude pin could have been missed.
const INDEX_UNREACHED_WARN_DAYS: i64 = 3;
/// How long an unchanged `index_build` — with the listing reachable — reads as a frozen
/// publisher rather than a quiet week: the ALab pins move on releases, the vendor rows
/// on every Claude Code and Codex release, and thirty days without either is neither.
const INDEX_FROZEN_WARN_DAYS: i64 = 30;

fn index_age_days(updated_at: &str, now: i64) -> Option<i64> {
    let then = crate::flow::rfc3339_to_unix(updated_at)?;
    Some((now - then) / 86_400)
}

/// (5f b) For every recorded seam, each stock name the view holds must present the tool it
/// stands for: the store's while the store's build is what the view presents (a clone of it
/// holding its bytes, [`presents`]), the dev-linked checkout's while trust is dev-linked to
/// a sysroot (the view is exec stubs there, so the stub's target is read rather than an
/// inode compared). `store_bin` is the store build's `bin/` — the active one, or `current`'s
/// when no shim names a build — or `None` when the store holds none. A mismatch is a tool
/// that cannot run: `repair` rebuilds the view, except under a dev-link the seam refuses,
/// where nothing rebuilds it until the link goes.
fn view_stock_names_check(
    out: &mut dyn std::io::Write,
    p: &str,
    layout: &Layout,
    store_bin: Option<PathBuf>,
    tools_cannot_run: &mut usize,
) {
    let source = crate::seam::view_source(layout);
    const STORE_WHAT: &str = "a clone of the store's";
    const STORE_WRONG: &str = "absent, other bytes, or the hard link a view held before clones";
    let (presented, presented_what, wrong, fix) = match (&source, store_bin) {
        (crate::seam::ViewSource::Linked(checkout), _) => (
            checkout.join("bin"),
            "the dev-linked checkout's",
            "absent, or a separate file",
            "`aterm pkg repair` rebuilds the view",
        ),
        (crate::seam::ViewSource::LinkedNoSysroot(_), Some(bin)) => (
            bin,
            STORE_WHAT,
            STORE_WRONG,
            "`aterm pkg unlink trust` — the seam refuses a dev-link whose checkout is not a \
             sysroot, so `repair` cannot rebuild the view until the link goes",
        ),
        (crate::seam::ViewSource::Store, Some(bin)) => (
            bin,
            STORE_WHAT,
            STORE_WRONG,
            "`aterm pkg repair` rebuilds the view",
        ),
        // A refused dev-link with no store build behind the view: nothing the view
        // could be checked against, and nothing it could correctly present — whatever
        // it last presented is what `rustup run trust` runs. Said, with the one fix.
        (crate::seam::ViewSource::LinkedNoSysroot(checkout), None) => {
            for name in crate::seam::recorded_names(layout) {
                *tools_cannot_run += 1;
                let _ = writeln!(
                    out,
                    "{p}: warn — rustup `{name}`: trust is dev-linked to {}, which atpkg cannot \
                     present (no bin/ and lib/, or a tree inside its own prefix), and no \
                     installed build stands behind the view, so `rustc +{name}` runs whatever \
                     the view last presented, or nothing; fix: `aterm pkg unlink trust`",
                    checkout.display()
                );
            }
            return;
        }
        (crate::seam::ViewSource::Store, None) => return,
    };
    for name in crate::seam::recorded_names(layout) {
        let view_bin = crate::seam::view_dir(layout, &name).join("bin");
        for (public, trust) in crate::seam::STOCK_NAMES {
            let tool = presented.join(trust);
            if !tool.is_file() {
                continue;
            }
            let view_tool = view_bin.join(public);
            // A linked view's entries are exec stubs into the checkout; a store view's are
            // clones of the store's tools.
            let presented_ok = match &source {
                crate::seam::ViewSource::Linked(_) => {
                    crate::platform::resolve_shim(&view_tool).is_some_and(|t| t == tool)
                }
                _ => presents(&tool, &view_tool),
            };
            if !presented_ok {
                *tools_cannot_run += 1;
                let _ = writeln!(
                    out,
                    "{p}: warn — rustup `{name}`: {} is not {presented_what} {} ({wrong}), so \
                     `{public} +{name}` runs something other than the managed {trust}, or \
                     nothing; fix: {fix}",
                    view_tool.display(),
                    tool.display()
                );
            }
        }
    }
}

/// The doctor line for a rustup `trust` entry that is NOT the managed seam, or `None`
/// when the entry is absent or already ours (the callers' other checks speak then).
/// Pure over the probe result, so the words are testable without a rustup.
///
/// A foreign LINK names the exact re-point (one `ln -sfn`, then `aterm pkg repair` to
/// record it); a foreign DIRECTORY/FILE cannot be re-pointed over — `ln -sfn` onto a
/// directory would lay the link INSIDE it — so that shape gets [`crate::seam::DETACH_FIX`].
/// A link into the store — `current`, or a numbered build, the layouts from before the
/// view — is a note, not a warn: `repair` re-points it to the view by itself.
fn seam_line(st: &crate::seam::SeamStatus, layout: &Layout) -> Option<String> {
    use crate::seam::Entry;
    let target = crate::seam::seam_target(layout, "trust");
    match &st.entry {
        Ok(Entry::Link(raw)) if !st.in_prefix => Some(format!(
            "warn — rustup `trust` -> {} is NOT the managed store ({}): `cargo +trust`, \
             `rustup run trust` and every repo pinning `channel = \"trust\"` build with THAT \
             copy, not the one `aterm pkg update` keeps current. re-point it (doctor never \
             will): ln -sfn '{}' '{}' — then `aterm pkg repair` records the seam; revert by \
             re-linking the old target the same way",
            raw.display(),
            target.display(),
            target.display(),
            st.path.display()
        )),
        Ok(Entry::Link(_)) if !st.targets_view => Some(format!(
            "note — rustup `trust` -> {} is inside the store, not the view atpkg lays \
             ({}); `aterm pkg repair` re-points it so the stock names rustup resolves are \
             the managed tools and updates move the channel",
            st.target.as_deref().unwrap_or(&st.path).display(),
            target.display()
        )),
        Ok(entry @ (Entry::Dir | Entry::File | Entry::Other)) => Some(format!(
            "warn — rustup `trust` at {} is {} — not a link into the managed store ({}), so \
             `cargo +trust` builds with whatever that holds; {}",
            st.path.display(),
            entry.describe(layout),
            target.display(),
            crate::seam::DETACH_FIX
        )),
        Ok(Entry::Link(_) | Entry::Absent) => None,
        Err(e) => Some(format!(
            "warn — rustup `trust` at {} could not be inspected ({e})",
            st.path.display()
        )),
    }
}

/// Where `rustup` is: the first executable `rustup` on `PATH`, else
/// `$CARGO_HOME/bin/rustup`, else `~/.cargo/bin/rustup` — the two places
/// rustup-init lays it, which an app-spawned pass (no rc file read) does not
/// have on its PATH. Existence only; whether it answers is [`rustup_present`].
fn rustup_binary(home: Option<&Path>) -> Option<std::path::PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("rustup"))
            .find(|candidate| candidate.is_file())
    });
    on_path
        .or_else(|| {
            std::env::var_os("CARGO_HOME")
                .map(|cargo_home| Path::new(&cargo_home).join("bin").join("rustup"))
                .filter(|candidate| candidate.is_file())
        })
        .or_else(|| {
            let candidate = home?.join(".cargo").join("bin").join("rustup");
            candidate.is_file().then_some(candidate)
        })
}

/// Whether the `rustup` at `bin` answers `--version` inside [`PROBE_TIMEOUT`].
fn rustup_present(bin: &Path) -> bool {
    output_bounded(std::process::Command::new(bin).arg("--version"))
        .is_some_and(|o| o.status.success())
}

/// Does rustup resolve a `trust` channel? `rustup which cargo --toolchain trust`
/// exits non-zero (and prints the famous `'rustc' is not installed for the
/// custom toolchain 'trust'`) when the channel is absent or dangling; a linked
/// channel answers with the cargo path. Bounded like every other probe here.
fn rustup_trust_channel_resolves(bin: &Path) -> bool {
    output_bounded(std::process::Command::new(bin).args(["which", "cargo", "--toolchain", "trust"]))
        .is_some_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// How long ONE `--version` probe may take before `doctor` gives up on it.
///
/// These are local binaries printing one line; a healthy one answers in
/// milliseconds. The ceiling exists for the unhealthy case, which is not
/// hypothetical here: `doctor` and `status` probe EVERY installed program plus
/// `rustup`, so one wedged binary — a stale NFS mount, a `rustup` shim waiting
/// on a network toolchain fetch, a program stopped on a debugger — hung the
/// whole report with no output and no way to tell what it was waiting for.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Poll interval while waiting for a probe to exit.
const PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Run a version probe with a bounded wall clock, killing and reaping it on
/// timeout. `None` when it could not be spawned, did not finish in time, or
/// could not be waited for.
///
/// Fails to `None`, not to an error, and that is the right direction HERE (the
/// opposite of the updater's fail-closed helpers this mirrors): the probe is a
/// convenience that renders one column of a report, and `doctor`'s contract is
/// that it never fails because a probe failed — the store integrity checks are
/// the verdict. A timed-out probe reads `unknown`, exactly like a binary that
/// will not run.
pub(crate) fn output_bounded(cmd: &mut std::process::Command) -> Option<std::process::Output> {
    use std::io::Read as _;
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                std::thread::sleep(PROBE_POLL.min(remaining));
            }
            Err(_) => return None,
        }
    };
    // The child has exited, so its stdout is closed and this read cannot block —
    // and a version line is one line, far inside any pipe buffer, so nothing
    // could have wedged the child on a full pipe before it got here either.
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    Some(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// Copy-pasteable manual PATH-append for the shell of THIS platform: PowerShell on Windows
/// (';' separator, `$env:PATH`), POSIX `export` elsewhere. Only a fallback — an aterm shell
/// auto-sources the `shell.d` hook that does this already.
#[cfg(windows)]
fn manual_path_hint(bin: &Path) -> String {
    format!(
        "$env:PATH += \";{}\"  (PowerShell; or add it to your User PATH via System Settings)",
        bin.display()
    )
}
#[cfg(not(windows))]
fn manual_path_hint(bin: &Path) -> String {
    format!("export PATH=\"$PATH:{}\"", bin.display())
}

/// The `shell.d` hook extension the interactive shell on this platform actually sources.
#[cfg(windows)]
fn native_hook_ext() -> &'static str {
    "ps1"
}
#[cfg(not(windows))]
fn native_hook_ext() -> &'static str {
    "zsh"
}

/// Report the aterm app's own update posture beside the toolchain's.
///
/// THE DESIGN OVERSIGHT THIS PARTIALLY CLOSES. atpkg manages the ALab toolchain; the aterm
/// app manages itself through a SEPARATE updater; and `atpkg` is a binary inside that app.
/// So the component a user is most likely to be running a stale copy of is the one thing
/// atpkg had nothing to say about — you could ask it about ten programs and get no hint
/// that the eleventh, the one answering, was months old.
///
/// The app cannot simply BECOME an atpkg program: swapping a running, notarized `.app`
/// needs Gatekeeper assessment, a crash-loop boot sentinel, and the in-session overlap
/// handoff (the running app spawns its successor and hands every PTY across), none of which
/// the shim-and-store model provides. What atpkg can do — and now does — is stop pretending
/// the app is not there, by reading the updater's own records rather than forming a second
/// opinion about them. What it must NEVER do is tell the user to reopen the app for an
/// update: the app applies a staged build to itself in-session, so there is nothing to
/// reopen — a note that said otherwise stood over the live mechanism until 2026-08-30.
///
/// Silent when there is no updater state: a bare CLI install is a legitimate posture, not a
/// fault.
fn report_aterm_posture(layout: &crate::store::Layout, p: &str, out: &mut dyn std::io::Write) {
    let Some(support) = layout.prefix.parent() else {
        return;
    };
    report_aterm_posture_at(&support.join("Updates"), running_bundle_build(), p, out);
}

/// [`report_aterm_posture`] over an explicit updater ledger dir and the SEALED build
/// number of the bundle this process runs from (`None` outside a bundle), so the arms
/// are testable against a fixture.
///
/// "Installed on disk" is the NEWER of the updater's receipt and the bundle's own
/// `CFBundleVersion`: a bundle placed by hand (`install.sh`, a DMG drag) is newer than
/// any receipt the updater wrote, and reading the receipt alone made doctor say
/// "running build X but build Y is installed on disk" with Y OLDER than X — on this
/// very machine, for a month (2026-09-10 audit). The note fires only when what is on
/// disk is strictly newer than what runs; an equal or older receipt is the quiet line.
fn report_aterm_posture_at(
    updates: &Path,
    bundle_build: Option<u64>,
    p: &str,
    out: &mut dyn std::io::Write,
) {
    let field = |file: &str, key: &str| -> Option<String> {
        let text = std::fs::read_to_string(updates.join(file)).ok()?;
        text.lines()
            .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
    };
    let Some(receipt) = field("installed.toml", "build_number") else {
        return;
    };
    let installed = match (receipt.parse::<u64>().ok(), bundle_build) {
        (Some(r), Some(b)) => r.max(b).to_string(),
        (None, Some(b)) => b.to_string(),
        _ => receipt,
    };
    let current = field("status.toml", "current_build");
    let newer_on_disk = match (current.as_deref(), installed.parse::<u64>().ok()) {
        (Some(c), Some(i)) => c.parse::<u64>().is_ok_and(|c| i > c),
        _ => false,
    };
    match (current, field("status.toml", "staged_build")) {
        (Some(current), Some(staged)) if current != staged => {
            // The staged build is applied IN-SESSION by the app's own overlap handoff
            // (automatic at the first quiet moment — forced within ~2 min — by default, one
            // click otherwise); the shells keep running. atpkg names that lane and the
            // `aterm ctl update` verbs, and never asks the user to reopen anything.
            let _ = writeln!(
                out,
                "{p}: note — aterm is running build {current} (installed {installed}); build \
                 {staged} is staged and aterm applies it at its next quiet moment, in-session \
                 and in place — your shells keep running (`aterm ctl update status` shows it; \
                 `aterm ctl update apply` presses it now)"
            );
        }
        (Some(current), _) if newer_on_disk => {
            // A newer bundle is already on disk: the GUI activates it in place, the same
            // in-session lane.
            let _ = writeln!(
                out,
                "{p}: note — aterm is running build {current} but build {installed} is \
                 installed on disk; aterm activates it in-session, in place — your shells \
                 keep running (`aterm ctl update status` shows it)"
            );
        }
        _ => {
            let _ = writeln!(out, "{p}: ok — aterm build {installed} installed");
        }
    }
}

/// The sealed `CFBundleVersion` of the `.app` this process runs from, read off its
/// `Contents/Info.plist`; `None` outside a bundle (a source build, a test binary) or on
/// any doubt. A plain text scan of the XML plist the release cutter emits — no
/// `PlistBuddy` subprocess for a diagnostic — that binds the value to the key
/// immediately before it ([`plist_bundle_version`]).
fn running_bundle_build() -> Option<u64> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let macos = exe.parent()?;
    if macos.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let text =
        crate::metadata_io::read_bounded_regular_utf8(&contents.join("Info.plist"), 1024 * 1024)
            .ok()?;
    plist_bundle_version(&text)
}

/// `CFBundleVersion` out of an XML plist: the `<string>` IMMEDIATELY after the key
/// (modulo whitespace) — never a string further on that another key owns — parsed as
/// the integer build number the cutter seals. `None` for a missing key, an intervening
/// element, or a non-integer value.
fn plist_bundle_version(text: &str) -> Option<u64> {
    let key = "<key>CFBundleVersion</key>";
    let after = text.find(key)? + key.len();
    let value = text[after..].trim_start().strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    value[..end].trim().parse::<u64>().ok()
}

/// The index (in `split_paths` order) of the first `PATH` entry holding an executable
/// `name` OUTSIDE the managed `prefix` — the reroute dir lives under it, so a laid stub
/// never counts as its own upstream. A local walk on purpose: the vendor probes are
/// [`crate::store::ToolName`]-gated and `cargo` is deny-listed there. Relative entries
/// are skipped (they name the cwd, not a toolchain), and only an executable regular file
/// counts — a directory named `cargo` is not a copy that runs.
fn upstream_index_on_path(
    entries: &[std::path::PathBuf],
    prefix: &Path,
    name: &str,
) -> Option<usize> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    entries.iter().position(|dir| {
        if dir.as_os_str().is_empty() || !dir.is_absolute() || dir.starts_with(prefix) {
            return false;
        }
        let Ok(meta) = std::fs::metadata(dir.join(&file)) else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    })
}

/// The clause of the Spotlight warning that says how far the unattended pass reaches:
/// the exposed target dirs beside a Cargo.toml are the pass's to migrate; the
/// FREE-STANDING ones (`free`, no Cargo.toml beside them) are pointed at by an env var
/// or a build system the pass cannot re-point, so it never renames them and the
/// operator has to — by name, after re-pointing. Up to three are named; a count that
/// names nothing sends the reader to `aterm pkg noindex` to find out which.
fn spotlight_reach(in_repo: usize, free: &[&crate::noindex::Target]) -> String {
    const SHOWN: usize = 3;
    let named = || {
        let mut names: Vec<String> = free
            .iter()
            .take(SHOWN)
            .map(|t| t.path.display().to_string())
            .collect();
        if free.len() > SHOWN {
            names.push(format!("+{} more", free.len() - SHOWN));
        }
        names.join(", ")
    };
    match (in_repo, free.len()) {
        (_, 0) => {
            "the next update pass migrates them (every one is beside a Cargo.toml)".to_string()
        }
        (0, _) => format!(
            "NONE is beside a Cargo.toml, so the update pass never renames any of them: each \
             is a free-standing target dir an env var or a build system points at ({}) — \
             re-point that, then `aterm pkg noindex apply <dir>` by name",
            named()
        ),
        (r, f) => format!(
            "the next update pass migrates the {r} beside a Cargo.toml; the other {f} \
             {} free-standing — an env var or a build system points at {} ({}), which the \
             pass cannot re-point, so it never renames {} — re-point that, then `aterm pkg \
             noindex apply <dir>` by name",
            if f == 1 { "is" } else { "are" },
            if f == 1 { "it" } else { "each" },
            named(),
            if f == 1 { "it" } else { "them" },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activate::{activate_channel, install_shims};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-doctor-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    fn tool(name: &str) -> crate::store::ToolName {
        crate::store::ToolName::new(name).unwrap()
    }

    fn synthetic_home(label: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("atpkg-dhome-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o700)).unwrap();
        h
    }

    /// The build tree alone — no shims, no channel. Split out so a test can construct the
    /// half-wired states the witness checks are about.
    fn install_build_tree(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        // The concrete executable the shim will forward to (`<program>.exe` on Windows) —
        // it must EXIST for the broken-shim scan (check 4) to see a healthy layout.
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        dir
    }

    fn install(layout: &Layout, program: &str, build: u64) {
        let dir = install_build_tree(layout, program, build);
        install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        activate_channel(layout, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
    }

    fn names(list: &[&str]) -> std::collections::BTreeSet<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn builds(list: &[&str]) -> std::collections::BTreeMap<String, u64> {
        list.iter().map(|s| ((*s).to_string(), 1u64)).collect()
    }

    /// A program the machine wants, does not have, and has NO ledger entry for is the
    /// state no other check in this report can see: no shim, no `status.toml` row, no
    /// recorded fault. It must be named.
    /// A DEV-LINKED program is present, not missing — and the link outranks a stale
    /// removed-ledger line. Measured 2026-09-14: a `# deliberate` removal of trust from
    /// 2026-08-27 made doctor say "removed on purpose" about the toolchain every build on
    /// the box was running through `aterm pkg link trust <stage2>`.
    #[test]
    fn a_dev_linked_program_is_neither_missing_nor_removed_even_when_the_ledger_names_it() {
        let (unexplained, on_purpose, dev_linked) = split_missing(
            &names(&["ay", "trust"]),
            &builds(&[]),
            &names(&["trust"]),
            &names(&["trust"]),
        );
        assert_eq!(
            dev_linked,
            vec!["trust".to_string()],
            "the link is the newer decision"
        );
        assert!(
            on_purpose.is_empty(),
            "a linked program is not 'removed on purpose': {on_purpose:?}"
        );
        assert_eq!(
            unexplained,
            vec!["ay".to_string()],
            "only the genuinely absent one is missing"
        );
    }

    #[test]
    fn a_wanted_program_that_never_arrived_is_unexplained() {
        let (unexplained, on_purpose, _dev_linked) = split_missing(
            &names(&["ay", "trust"]),
            &builds(&["ay"]),
            &names(&[]),
            &names(&[]),
        );
        assert_eq!(unexplained, vec!["trust".to_string()]);
        assert!(on_purpose.is_empty());
    }

    /// A deliberate removal is a DECISION, not a fault — reporting it as a problem would
    /// be the manager arguing with the user. It still has to be visible, which is the
    /// other half of what went wrong: the ledger is a file nobody reads.
    #[test]
    fn a_removed_program_is_explained_never_a_fault() {
        let (unexplained, on_purpose, _dev_linked) = split_missing(
            &names(&["ay", "trust"]),
            &builds(&["ay"]),
            &names(&["trust"]),
            &names(&[]),
        );
        assert!(unexplained.is_empty(), "a recorded decision is not a fault");
        assert_eq!(on_purpose, vec!["trust".to_string()]);
    }

    #[test]
    fn a_complete_machine_reports_nothing() {
        let (unexplained, on_purpose, _dev_linked) = split_missing(
            &names(&["ay", "trust"]),
            &builds(&["ay", "trust"]),
            &names(&[]),
            &names(&[]),
        );
        assert!(unexplained.is_empty() && on_purpose.is_empty());
    }

    /// THE INCIDENT, in the shape it actually had (2026-08-23 → 2026-08-31).
    ///
    /// A leaky unit test wrote `trust` into a real machine's removed ledger. That
    /// suppressed the compiler's stub, and because a coherence group only activates once
    /// one of its members is installed, the whole `rustc` tuple — trust, trust-ir,
    /// trust-cg, trust-vc — never arrived. The signed index served ten programs; six were
    /// installed; `doctor` said "healthy — 6 ALab program(s) active" for eight days while
    /// every ALab repo on that machine failed to compile.
    ///
    /// Note what this asserts: the ledger explains `trust` ONLY. The three siblings it
    /// took down with it are unexplained, and naming them is what turns "healthy" into a
    /// report that points at the real state.
    #[test]
    fn the_suppressed_compiler_tuple_is_reported() {
        let (unexplained, on_purpose, _dev_linked) = split_missing(
            &names(&[
                "ay", "clean", "nn", "ny", "trust", "trust-cg", "trust-ir", "trust-mc", "trust-vc",
                "ty",
            ]),
            &builds(&["ay", "clean", "nn", "ny", "trust-mc", "ty"]),
            &names(&["trust"]),
            &names(&[]),
        );
        assert_eq!(
            unexplained,
            vec![
                "trust-cg".to_string(),
                "trust-ir".to_string(),
                "trust-vc".to_string()
            ],
            "the siblings the suppressed compiler took with it must be named"
        );
        assert_eq!(
            on_purpose,
            vec!["trust".to_string()],
            "the ledger accounts for the compiler itself, and that must be said out loud"
        );
    }

    /// Fail-quiet: with no cached index there is no trustworthy answer about what SHOULD
    /// be installed, and inventing one would turn an unreachable network into a pile of
    /// phantom missing programs.
    #[test]
    fn no_cached_index_invents_no_missing_programs() {
        let l = layout("no-index");
        let (unexplained, on_purpose, dev_linked) = missing_against_index(&l, &builds(&["ay"]));
        assert!(unexplained.is_empty() && on_purpose.is_empty() && dev_linked.is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn index_age_math() {
        // 0 days.
        let then = crate::flow::rfc3339_to_unix("2026-07-01T00:00:00Z").unwrap();
        assert_eq!(index_age_days("2026-07-01T00:00:00Z", then), Some(0));
        // 31 days > 30.
        assert_eq!(
            index_age_days("2026-07-01T00:00:00Z", then + 31 * 86_400),
            Some(31)
        );
        // Garbage → None.
        assert_eq!(index_age_days("not-a-date", then), None);
    }

    // (9) A rustup `trust` entry that RESOLVES but is not the managed seam — m21's
    // `~/.rustup/toolchains/trust -> $HOME/trust/build/host/stage2` — is a WARN that names
    // the one re-point command and moves nothing; a link at a numbered store build is a
    // note; the seam itself and an absent entry say nothing here.
    #[cfg(unix)]
    #[test]
    fn a_foreign_rustup_trust_entry_is_named_with_its_repoint_and_never_repointed() {
        let l = layout("foreign-seam");
        // The managed store: trust/6808 with current -> 6808.
        let build = l.build_dir("trust", 6808);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"trustc").unwrap();
        crate::activate::atomic_symlink(&build, &crate::seam::store_current(&l)).unwrap();
        let home = synthetic_home("foreign-seam");
        let rustup = home.join(".rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
        let dev = home.join("trust/build/host/stage2");
        std::fs::create_dir_all(dev.join("bin")).unwrap();
        let entry = rustup.join("toolchains/trust");
        std::os::unix::fs::symlink(&dev, &entry).unwrap();
        let st = crate::seam::status(&l, &rustup, "trust");
        let line = seam_line(&st, &l).expect("a foreign link is said");
        assert!(line.starts_with("warn — "), "{line}");
        assert!(line.contains("is NOT the managed store"), "{line}");
        assert!(
            line.contains(&format!(
                "ln -sfn '{}' '{}'",
                crate::seam::seam_target(&l, "trust").display(),
                entry.display()
            )),
            "the exact re-point command, quoted — the real prefix has a space in \
             `Application Support`: {line}"
        );
        assert!(line.contains("aterm pkg repair"), "{line}");
        // Nothing moved: the entry still points at the dev build.
        assert_eq!(std::fs::read_link(&entry).unwrap(), dev);
        // A numbered build inside the store is a note, not a warn.
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(&build, &entry).unwrap();
        let line = seam_line(&crate::seam::status(&l, &rustup, "trust"), &l).unwrap();
        assert!(line.starts_with("note — "), "{line}");
        assert!(line.contains("inside the store, not the view"), "{line}");
        // So is the layout from before the view: a link at `current`.
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(crate::seam::store_current(&l), &entry).unwrap();
        let line = seam_line(&crate::seam::status(&l, &rustup, "trust"), &l).unwrap();
        assert!(line.starts_with("note — "), "{line}");
        // The seam itself, and no entry at all: nothing to say from this line.
        std::fs::remove_file(&entry).unwrap();
        crate::seam::refresh_view(&l, "trust").unwrap();
        std::os::unix::fs::symlink(crate::seam::seam_target(&l, "trust"), &entry).unwrap();
        assert_eq!(
            seam_line(&crate::seam::status(&l, &rustup, "trust"), &l),
            None
        );
        std::fs::remove_file(&entry).unwrap();
        assert_eq!(
            seam_line(&crate::seam::status(&l, &rustup, "trust"), &l),
            None
        );
        // A real directory gets the detach fix, since nothing can be linked over it.
        std::fs::create_dir_all(entry.join("bin")).unwrap();
        let line = seam_line(&crate::seam::status(&l, &rustup, "trust"), &l).unwrap();
        assert!(line.contains("a real directory"), "{line}");
        assert!(line.contains(crate::seam::DETACH_FIX), "{line}");
        // Through the whole report, with the synthetic home carrying the foreign link:
        // the line is printed and the report is still advisory about it.
        std::fs::remove_dir_all(&entry).unwrap();
        std::os::unix::fs::symlink(&dev, &entry).unwrap();
        if rustup_binary(Some(&home)).is_some_and(|bin| rustup_present(&bin))
            && std::env::var_os("RUSTUP_HOME").is_none()
        {
            let mut out = Vec::new();
            let path = std::env::join_paths([l.bin_dir()]).unwrap();
            let _ = run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut std::io::sink(),
            );
            let text = String::from_utf8_lossy(&out);
            assert!(
                text.contains("is NOT the managed store"),
                "the report carries the seam line:\n{text}"
            );
            assert_eq!(
                std::fs::read_link(&entry).unwrap(),
                dev,
                "doctor moved nothing"
            );
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn healthy_layout_returns_true() {
        let l = layout("healthy");
        install(&l, "ay", 18);
        let home = synthetic_home("healthy");
        // PATH contains the managed bin/ so even the advisory check is clean.
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a clean install is healthy"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// (3b) REROUTE: per-row stub state (laid / missing / foreign) and PATH ORDER — the
    /// measured failure (2026-09-07) was upstream `~/.cargo/bin` AHEAD of the reroute
    /// dir, never the dir's absence — with the reroute dir compared against a fake
    /// upstream `cargo` outside the prefix through the injected `path_var`. Every line
    /// is advisory: the warn leaves a healthy toolset healthy.
    #[cfg(unix)]
    #[test]
    fn reroute_section_reports_stub_state_and_path_order() {
        let l = layout("reroute");
        install(&l, "ay", 18);
        let home = synthetic_home("reroute");
        // A fake upstream cargo OUTSIDE the prefix (a sibling temp dir), so the walk
        // counts it — a copy under the prefix would be the stub counting itself.
        let upstream = std::env::temp_dir().join(format!(
            "atpkg-doctor-reroute-upstream-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&upstream);
        std::fs::create_dir_all(&upstream).unwrap();
        std::fs::write(upstream.join("cargo"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            upstream.join("cargo"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let run = |entries: &[PathBuf]| {
            let path = std::env::join_paths(entries).unwrap();
            let mut out: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut std::io::sink(),
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        // Nothing laid and the reroute dir absent from PATH (a shell outside a session):
        // every row missing as a warning, the order line a note, the toolset still healthy.
        let (ok, out) = run(&[l.bin_dir(), upstream.clone()]);
        assert!(ok, "missing reroute stubs are advisory:\n{out}");
        for row in crate::reroute::TABLE {
            assert!(
                out.contains(&format!(
                    "doctor: warn — reroute stub {} missing (an aterm session lays it at \
                     spawn; `aterm pkg repair` re-lays it; a recorded decline lays none)",
                    row.upstream
                )),
                "{out}"
            );
        }
        assert!(
            out.contains(
                "doctor: note — reroute dir not on this PATH (expected outside an aterm session)"
            ),
            "{out}"
        );
        crate::reroute::lay(&l).unwrap();
        let reroute = l.reroute_dir();
        // Upstream AHEAD of the reroute dir: the measured failure, as a warning — exit 0.
        let (ok, out) = run(&[upstream.clone(), reroute.clone(), l.bin_dir()]);
        assert!(ok, "a mis-ordered PATH is advisory:\n{out}");
        assert!(
            out.contains("doctor: ok — reroute stub cargo laid"),
            "{out}"
        );
        assert!(
            out.contains(
                "doctor: warn — upstream cargo at PATH[0] precedes the reroute dir at PATH[1]; \
                 inside an aterm session the spawn seam and shell integration put it first"
            ),
            "{out}"
        );
        // The reroute dir first: the order a session guarantees.
        let (ok, out) = run(&[reroute.clone(), upstream.clone(), l.bin_dir()]);
        assert!(ok, "{out}");
        assert!(
            out.contains("doctor: ok — reroute dir precedes upstream cargo (PATH[0] < PATH[1])"),
            "{out}"
        );
        // On PATH with no upstream cargo anywhere: said, not warned.
        let (_, out) = run(&[reroute.clone(), l.bin_dir()]);
        assert!(
            out.contains(
                "doctor: ok — reroute dir on PATH (PATH[0]); no upstream cargo on this PATH"
            ),
            "{out}"
        );
        // A foreign occupant of a row's name is named by path and never claimed.
        std::fs::write(reroute.join("z3"), "#!/bin/sh\nexit 0\n").unwrap();
        let (ok, out) = run(&[reroute.clone(), upstream.clone(), l.bin_dir()]);
        assert!(ok, "{out}");
        assert!(
            out.contains(&format!(
                "doctor: warn — {} is not ours (foreign file; never touched)",
                reroute.join("z3").display()
            )),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&upstream);
    }

    /// THE QUESTION THE COMMAND EXISTS FOR. Every structural check passes vacuously
    /// on a store that received nothing, so `doctor` used to print "healthy" to the
    /// one user most in need of an answer — someone whose toolchain never arrived,
    /// running the command the docs point them at. An empty store is not health.
    #[test]
    fn an_empty_store_is_not_healthy() {
        let l = layout("empty-store");
        let home = synthetic_home("empty-store");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        // Structurally spotless — and still not healthy, because there is no toolchain.
        assert!(
            !run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a store with no ALab programs must report a problem, not health"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// ONE TYPO MUST NOT CONDEMN A HEALTHY TOOLSET.
    ///
    /// A key beginning with `-` cannot be a program: no shim exists under one and the
    /// signed index cannot name one. Before this, a single `atpkg install --help` minted
    /// `[programs.--help]` permanently and doctor read it back as a missing member — so a
    /// machine with ten verified programs reported "the toolset is incomplete" and exited
    /// 1, forever. That exit code is a published contract: `tools/install.sh` hands
    /// `pkg doctor` to every failed-seed installer as THE diagnostic to trust.
    ///
    /// The second case is the one that gives the first its teeth — it proves the scan was
    /// NARROWED to stray flags, not disabled.
    #[test]
    fn a_stray_flag_row_is_not_a_missing_program() {
        let stray_row = |name: &str| {
            let l = layout(&format!("stray-{}", name.trim_start_matches('-')));
            install(&l, "ay", 18);
            let existing = crate::status::read(&l).unwrap_or_default();
            let mut programs = existing.programs;
            programs.insert(
                "ay".to_string(),
                crate::ProgramStatus {
                    installed_build: Some(18),
                    state: "active".into(),
                    tree_root: String::new(),
                },
            );
            programs.insert(
                name.to_string(),
                crate::ProgramStatus {
                    installed_build: None,
                    state: format!("error: {name} is not named in the signed index"),
                    tree_root: String::new(),
                },
            );
            crate::status::write(
                &l,
                &crate::Status {
                    programs,
                    ..existing
                },
            )
            .unwrap();
            l
        };

        let l = stray_row("--help");
        let home = synthetic_home("stray-help");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a mistyped flag left in the record is a stray row, not a missing program"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);

        // …and a plausible PROGRAM name in the same error state still fails, so the change
        // narrowed the scan rather than blunting it.
        let l = stray_row("trust-vc");
        let home = synthetic_home("stray-real");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            !run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a real program name in an error state is still a problem doctor must report"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE SCAN MUST SEE WHAT IT IS FOR. Two independent blindnesses, fixed together
    /// because they are the same failure — a diagnostic that under-reports.
    ///
    /// `aborted: <phase>` is written for every member of a coherence group whose
    /// transaction was killed mid-flight — precisely the state in which a tuple's members
    /// may disagree with each other — and the scan matched only three prefixes, so doctor
    /// pronounced such a machine healthy. That is the mirror image of the stray-row bug:
    /// one invented a fault, this one missed a real one.
    #[test]
    fn an_aborted_transaction_is_a_problem_doctor_can_see() {
        assert!(
            is_problem_state("aborted: activate"),
            "a killed coherence-group transaction is a fault, not an informational state"
        );
        for fault in ["error: x", "unavailable: y", "blocked: z"] {
            assert!(is_problem_state(fault), "{fault} stays a fault");
        }
        // Allow-by-prefix: a state the scan does not recognize reads as benign. Inventing
        // faults from unknown states is how a diagnostic teaches people to ignore it.
        for benign in ["active", "dev-linked (skipped)", "staged 4821"] {
            assert!(!is_problem_state(benign), "{benign} is not a fault");
        }
    }

    /// A YANKED PIN IS NOT HEALTH — the quietest fault in the crate.
    ///
    /// A tombstoned program's shims are replaced by stubs that print "was yanked/revoked",
    /// the broken-shim scan skips tombstones by design, and the program drops out of
    /// `active_builds` — so every other check went quiet at once and doctor said "healthy"
    /// about a machine whose compiler had become a stub.
    #[test]
    fn a_yanked_pin_is_a_problem_not_silence() {
        assert!(
            is_problem_state("tombstoned: pin yanked/below floor"),
            "a yanked pin leaves failing stubs behind; that is not health"
        );
    }

    /// THE LISTING MUST HANG OFF THE BRANCH THAT SPOKE.
    ///
    /// Derived from `store_empty` alone, it was wrong in both directions on a DECLINED
    /// store: with an empty store it skipped a problem no branch had named — losing the
    /// only finding, since `*toolset*: unavailable: …` is the normal single row on a Mac
    /// the index does not serve — and with a populated store it printed every problem as
    /// an orphan line under "the ALab toolset was removed on this machine".
    ///
    /// Exhaustive over (declined) x (store empty) x (0 / 1 / many problems), because that
    /// is the matrix the bug lived in and no single case would have exposed it.
    #[test]
    fn the_problem_listing_follows_the_verdict_that_named_it() {
        for empty in [true, false] {
            for n in [0, 1, 5] {
                assert_eq!(
                    problem_listing_start(true, empty, n),
                    None,
                    "a declined store lists nothing (empty={empty}, n={n}): the decline is \
                     the verdict, and orphan lines would describe one nobody gave"
                );
            }
        }
        // Empty store: the verdict names reason #1 inline, so the listing resumes after it
        // and each problem is printed exactly once.
        assert_eq!(problem_listing_start(false, true, 0), None);
        assert_eq!(problem_listing_start(false, true, 1), Some(1));
        assert_eq!(problem_listing_start(false, true, 5), Some(1));
        // Populated store: the verdict names only a count, so all of them list.
        assert_eq!(problem_listing_start(false, false, 0), None);
        assert_eq!(problem_listing_start(false, false, 1), Some(0));
        assert_eq!(problem_listing_start(false, false, 5), Some(0));
    }

    /// …and it must report ALL of them. The scan used to `.find()`, so a second failing
    /// program stayed invisible until the first was repaired — and because `status.toml` is
    /// a `BTreeMap`, which failure you were shown was alphabetical accident.
    #[test]
    fn every_recorded_problem_is_reported_not_just_the_first() {
        let l = layout("multi-problem");
        install(&l, "ay", 18);
        let existing = crate::status::read(&l).unwrap_or_default();
        let mut programs = existing.programs;
        for (name, state) in [
            ("ay", "active"),
            ("clean", "error: no build for this Mac"),
            ("trust", "blocked: disk full"),
            ("ty", "aborted: activate"),
        ] {
            programs.insert(
                name.to_string(),
                crate::ProgramStatus {
                    installed_build: None,
                    state: state.into(),
                    tree_root: String::new(),
                },
            );
        }
        crate::status::write(
            &l,
            &crate::Status {
                programs,
                ..existing
            },
        )
        .unwrap();

        let home = synthetic_home("multi-problem");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        assert!(
            !run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "three recorded faults must fail the health verdict"
        );

        // THE ASSERTION THAT PROVES THE FIX. Against the real collector, not a
        // re-implementation of it: the old `.find()` could return at most one of these, and
        // `clean` — alphabetically first — is the one it would have returned, leaving
        // `trust` and `ty` unseen.
        let status = crate::status::read(&l).expect("status present");
        assert_eq!(
            recorded_problems(Some(&status)),
            vec![
                "clean: error: no build for this Mac".to_string(),
                "trust: blocked: disk full".to_string(),
                "ty: aborted: activate".to_string(),
            ],
            "all three faults are reported — one error, one blocked, one aborted"
        );
        // The healthy member is not swept up in the reporting.
        assert!(
            !recorded_problems(Some(&status))
                .iter()
                .any(|p| p.starts_with("ay:")),
            "an active program is not a problem"
        );

        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// …unless the emptiness was ASKED FOR. A user who removed the toolset is not
    /// broken, and telling them so would train them to ignore the diagnostic.
    #[test]
    fn a_declined_store_is_healthy_while_empty() {
        let l = layout("declined-store");
        let home = synthetic_home("declined-store");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        std::fs::create_dir_all(&l.prefix).unwrap();
        std::fs::write(l.declined(), b"# removed on purpose\n").unwrap();
        assert!(
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a deliberate removal is a healthy state, not a fault"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn broken_bin_symlink_fails() {
        let l = layout("broken");
        install(&l, "ay", 18);
        // Add a shim pointing at a nonexistent target (a dangling symlink on Unix, a `.cmd`
        // forwarding to a missing exe on Windows) via the same primitive a real install uses.
        let ghost = tool("ghost");
        crate::platform::install_shim(&l.build_dir("ay", 99).join("bin"), &ghost, &l.shim(&ghost))
            .unwrap();
        let home = synthetic_home("broken");
        assert!(
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a dangling bin symlink is structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A SHIM WE COULD NOT STAT IS NOT A BROKEN SHIM.
    ///
    /// Check (4) asked `Path::exists()`, which is `fs::metadata(..).is_ok()` and so
    /// answers `false` for a target that is THERE and merely unreadable — EACCES on a
    /// parent directory, EPERM from macOS privacy consent, EIO. doctor then printed
    /// `FAIL — broken bin shim` and exited non-zero about a shim that works, and
    /// `tools/install.sh` hands that exit code to every failed-seed installer as THE
    /// diagnostic to trust. The remedy it implies — reinstall — repairs nothing.
    ///
    /// Nothing about the store changes here; only the permission to LOOK at it. The
    /// health check must not invent a fault out of its own blindness — and it must not
    /// silently pass either, so the Unknown is REPORTED, as a warn that names the cause.
    #[cfg(unix)]
    #[test]
    fn a_shim_whose_target_cannot_be_stat_ed_is_not_reported_broken() {
        use std::os::unix::fs::PermissionsExt as _;
        if crate::platform::our_uid() == 0 {
            return; // root reads through mode 0; the case does not exist.
        }
        let l = layout("unstattable-shim");
        install(&l, "ay", 18);
        let home = synthetic_home("unstattable-shim");
        let holder = l.build_dir("ay", 18).join("bin");
        let saved = std::fs::metadata(&holder).unwrap().permissions();
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
        let target = crate::platform::resolve_shim(&l.shim(&tool("ay"))).unwrap();
        let armed = std::fs::metadata(&target).is_err();
        let mut err = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            None,
            0,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut err,
        );
        std::fs::set_permissions(&holder, saved).unwrap();
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
        assert!(
            armed,
            "the fixture must make the stat fail, or it proves nothing"
        );
        let err = String::from_utf8_lossy(&err).into_owned();
        assert!(
            !err.contains("broken bin shim"),
            "a shim we could not look at was condemned as broken: {err}"
        );
    }

    /// AND IT MUST SAY SO. The sibling above pins that blindness is not a FAULT; this
    /// one pins that it is not SILENCE either. Dropping the unstattable shim on the
    /// floor would make check (4) report a clean bill it never observed — the same
    /// defect as the FAIL, pointed the other way — and this is the check a user runs
    /// precisely when something is wrong. The line names the shim, the target and the
    /// errno, so the reader can tell a permission problem from a missing file without
    /// reading source.
    #[cfg(unix)]
    #[test]
    fn doctor_says_when_it_could_not_check_a_shim() {
        use std::os::unix::fs::PermissionsExt as _;
        if crate::platform::our_uid() == 0 {
            return; // root reads through mode 0; the case does not exist.
        }
        let l = layout("unchecked-shim");
        install(&l, "ay", 18);
        let home = synthetic_home("unchecked-shim");
        let holder = l.build_dir("ay", 18).join("bin");
        let saved = std::fs::metadata(&holder).unwrap().permissions();
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
        let target = crate::platform::resolve_shim(&l.shim(&tool("ay"))).unwrap();
        let armed = std::fs::metadata(&target).is_err();
        let mut out = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            None,
            0,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink(),
        );
        std::fs::set_permissions(&holder, saved).unwrap();
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
        assert!(
            armed,
            "the fixture must make the stat fail, or it proves nothing"
        );
        let out = String::from_utf8_lossy(&out).into_owned();
        assert!(
            out.contains("could not be checked"),
            "a shim the scan could not look at must be reported, not skipped: {out}"
        );
    }

    #[test]
    fn active_build_with_missing_store_fails() {
        let l = layout("missing-store");
        install(&l, "ay", 18);
        // The shim resolves, but the completeness marker is gone (check 5 vs check 4).
        crate::store::discard_build(&l.build_dir("ay", 18));
        // Re-create just the bin so the shim isn't dangling (isolate check 5 from check 4).
        std::fs::create_dir_all(l.build_dir("ay", 18).join("bin")).unwrap();
        std::fs::write(
            l.build_dir("ay", 18)
                .join("bin")
                .join(tool("ay").exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        assert!(!crate::store::build_is_complete(&l.build_dir("ay", 18)));
        let home = synthetic_home("missing-store");
        assert!(
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "an incomplete active build is structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn stray_posix_sh_in_shell_d_fails() {
        let l = layout("stray-sh");
        install(&l, "ay", 18);
        let home = synthetic_home("stray-sh");
        let shell_d = home.join(".aterm/shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        std::fs::write(shell_d.join("00-atpkg.sh"), b"echo stray\n").unwrap();
        assert!(
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a fish-breaking stray .sh is structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Doctor says plainly which rc files reach the managed `bin/`, in the pass's own five
    /// states: wired, sourced through a line that is not atpkg's, opted out, consent-fenced,
    /// or not yet wired. When none reaches, it names every file it read rather than calling
    /// the machine aterm-only.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_is_reported_in_all_five_states() {
        let l = layout("rc-wiring");
        install(&l, "ay", 18);
        let home = synthetic_home("rc-wiring");
        let shell_d = home.join(".aterm/shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        let zshrc = home.join(".zshrc");
        let report = |home: &Path| {
            let mut out = Vec::new();
            let _ = run_with(
                &l,
                Some(home),
                None,
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut std::io::sink(),
            );
            String::from_utf8(out).unwrap()
        };

        // No rc at all: nothing to wire, and the report must say the consequence — about
        // the files it read, named, never about the machine as a whole.
        let none = report(&home);
        assert!(
            none.contains(
                "none of ~/.zshrc, ~/.bashrc, ~/.bash_profile, ~/.config/fish/config.fish, \
                 ~/.bash_login, ~/.profile sources ~/.aterm/shell.d"
            ) && none.contains("`aterm <tool>`"),
            "no rc ⇒ the six files read are named, with the way in: {none}"
        );
        assert!(
            !none.contains("aterm-only"),
            "six files read are not the whole machine: {none}"
        );

        // An rc that exists but was never wired.
        std::fs::write(&zshrc, "export FOO=1\n").unwrap();
        let unwired = report(&home);
        assert!(
            unwired.contains("~/.zshrc does not source ~/.aterm/shell.d")
                && unwired.contains("`aterm pkg repair` appends atpkg's block"),
            "unwired names the pass that wires it: {unwired}"
        );

        // Wired by a real pass, so the ledger entry the opt-out state needs is the one the
        // pass writes, not a shape this test invented.
        let pass = crate::hooks::pass_at(&l, &home, crate::hooks::RcWiring::HonorOptOut);
        assert_eq!(
            pass.rc(),
            [(".zshrc", crate::hooks::RcOutcome::Appended)],
            "the pass wired ~/.zshrc"
        );
        let wired = report(&home);
        assert!(
            wired.contains("ok — ~/.zshrc sources ~/.aterm/shell.d")
                && wired.contains(
                    "delete the block to opt out — it stays deleted until `aterm pkg repair`"
                ),
            "wired names the rc, the one-motion opt-out, and the one verb that undoes it: \
             {wired}"
        );
        assert!(
            !wired.contains("none of ~/"),
            "a wired rc reaches the toolchain: {wired}"
        );

        // Opted out: recorded as wired, block deleted.
        std::fs::write(&zshrc, "export FOO=1\n").unwrap();
        let opted = report(&home);
        assert!(
            opted.contains("~/.zshrc: atpkg's block was deleted — opted out")
                && opted.contains("aterm pkg repair"),
            "opted-out is honoured and names the one verb that re-wires: {opted}"
        );

        // Sourced by a line that is not atpkg's — tools/install.sh's `wire_shell_path`
        // marker pair, sourcing the same hook — on top of the opt-out ledger entry above.
        // The toolchain reaches this shell, so doctor must not call the rc unwired.
        std::fs::write(
            &zshrc,
            format!(
                "export FOO=1\n# >>> aterm ALab toolset (managed by install.sh) >>>\n\
                 if [ -f \"{h}\" ]; then . \"{h}\"; fi\n# <<< aterm ALab toolset <<<\n",
                h = shell_d.join("00-atpkg.zsh").display()
            ),
        )
        .unwrap();
        let foreign = report(&home);
        assert!(
            foreign.contains(
                "ok — ~/.zshrc sources ~/.aterm/shell.d via a line that is not atpkg's block"
            ),
            "install.sh's block is reported as what it is: {foreign}"
        );
        assert!(
            !foreign.contains("none of ~/")
                && !foreign.contains("does not source")
                && !foreign.contains("opted out"),
            "a shell that reaches the toolchain is neither unreached nor unwired: {foreign}"
        );

        // Consent-fenced: the rc resolves under a folder macOS guards, so no pass opens it,
        // and neither does this report. Without the state nothing tells the user why their
        // rc is perpetually untouched.
        let docs = home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("zshrc"), "export FOO=1\n").unwrap();
        std::fs::remove_file(&zshrc).unwrap();
        std::os::unix::fs::symlink(docs.join("zshrc"), &zshrc).unwrap();
        let fenced = report(&home);
        assert!(
            fenced.contains(
                "~/.zshrc resolves under a folder macOS guards with a consent \
                 dialog, so no atpkg pass opens it"
            ),
            "the fenced rc is named, with why: {fenced}"
        );
        assert!(
            !fenced.contains("does not source"),
            "doctor did not read it, so it claims nothing about its contents: {fenced}"
        );
        assert_eq!(
            crate::hooks::rc_wiring(&home),
            vec![(".zshrc", crate::hooks::RcState::ConsentFenced)]
        );

        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// tools/install.sh wires macOS bash through a login profile: `path_block_rc_target`
    /// elects ~/.bash_profile, then ~/.bash_login, then ~/.profile, never ~/.bashrc, because
    /// Terminal.app starts a login bash. atpkg has a row for the first of those three only,
    /// so a Mac that fell through to ~/.bash_login would read as "nothing sources
    /// ~/.aterm/shell.d" while it plainly did. Pinned in both shapes — content is read before
    /// the ledger — and a profile that does not source shell.d is not listed at all.
    #[cfg(unix)]
    #[test]
    fn an_install_sh_block_in_a_login_profile_is_reported_as_sourcing_shell_d() {
        let l = layout("rc-login-profile");
        install(&l, "ay", 18);
        let home = synthetic_home("rc-login-profile");
        let shell_d = home.join(".aterm/shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        // install.sh's block, in the shape `wire_shell_path` writes it for bash.
        let block = |hook: &str| {
            format!(
                "export FOO=1\n\n# >>> aterm ALab toolset (managed by install.sh) >>>\n\
                 if [ -f \"{h}\" ]; then . \"{h}\"; fi\n# <<< aterm ALab toolset <<<\n",
                h = shell_d.join(hook).display()
            )
        };
        let report = |home: &Path| {
            let mut out = Vec::new();
            let _ = run_with(
                &l,
                Some(home),
                None,
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut std::io::sink(),
            );
            String::from_utf8(out).unwrap()
        };

        // An atpkg row carrying install.sh's block, not atpkg's.
        std::fs::write(home.join(".bash_profile"), block("00-atpkg.bash")).unwrap();
        // A login profile that does not source shell.d is not reported.
        std::fs::write(home.join(".profile"), "export BAR=1\n").unwrap();
        let out = report(&home);
        assert!(
            out.contains(
                "ok — ~/.bash_profile sources ~/.aterm/shell.d via a line that is not \
                 atpkg's block"
            ),
            "install.sh's block in an atpkg row is seen for what it is: {out}"
        );
        assert!(
            !out.contains("none of ~/") && !out.contains("aterm-only"),
            "a machine whose login bash reaches the toolchain is not reported unreached: \
             {out}"
        );
        assert!(
            !out.contains("~/.profile"),
            "a profile atpkg does not wire and that does not source shell.d is not listed: \
             {out}"
        );
        assert_eq!(
            crate::hooks::rc_wiring(&home),
            vec![(".bash_profile", crate::hooks::RcState::SourcedElsewhere)]
        );

        // The fall-through profile atpkg has no row for: the same block, the same answer.
        std::fs::remove_file(home.join(".bash_profile")).unwrap();
        std::fs::write(home.join(".bash_login"), block("00-atpkg.bash")).unwrap();
        let out = report(&home);
        assert!(
            out.contains(
                "ok — ~/.bash_login sources ~/.aterm/shell.d via a line that is not \
                 atpkg's block"
            ),
            "an install.sh-only profile is read, and reported: {out}"
        );
        assert!(
            !out.contains("none of ~/"),
            "it reaches the toolchain: {out}"
        );
        assert_eq!(
            crate::hooks::rc_wiring(&home),
            vec![(".bash_login", crate::hooks::RcState::SourcedElsewhere)]
        );

        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A shim and the channel naming different builds is STRUCTURAL: something on PATH is
    /// running a build the channel does not select, and `gc` has stopped reclaiming that
    /// program entirely. Doctor is the only place that says so.
    #[test]
    fn a_shim_disagreeing_with_the_channel_is_structural() {
        let l = layout("witness-mismatch");
        install(&l, "ay", 19); // channel + shim both at 19
        // Stage 18 COMPLETE on disk (so check 5 stays quiet) and re-point ONLY the shim.
        let older = install_build_tree(&l, "ay", 18);
        crate::store::mark_build_ready(&older).unwrap();
        let ay = tool("ay");
        crate::platform::install_shim(&older.join("bin"), &ay, &l.shim(&ay)).unwrap();
        let home = synthetic_home("witness-mismatch");
        assert!(
            !run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "channel says 19, shims say 18 — structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A program with no channel witness is not breakage — nothing is broken, gc just
    /// abstains — so it warns and stays exit-0. It must still be SAID: the whole cost of
    /// abstaining is that it is otherwise invisible.
    #[test]
    fn a_program_with_no_channel_witness_warns_but_exit_zero() {
        let l = layout("witness-absent");
        install_build_tree(&l, "ay", 18);
        let dir = l.build_dir("ay", 18);
        install_shims(&l, &dir, &["ay".to_string()], crate::activate::Aliases::Off).unwrap(); // shimmed, never activated
        crate::store::mark_build_ready(&dir).unwrap();
        let home = synthetic_home("witness-absent");
        assert!(
            run_with(
                &l,
                Some(&home),
                None,
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "an un-witnessed program is advisory, not structural"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn bin_not_on_path_warns_but_exit_zero() {
        let l = layout("notonpath");
        install(&l, "ay", 18);
        let home = synthetic_home("notonpath");
        // PATH without the managed bin/ → a warning, not a structural fail.
        let path = std::ffi::OsString::from("/usr/bin:/bin");
        assert!(
            run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut std::io::sink(),
                &mut std::io::sink()
            ),
            "a PATH warning stays exit-0"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A `satisfied by system` row is an OK line while the system binary is there, a WARN
    /// (never a PROBLEM) once it is gone — and it is never counted as a recorded fault.
    #[test]
    fn a_system_satisfied_member_is_reported_and_never_a_fault() {
        let l = layout("system-satisfied");
        install(&l, "ay", 19);
        let exe = l.prefix.join("fake-system-gh");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        let existing = crate::status::read(&l).unwrap_or_default();
        let mut programs = existing.programs.clone();
        programs.insert(
            "gh".into(),
            crate::ProgramStatus {
                installed_build: None,
                state: crate::state::system(&exe, Some("2026-08-27")),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                schema: 1,
                updated_at: "2026-08-27T00:00:00Z".into(),
                enabled: true,
                index_source: "alabsystems/aterm".into(),
                outcome: "up to date".into(),
                seams: Vec::new(),
                last_success_at: String::new(),
                last_index_reached_at: String::new(),
                last_index_build: 0,
                index_build_changed_at: String::new(),
                programs,
            },
        )
        .unwrap();
        let home = synthetic_home("system-satisfied");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = |l: &Layout| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let (ok, out) = run(&l);
        assert!(ok, "a satisfied member is not a problem:\n{out}");
        // The SAME words the pass wrote — the canonical state, retirement note included.
        assert!(
            out.contains(&format!(
                "doctor: ok — gh: system: {} — not managed by aterm (managed copy retired \
                 2026-08-27)",
                exe.display()
            )),
            "{out}"
        );
        assert!(recorded_problems(crate::status::read(&l).as_ref()).is_empty());
        // The system binary goes away: a warning naming the remedy, still not a problem.
        std::fs::remove_file(&exe).unwrap();
        let (ok, out) = run(&l);
        assert!(ok, "{out}");
        assert!(
            out.contains("doctor: warn — gh: recorded as `system: ")
                && out.contains("system copy gone: the next `aterm pkg update` reinstalls"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// An `installed via <protocol>: <path>` row (Homebrew's pkg, the Command Line
    /// Tools) is an OK line while its `provides` path is there — the words the pass
    /// wrote — and a WARN naming the door once it is gone; `needs admin` is a WARN with
    /// the door's spelling. Neither is ever a recorded fault.
    #[test]
    fn an_os_installed_member_is_reported_by_its_provides_path_and_never_a_fault() {
        let l = layout("installed-via");
        install(&l, "ay", 19);
        let brew = l.prefix.join("fake-opt-homebrew-bin-brew");
        std::fs::write(&brew, b"#!/bin/sh\nexit 0\n").unwrap();
        let existing = crate::status::read(&l).unwrap_or_default();
        let mut programs = existing.programs.clone();
        programs.insert(
            "brew".into(),
            crate::ProgramStatus {
                installed_build: None,
                state: crate::state::installed_via("pkg", &brew),
                tree_root: String::new(),
            },
        );
        programs.insert(
            "clt".into(),
            crate::ProgramStatus {
                installed_build: None,
                state: crate::state::needs_admin("clt"),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                schema: 1,
                updated_at: "2026-08-27T00:00:00Z".into(),
                enabled: true,
                index_source: "alabsystems/aterm".into(),
                outcome: "up to date".into(),
                seams: Vec::new(),
                last_success_at: String::new(),
                last_index_reached_at: String::new(),
                last_index_build: 0,
                index_build_changed_at: String::new(),
                programs,
            },
        )
        .unwrap();
        let home = synthetic_home("installed-via");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = |l: &Layout| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let (ok, out) = run(&l);
        assert!(ok, "an OS-installed member is not a problem:\n{out}");
        assert!(
            out.contains(&format!(
                "doctor: ok — brew: installed via pkg: {} (kept current by pkg, not by aterm)",
                brew.display()
            )),
            "{out}"
        );
        assert!(
            out.contains(
                "doctor: warn — clt: needs admin — run: aterm pkg install clt (in a terminal; \
                 sudo asks there)"
            ),
            "{out}"
        );
        assert!(recorded_problems(crate::status::read(&l).as_ref()).is_empty());
        // The provides path goes away: a warning naming the door, still not a problem.
        std::fs::remove_file(&brew).unwrap();
        let (ok, out) = run(&l);
        assert!(ok, "{out}");
        assert!(
            out.contains("doctor: warn — brew: recorded as `installed via pkg: ")
                && out.contains("reinstall: aterm pkg install brew"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// SHADOWED (design S5): a foreign copy of a managed tool AHEAD of the managed bin/ on
    /// PATH is a WARN line in the canonical words — never a fault, never touched — and a
    /// copy BEHIND the managed bin/ is not mentioned at all.
    #[cfg(unix)]
    /// A member BLOCKED by a requirement (§17.10) is reported in its own words as a
    /// `warn` naming the dependency and the door that installs both — and never counted
    /// as a fault: `doctor` stays healthy over it.
    #[test]
    fn a_blocked_member_is_a_warning_naming_its_dependency_never_a_fault() {
        let layout = layout("doctor-blocked");
        install(&layout, "ay", 18);
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            crate::ProgramStatus {
                installed_build: Some(18),
                state: crate::state::managed(18, 41),
                tree_root: String::new(),
            },
        );
        let blocked = crate::state::blocked("clt", &crate::state::needs_admin("clt"));
        programs.insert(
            "brew".to_string(),
            crate::ProgramStatus {
                installed_build: None,
                state: blocked.clone(),
                tree_root: String::new(),
            },
        );
        let status = crate::Status {
            schema: 1,
            updated_at: "2026-08-27T00:00:00Z".into(),
            enabled: true,
            index_source: "x/y".into(),
            outcome: "up to date".into(),
            seams: Vec::new(),
            last_success_at: String::new(),
            last_index_reached_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            programs,
        };
        crate::status::write(&layout, &status).unwrap();
        assert!(
            recorded_problems(Some(&status)).is_empty(),
            "a blocked row is deferred, not a fault"
        );
        let home = synthetic_home("doctor-blocked");
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let path = std::env::join_paths([layout.bin_dir()]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &layout,
            Some(&home),
            Some(&path),
            now,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            ok,
            "a blocked row is a warning, not a structural fault:
{text}"
        );
        assert!(
            text.contains(&format!("warn — brew: {blocked} (installs once clt is;")),
            "{text}"
        );
        assert!(text.contains("aterm pkg install brew"), "{text}");
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A member HELD on its current build (its coherence group's new pin is not
    /// published for this target) is reported as `ok`, quoting its row, on the
    /// owner's row and on a sibling's — never a fault, and never silent: an Intel
    /// Mac whose trust-cg/-ir/-vc rows still read `pinned by index 15` beside a
    /// held trust had no line saying why (2026-09-15).
    #[test]
    fn a_held_member_is_an_ok_line_quoting_its_row_never_a_fault() {
        let layout = layout("doctor-held");
        install(&layout, "trust", 6808);
        install(&layout, "trust-cg", 3095);
        let triple = "x86_64-apple-darwin";
        let mut programs = std::collections::BTreeMap::new();
        let own = crate::state::held_unpublished(None, 8595, triple, 6808);
        let sibling = crate::state::held_unpublished(Some("trust"), 8595, triple, 3095);
        for (p, build, state) in [("trust", 6808, &own), ("trust-cg", 3095, &sibling)] {
            programs.insert(
                p.to_string(),
                crate::ProgramStatus {
                    installed_build: Some(build),
                    state: state.clone(),
                    tree_root: String::new(),
                },
            );
        }
        let status = crate::Status {
            schema: 1,
            updated_at: "2026-09-15T00:00:00Z".into(),
            enabled: true,
            index_source: "x/y".into(),
            outcome: "rustc NOT updated".into(),
            seams: Vec::new(),
            last_success_at: "2026-09-15T00:00:00Z".into(),
            programs,
            ..crate::Status::default()
        };
        crate::status::write(&layout, &status).unwrap();
        assert!(
            recorded_problems(Some(&status)).is_empty(),
            "a held row is deferred, not a fault"
        );
        let home = synthetic_home("doctor-held");
        let now = crate::flow::rfc3339_to_unix("2026-09-15T00:00:00Z").unwrap();
        let path = std::env::join_paths([layout.bin_dir()]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &layout,
            Some(&home),
            Some(&path),
            now,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(ok, "a held tuple is healthy:\n{text}");
        for (p, row) in [("trust", &own), ("trust-cg", &sibling)] {
            assert!(
                text.contains(&format!(
                    "ok — {p}: {row} (nothing to do here; the next pass moves the group once \
                     the index publishes for this target)"
                )),
                "{p}'s held row is said, verbatim:\n{text}"
            );
        }
        assert!(
            !text.contains("PROBLEM"),
            "nothing about a hold is a problem:\n{text}"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn a_shadowed_managed_member_is_a_warning_never_a_fault() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("shadowed");
        install(&l, "ay", 19);
        let foreign = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-doctor-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&foreign);
        std::fs::create_dir_all(&foreign).unwrap();
        let exe = foreign.join("ay");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = synthetic_home("shadowed");
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = |path: &std::ffi::OsStr| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let ahead = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
        let (ok, out) = run(&ahead);
        assert!(ok, "a shadow is a warning, not a structural fault:\n{out}");
        assert!(
            out.contains(&format!(
                "doctor: warn — ay: {}",
                crate::state::shadowed(19, &exe)
            )),
            "{out}"
        );
        assert!(
            exe.exists() && crate::which(&l, "ay").is_some(),
            "never fixed"
        );
        let behind = std::env::join_paths([l.bin_dir(), foreign.clone()]).unwrap();
        let (ok, out) = run(&behind);
        assert!(ok, "{out}");
        assert!(!out.contains("SHADOWED"), "{out}");
        // No alias laid (a vendor-shaped install): no fix-line either.
        let (_, out) = run(&ahead);
        assert!(!out.contains("for the managed one"), "{out}");
        let _ = std::fs::remove_dir_all(&foreign);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE AGENT PROGRAMS' SHADOW WORDING (2026-09-16). For `claude`/`codex` aterm's copy is
    /// what every tab runs (owner decision 2026-09-10), through `agents/` first on PATH;
    /// a foreign copy ahead of it in THIS shell is a shell that has not run the hook, and
    /// the remedy is the hook sourced in place — `. ~/.aterm/shell.d/00-atpkg.zsh` — never
    /// "open a new tab" (owner, 2026-09-16), never `exec $SHELL` (it drops the tab's shell
    /// integration; measured 2026-09-16), never "remove or reorder that copy". A warn,
    /// never a fault; the plain SHADOWED wording returns only when the twin itself is gone.
    #[cfg(unix)]
    #[test]
    fn an_agent_program_shadowed_in_this_shell_names_the_hook_source_never_a_new_tab() {
        let l = layout("agent-shadow");
        // An ALab member too, so the report has a toolset to be healthy about.
        install(&l, "ay", 19);
        install(&l, "claude", 2_026_091_601);
        let twin = l.agent_shim(&tool("claude"));
        assert!(twin.exists(), "install lays the agents/ twin");
        let foreign = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-doctor-agent-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&foreign);
        std::fs::create_dir_all(&foreign).unwrap();
        let exe = foreign.join("claude");
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = synthetic_home("agent-shadow");
        let now = crate::flow::rfc3339_to_unix("2026-09-16T00:00:00Z").unwrap();
        let run = |path: &std::ffi::OsStr| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        // A shell from before the install: no agents/ on its PATH.
        let no_agents = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
        let (ok, out) = run(&no_agents);
        assert!(ok, "a warning, never a fault:\n{out}");
        assert!(
            out.contains(&format!(
                "doctor: warn — claude: {}",
                crate::state::agent_shadowed_in_shell(
                    2_026_091_601,
                    &exe,
                    &l.agents_dir(),
                    false,
                    &crate::cli::shell_remedy_command(&l),
                )
            )),
            "{out}"
        );
        assert!(
            !out.contains("new tab")
                && !out.contains("exec $SHELL")
                && !out.contains("remove or reorder")
                && !out.contains("not the pinned build"),
            "the in-place remedy only: {out}"
        );
        // agents/ present but behind the foreign copy: named as such.
        let behind = std::env::join_paths([foreign.clone(), l.bin_dir(), l.agents_dir()]).unwrap();
        let (ok, out) = run(&behind);
        assert!(ok, "{out}");
        assert!(
            out.contains(&crate::state::agent_shadowed_in_shell(
                2_026_091_601,
                &exe,
                &l.agents_dir(),
                true,
                &crate::cli::shell_remedy_command(&l),
            )),
            "{out}"
        );
        // agents/ first (every aterm tab): nothing is shadowed.
        let first = std::env::join_paths([l.agents_dir(), foreign.clone(), l.bin_dir()]).unwrap();
        let (ok, out) = run(&first);
        assert!(ok, "{out}");
        assert!(!out.contains("SHADOWED"), "{out}");
        // The twin gone (a lane that could not lay it): the machine-wide wording is the
        // truth again, unchanged from every other program's.
        std::fs::remove_file(&twin).unwrap();
        let (ok, out) = run(&no_agents);
        assert!(ok, "{out}");
        assert!(
            out.contains(&format!(
                "doctor: warn — claude: {} (not the pinned build",
                crate::state::shadowed(2_026_091_601, &exe)
            )),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&foreign);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// DESIGN S7 on the `doctor` surface: a managed member whose shim exports its
    /// manifest's `shim_env` gets ONE `ok` line — the canonical row (the recorded one when
    /// it is managed, else derived) and the trailing `self-update off (…)` sentence —
    /// never a fault, never inside the state; a plain shim gets no such line, and a
    /// shadowed one keeps (10d)'s warn alone (the env never reaches the system copy).
    #[cfg(unix)]
    #[test]
    fn a_managed_member_with_a_shim_env_says_self_update_off_as_a_trailing_line() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("shim-env");
        let dir = install_build_tree(&l, "claude", 2026082701);
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        crate::activate::install_tools_env(
            &l,
            &dir,
            &[tool("claude")],
            crate::activate::Aliases::Off,
            &env,
        )
        .unwrap();
        activate_channel(&l, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "claude".to_string(),
            crate::ProgramStatus {
                installed_build: Some(2026082701),
                state: crate::state::managed(2026082701, 41),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                schema: 1,
                updated_at: "2026-08-28T00:00:00Z".into(),
                enabled: true,
                index_source: "alabsystems/aterm".into(),
                outcome: "up to date".into(),
                seams: Vec::new(),
                last_success_at: String::new(),
                last_index_reached_at: String::new(),
                last_index_build: 0,
                index_build_changed_at: String::new(),
                programs,
            },
        )
        .unwrap();
        let home = synthetic_home("shim-env");
        let now = crate::flow::rfc3339_to_unix("2026-08-28T00:00:00Z").unwrap();
        let run = |path: &std::ffi::OsStr| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };
        let managed_only = std::env::join_paths([l.bin_dir()]).unwrap();
        let (ok, out) = run(&managed_only);
        assert!(ok, "{out}");
        let line = out
            .lines()
            .find(|l| l.contains("ok — claude:"))
            .unwrap_or_else(|| panic!("an ok line for claude:\n{out}"));
        assert_eq!(
            line,
            "doctor: ok — claude: managed 2026082701 — pinned by index 41 — self-update off \
             (DISABLE_AUTOUPDATER=1) (updates arrive with the ALab index)"
        );
        // Shadowed: the warn alone — the foreign copy runs, and runs without the env.
        let foreign = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-doctor-env-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&foreign);
        std::fs::create_dir_all(&foreign).unwrap();
        let exe = foreign.join("claude");
        std::fs::write(&exe, b"#!/bin/sh\necho vendor\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ahead = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
        let (ok, out) = run(&ahead);
        assert!(ok, "{out}");
        assert!(out.contains("warn — claude:"), "{out}");
        assert!(!out.contains("self-update"), "{out}");
        // A plain shim: no line about it at all.
        crate::activate::install_tools(&l, &dir, &[tool("claude")], crate::activate::Aliases::Off)
            .unwrap();
        let (ok, out) = run(&managed_only);
        assert!(ok, "{out}");
        assert!(!out.contains("self-update"), "{out}");
        let _ = std::fs::remove_dir_all(&foreign);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE p11-kit STORY (§17.11): a Homebrew-style `trust` ahead of the managed bin/
    /// shadows ALab's `trust`; the warn line keeps the canonical state and gains ONE
    /// trailing sentence naming the alias that runs the managed copy — because the alias
    /// is laid, and only because of that.
    #[cfg(unix)]
    #[test]
    fn a_shadowed_alab_tool_names_its_alias_as_the_way_to_the_managed_copy() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("shadowed-alias");
        let dir = install_build_tree(&l, "trust", 6808);
        install_shims(
            &l,
            &dir,
            &["trust".to_string()],
            crate::activate::Aliases::Alab,
        )
        .unwrap();
        activate_channel(&l, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        let homebrew = l
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-doctor-homebrew-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&homebrew);
        std::fs::create_dir_all(&homebrew).unwrap();
        let exe = homebrew.join("trust");
        std::fs::write(&exe, b"#!/bin/sh\necho p11-kit\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = synthetic_home("shadowed-alias");
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let ahead = std::env::join_paths([homebrew.clone(), l.bin_dir()]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(&ahead),
            now,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        let out = String::from_utf8_lossy(&out).into_owned();
        assert!(ok, "a shadow is a warning, not a structural fault:\n{out}");
        let state = crate::state::shadowed(6808, &exe);
        let line = out
            .lines()
            .find(|l| l.contains("warn — trust:"))
            .unwrap_or_else(|| panic!("a warn line for trust:\n{out}"));
        assert!(
            line.contains(&format!("doctor: warn — trust: {state} (")),
            "{line}"
        );
        assert!(
            line.ends_with(" — type alab-trust for the managed one"),
            "the fix-line is the trailing sentence: {line}"
        );
        // And the alias really is what runs the managed copy.
        assert!(
            crate::which(&l, "alab-trust").is_some_and(|t| t.starts_with(&dir)),
            "alab-trust resolves into the managed build"
        );
        let _ = std::fs::remove_dir_all(&homebrew);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// TWO NAMES, ONE REPORT, AND THE SPEAKER IS THE NAME YOU TYPED. `status` used to
    /// answer every line as "doctor:" — a user could not tell which verb they had run,
    /// and a script keying on the prefix keyed on the wrong verb.
    #[test]
    fn status_never_speaks_as_doctor() {
        let l = layout("speaker");
        install(&l, "ay", 18);
        let home = synthetic_home("speaker");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        for (invoked, other) in [("status", "doctor:"), ("doctor", "status:")] {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let _ = run_with(
                &l,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                invoked,
                &Probes::default(),
                &mut out,
                &mut err,
            );
            for stream in [&out, &err] {
                let text = String::from_utf8_lossy(stream);
                assert!(
                    !text.lines().any(|line| line.starts_with(other)),
                    "invoked as {invoked}, no line may speak as {other}:\n{text}"
                );
                assert!(
                    text.lines()
                        .all(|line| line.is_empty() || line.starts_with(invoked)),
                    "every line is signed by the verb the user typed ({invoked}):\n{text}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// AN UNHEALTHY REPORT ENDS IN ONE ACT. A fresh machine's only story is "nothing
    /// installed yet"; the tail must name the one command that fixes it — and exactly
    /// one, after the byte-stable "found N problem(s)" line, never instead of it.
    #[test]
    fn an_unhealthy_report_names_one_next_act() {
        let l = layout("next-act");
        let home = synthetic_home("next-act");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        assert!(!run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err
        ));
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("doctor: found 1 problem(s)"),
            "the verdict line keeps its bytes:\n{text}"
        );
        let next: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("doctor: next — "))
            .collect();
        assert_eq!(
            next,
            vec!["doctor: next — aterm pkg install --default-set"],
            "exactly one next act, and it is the whole-set install:\n{text}"
        );
        assert!(
            text.contains("Fix: aterm pkg install"),
            "the PROBLEM line states the remedy as what it is:\n{text}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// ATPKG NEVER TELLS THE USER TO RESTART FOR AN APP UPDATE. A staged build is applied
    /// in-session by the app's own overlap handoff (automatic by default, one click
    /// otherwise) and the shells keep running, so the posture note names that lane and the
    /// `aterm ctl update` verbs — never a reopen. Both note arms are pinned (a newer build
    /// staged; a newer bundle already on disk), plus the quiet case, against the words that
    /// stood here until 2026-08-30.
    #[test]
    fn aterm_posture_never_prompts_a_restart() {
        let base = layout("posture");
        // The updater's ledger lives BESIDE the pkg prefix (`<support>/Updates`), so the
        // layout under test is `<scratch>/pkg` and the ledger goes in `<scratch>/Updates`.
        let lay = Layout {
            prefix: base.prefix.join("pkg"),
        };
        std::fs::create_dir_all(&lay.prefix).unwrap();
        let updates = base.prefix.join("Updates");
        std::fs::create_dir_all(&updates).unwrap();
        let forbidden = ["restart", "relaunch", "reopen"];
        let report = |lay: &Layout| -> String {
            let mut out = Vec::new();
            report_aterm_posture(lay, "atpkg", &mut out);
            String::from_utf8(out).unwrap()
        };

        // A strictly-newer build staged while an older one runs: the in-session apply lane.
        std::fs::write(
            updates.join("installed.toml"),
            "build_number = 1788035619\n",
        )
        .unwrap();
        std::fs::write(
            updates.join("status.toml"),
            "current_build = 1788035619\nstaged_build = 1788077184\n",
        )
        .unwrap();
        let text = report(&lay);
        assert!(text.starts_with("atpkg: note — "), "{text}");
        for want in [
            "in-session",
            "staged",
            "1788077184",
            "in place — your shells keep running",
            "aterm ctl update status",
            "aterm ctl update apply",
        ] {
            assert!(text.contains(want), "{want:?} missing from: {text}");
        }
        let lower = text.to_lowercase();
        for w in forbidden {
            assert!(!lower.contains(w), "{w:?} must never appear: {text}");
        }

        // A newer bundle already on disk (the running process predates it): activation,
        // still in-session and still no reopen.
        std::fs::write(
            updates.join("installed.toml"),
            "build_number = 1788077184\n",
        )
        .unwrap();
        std::fs::write(updates.join("status.toml"), "current_build = 1788035619\n").unwrap();
        let text = report(&lay);
        assert!(text.contains("activates it in-session"), "{text}");
        assert!(
            text.contains("in place — your shells keep running"),
            "{text}"
        );
        let lower = text.to_lowercase();
        for w in forbidden {
            assert!(!lower.contains(w), "{w:?} must never appear: {text}");
        }

        // Running what is installed, nothing staged: the plain ok line, no note.
        std::fs::write(updates.join("status.toml"), "current_build = 1788077184\n").unwrap();
        let text = report(&lay);
        assert_eq!(text, "atpkg: ok — aterm build 1788077184 installed\n");

        let _ = std::fs::remove_dir_all(&base.prefix);
    }

    /// THE HAND-PLACED BUNDLE (m21, 2026-09-10 audit): the updater's receipt says build
    /// 1786405661 (its last own install, Aug 10) while the bundle on disk — dragged in by
    /// hand — and the running process are both 1789000876. Reading the receipt alone made
    /// doctor print "running build 1789000876 but build 1786405661 is installed on disk",
    /// a note about a DOWNGRADE that does not exist. "On disk" is the newer of receipt and
    /// sealed bundle, and the note fires only when that is strictly newer than what runs.
    #[test]
    fn on_disk_build_is_the_newer_of_receipt_and_bundle_and_the_note_needs_installed_newer_than_running()
     {
        let base = layout("posture-bundle");
        let updates = base.prefix.join("Updates");
        std::fs::create_dir_all(&updates).unwrap();
        let report = |bundle: Option<u64>| -> String {
            let mut out = Vec::new();
            report_aterm_posture_at(&updates, bundle, "atpkg", &mut out);
            String::from_utf8(out).unwrap()
        };
        std::fs::write(
            updates.join("installed.toml"),
            "build_number = 1786405661\n",
        )
        .unwrap();
        std::fs::write(
            updates.join("status.toml"),
            "current_build = 1789000876\nstaged_build = 1789000876\n",
        )
        .unwrap();
        // The m21 shape: receipt older than the running build, bundle == running.
        assert_eq!(
            report(Some(1_789_000_876)),
            "atpkg: ok — aterm build 1789000876 installed\n",
            "on disk is derived from the bundle; no downgrade note"
        );
        // Receipt older, no bundle readable (a source build): the receipt is OLDER than
        // what runs, so still no "installed on disk" note — that note is for NEWER.
        assert_eq!(
            report(None),
            "atpkg: ok — aterm build 1786405661 installed\n"
        );
        // A bundle strictly newer than the running process: the activation note.
        let text = report(Some(1_789_999_999));
        assert!(
            text.contains("running build 1789000876 but build 1789999999 is installed on disk"),
            "{text}"
        );
        assert!(text.contains("activates it in-session"), "{text}");
        // A receipt newer than both (the updater installed, the process predates it):
        // the receipt wins the max and the note fires off it.
        std::fs::write(
            updates.join("installed.toml"),
            "build_number = 1790000000\n",
        )
        .unwrap();
        let text = report(Some(1_789_000_876));
        assert!(
            text.contains("but build 1790000000 is installed on disk"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&base.prefix);
    }

    /// The plist scan binds the value to the key right before it and parses only an
    /// integer build.
    #[test]
    fn plist_bundle_version_reads_only_the_string_bound_to_the_key() {
        assert_eq!(
            plist_bundle_version(
                "<plist><dict><key>CFBundleShortVersionString</key><string>0.79.0</string>\
                 <key>CFBundleVersion</key>\n  <string>1789000876</string></dict></plist>"
            ),
            Some(1_789_000_876)
        );
        assert_eq!(
            plist_bundle_version("<key>CFBundleVersion</key><key>Other</key><string>54</string>"),
            None,
            "an intervening key cannot lend its string"
        );
        assert_eq!(
            plist_bundle_version("<key>CFBundleVersion</key><string>0.79.0</string>"),
            None,
            "not an integer build"
        );
        assert_eq!(plist_bundle_version("<plist/>"), None);
    }
    /// Build a directory `scan` will recognize as cargo output by its `RustcInfo` evidence
    /// — the arm measured off `/Users//example/aterm/target` on 2026-09-02, which had
    /// `.rustc_info.json` and `debug/` and no `CACHEDIR.TAG`.
    #[cfg(target_os = "macos")]
    fn cargo_target(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        std::fs::write(dir.join(".rustc_info.json"), b"{}\n").unwrap();
        std::fs::write(dir.join("debug/libay.rlib"), b"0123456789").unwrap();
        dir
    }

    /// The Spotlight warning says how far the UNATTENDED PASS reaches. The pass renames
    /// only a target dir beside a Cargo.toml (`noindex::apply_under`, `require_repo`); a
    /// free-standing one — an env var's, a build system's — is skipped on every pass, and
    /// until 2026-09-13 this line counted it as "the next update pass migrates" all the
    /// same. The "8 of 44" m3's doctor had reported for a day were eight free-standing
    /// dirs (`$HOME/trust/build/bootstrap`, `$HOME/trust-verify-scratch/*`). The line now splits
    /// the count and NAMES the free ones, in each of the three shapes.
    #[cfg(target_os = "macos")]
    #[test]
    fn spotlight_warning_separates_the_pass_reach_from_free_standing_dirs() {
        let l = layout("reach");
        install(&l, "ay", 19);
        let home = synthetic_home("reach");
        let manifest = "[package]\nname = \"a\"\nversion = \"0.0.0\"\n";
        // Beside a Cargo.toml: the pass's to migrate.
        std::fs::create_dir_all(home.join("repo-a")).unwrap();
        std::fs::write(home.join("repo-a/Cargo.toml"), manifest).unwrap();
        cargo_target(&home.join("repo-a"), "target");
        // Free-standing: nothing beside it names it, so nothing the pass can re-point.
        let free = cargo_target(&home.join("scratch"), "tgt");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = || {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            (ok, String::from_utf8_lossy(&out).into_owned())
        };

        // Mixed: one of each.
        let (ok, out) = run();
        assert!(ok, "{out}");
        assert!(
            out.contains(&format!(
                "doctor: warn — 2 of 2 cargo target dir(s) under {} are indexed by Spotlight",
                home.display()
            )),
            "{out}"
        );
        assert!(
            out.contains(
                "the next update pass migrates the 1 beside a Cargo.toml; the other 1 is \
                 free-standing"
            ),
            "{out}"
        );
        assert!(
            out.contains(&free.display().to_string()),
            "the free-standing dir must be NAMED, not counted: {out}"
        );
        assert!(
            out.contains("`aterm pkg noindex apply <dir>` by name"),
            "the free one has a verb, and it is the by-name one: {out}"
        );
        assert!(
            out.contains("aterm pkg noindex apply --all"),
            "the pass's own verb stays named: {out}"
        );

        // All free: the pass will never move any of them, and the line must say so
        // instead of promising a migration that never comes.
        std::fs::remove_file(home.join("repo-a/Cargo.toml")).unwrap();
        let (ok, out) = run();
        assert!(ok, "{out}");
        assert!(
            out.contains(
                "NONE is beside a Cargo.toml, so the update pass never renames any of them"
            ),
            "{out}"
        );
        assert!(
            out.contains(&free.display().to_string())
                && out.contains(&home.join("repo-a/target").display().to_string()),
            "both free-standing dirs are named: {out}"
        );
        // AND NO REMEDY THAT CANNOT REACH THEM. `--all` walks with `require_repo`, so
        // offering it here — one clause after saying the pass renames none of these —
        // is a guaranteed no-op that leaves the warning standing.
        assert!(
            !out.contains("apply --all"),
            "a remedy that skips every dir named must not be offered: {out}"
        );
        assert!(
            out.contains("`aterm pkg noindex apply <dir>` by name"),
            "the reach clause still names the verb that DOES reach them: {out}"
        );

        // All in repos: no free-standing clause at all.
        std::fs::write(home.join("repo-a/Cargo.toml"), manifest).unwrap();
        std::fs::write(home.join("scratch/Cargo.toml"), manifest).unwrap();
        let (ok, out) = run();
        assert!(ok, "{out}");
        assert!(
            out.contains("the next update pass migrates them (every one is beside a Cargo.toml)"),
            "{out}"
        );
        assert!(!out.contains("free-standing"), "{out}");
        assert!(
            out.contains("now: `aterm pkg machine apply`"),
            "with dirs the pass can reach, the remedy is the pass's own verb: {out}"
        );
        assert!(out.contains("apply --all"), "{out}");
    }

    /// A Spotlight-exposed target dir is a WARNING and exit stays 0.
    ///
    /// The 2026-09-01 01:54 watchdog kill had two amplifiers; `mds` grinding 2.0 TB of
    /// build output was the one a rename removes, so doctor has to say it out loud. But it
    /// is NOT structural — every installed tool still runs — and the scan is a name-based
    /// heuristic over a depth-limited walk, so counting it as a problem would be doctor
    /// inventing a fault from evidence it did not measure. The only thing that measures
    /// exclusion is `noindex verify`, which WRITES a probe file; doctor never mutates, so
    /// doctor never probes — it names the verb instead. This pins all three: the words, the
    /// stream (`out`, never `err`), and the exit code.
    #[cfg(target_os = "macos")]
    #[test]
    fn spotlight_exposed_build_output_warns_but_exit_zero() {
        let l = layout("indexed");
        install(&l, "ay", 19);
        // The label must not contain the word the assertions grep for: several other doctor
        // lines print $HOME, and a home named "spotlight" would satisfy them vacuously.
        let home = synthetic_home("indexed");
        std::fs::create_dir_all(home.join("repo-a")).unwrap();
        std::fs::write(
            home.join("repo-a/Cargo.toml"),
            b"[package]\nname = \"a\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        let exposed = cargo_target(&home.join("repo-a"), "target");
        cargo_target(&home.join("repo-b"), "target.noindex");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let run = || {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let ok = run_with(
                &l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            (
                ok,
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
            )
        };

        let (ok, out, err) = run();
        assert!(
            ok,
            "an indexed target dir is advisory, never structural:\n{out}"
        );
        assert!(
            out.contains(&format!(
                "doctor: warn — 1 of 2 cargo target dir(s) under {} are indexed by Spotlight",
                home.display()
            )),
            "{out}"
        );
        // The remedy names the verb that APPLIES (what the pass runs) and the verb that
        // MEASURES, because doctor itself never probes and never renames.
        for want in [
            "aterm pkg noindex apply --all",
            "[machine] spotlight_noindex",
            "aterm pkg noindex verify <dir>",
            "2026-09-01 WindowServer watchdog kill",
        ] {
            assert!(out.contains(want), "{want:?} missing from:\n{out}");
        }
        // Bytes come from `noindex::human_size` → `cost::human_bytes`, never hand-rolled:
        // cli.rs's source guards reject a `1e9` or a `{:.1} GB` anywhere in production code.
        // 13 B is the whole synthetic tree — `.rustc_info.json` plus one 10-byte artifact.
        assert!(
            out.contains(&format!("({})", crate::cost::human_bytes(13))),
            "the exposed size must be rendered through cost::human_bytes:\n{out}"
        );
        assert!(
            !err.to_lowercase().contains("spotlight"),
            "nothing here is a FAIL, so nothing here belongs on stderr:\n{err}"
        );

        // Once the exposed one is hidden too, the same check says `ok` — and still exit 0.
        std::fs::rename(&exposed, exposed.with_file_name("target.noindex")).unwrap();
        let (ok, out, _) = run();
        assert!(ok, "{out}");
        assert!(
            out.contains(&format!(
                "doctor: ok — 2 cargo target dir(s) under {} are hidden from Spotlight",
                home.display()
            )),
            "{out}"
        );
        // `ok` here is `exclusion_of` reading the NAME — doctor never probes. Saying so is
        // the difference between this line and the 189 inert `.metadata_never_index`
        // markers: the day macOS changes the `.noindex` behaviour, an unqualified `ok` is a
        // confident false success on every run.
        assert!(
            out.contains("that is what the NAMES say")
                && out.contains("aterm pkg noindex verify <dir>"),
            "the ok line must name its provenance and the verb that MEASURES:\n{out}"
        );

        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A home with no cargo output says NOTHING about Spotlight.
    ///
    /// Silence is the answer for a check that does not apply to this machine — the same
    /// discipline as the `rustup` block. A "not applicable" line on every `doctor` run of a
    /// non-Rust Mac is noise that trains people to skim the report, which is the one
    /// failure mode a diagnostic cannot recover from. Portable on purpose: off macOS
    /// `noindex::scan` returns an empty, `complete` scan, so this asserts the no-`cfg`
    /// call site really is a clean no-op there.
    #[test]
    fn a_home_without_build_output_says_nothing_about_spotlight() {
        let l = layout("indexed-quiet");
        install(&l, "ay", 19);
        let home = synthetic_home("indexed-quiet");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        let out = String::from_utf8_lossy(&out).into_owned();
        let err = String::from_utf8_lossy(&err).into_owned();
        assert!(ok, "{out}");
        for text in [&out, &err] {
            assert!(
                !text.to_lowercase().contains("spotlight"),
                "a machine with no build output must hear nothing:\n{text}"
            );
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_workspace_policy_the_installed_targo_refuses_is_a_problem_that_names_the_cure() {
        let l = layout("wspolicy");
        install(&l, "trust", 6808);
        let home = synthetic_home("wspolicy");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let probes = Probes {
            provenance_attr: crate::provenance::PROVENANCE_XATTR,
            workspace: Some(WorkspacePolicyProbe {
                workspace: PathBuf::from("/work/ty"),
                policy: WorkspacePolicy::UnknownField {
                    field: "compiler_timeout_secs".into(),
                },
            }),
            local_seal: None,
        };
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        assert!(!run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            None,
            "doctor",
            &probes,
            &mut out,
            &mut err
        ));
        let err = String::from_utf8(err).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            err.contains("`compiler_timeout_secs`"),
            "names the refused key: {err}"
        );
        assert!(
            err.contains("build 6808"),
            "names the installed build: {err}"
        );
        assert!(err.contains(PUBLISH_RUSTC_GROUP), "names the cure: {err}");
        assert!(
            out.contains("next — publish the newer Trust coherence group"),
            "next act: {out}"
        );
    }

    #[test]
    fn an_accepted_workspace_policy_is_ok_and_a_local_seal_only_warns() {
        let l = layout("wsok");
        install(&l, "trust", 6808);
        let home = synthetic_home("wsok");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let probes = Probes {
            provenance_attr: crate::provenance::PROVENANCE_XATTR,
            workspace: Some(WorkspacePolicyProbe {
                workspace: PathBuf::from("/work/aterm"),
                policy: WorkspacePolicy::Accepted,
            }),
            local_seal: Some(LocalSealProbe {
                link_target: PathBuf::from("/Users//me/toolchains/trust-d3866677"),
                trustc: "d3866677".into(),
            }),
        };
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        assert!(run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            None,
            "doctor",
            &probes,
            &mut out,
            &mut err
        ));
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("[trust] policy the installed trust build 6808 accepts"),
            "{out}"
        );
        assert!(
            out.contains("warn — rustup's trust channel resolves to a LOCAL toolchain"),
            "{out}"
        );
        assert!(out.contains("trust-d3866677"), "{out}");
        assert!(out.contains("healthy"), "{out}");
    }

    #[test]
    fn targo_refusal_text_yields_the_field_name() {
        let stderr = "error: unknown field `compiler_timeout_secs`, expected one of `enabled`, \
                      `level`\n    --> Cargo.toml:3151:1\n";
        assert_eq!(
            unknown_field_in(stderr).as_deref(),
            Some("compiler_timeout_secs")
        );
        assert_eq!(
            unknown_field_in("warning: unused manifest key: trust\n"),
            None
        );
        assert_eq!(unknown_field_in(""), None);
    }

    /// A store with trust 8589 active, a synthetic home, and PATH on the store's bin —
    /// what every (5f) test runs the whole report over. Returns the build's `bin/`.
    fn tippy_sibling_store(label: &str) -> (Layout, PathBuf, std::ffi::OsString, PathBuf) {
        let l = layout(label);
        install(&l, "trust", 8589);
        let home = synthetic_home(label);
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let bin = l.build_dir("trust", 8589).join("bin");
        (l, home, path, bin)
    }

    /// The whole report: (exit ok, stdout, stderr).
    fn whole_report(l: &Layout, home: &Path, path: &OsStr) -> (bool, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ok = run_with(
            l,
            Some(home),
            Some(path),
            0,
            None,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        (
            ok,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn healthy(out: &str) -> bool {
        out.lines().any(|line| line == "doctor: healthy")
    }

    /// The one "tippy cannot run" warn in `out`, which must exist.
    fn tippy_warn(out: &str) -> &str {
        let mut warns = out.lines().filter(|line| line.contains("tippy cannot run"));
        let warn = warns
            .next()
            .unwrap_or_else(|| panic!("the warn is printed:\n{out}"));
        assert_eq!(warns.next(), None, "one warn, not two:\n{out}");
        assert!(
            warn.starts_with("doctor: warn — trust build 8589: tippy cannot run — "),
            "{warn}"
        );
        assert!(
            warn.ends_with(&format!(" (the bundle's fix: {TRUST_COMPILER_FIX})")),
            "every refusal carries the one fix that was measured to work: {warn}"
        );
        warn
    }

    /// The report withholds "healthy" for one tool that cannot run, and exits 0.
    fn assert_not_healthy_exit_0(ok: bool, out: &str, err: &str) {
        assert_withheld_exit_0(
            ok,
            out,
            err,
            "1 warning(s) above name a managed tool that cannot run",
        );
    }

    /// The report withholds "healthy" for one tool doctor cannot show to run — its warn
    /// says "may not run" or "cannot tell" — and says exactly that, never "cannot run".
    fn assert_unproven_exit_0(ok: bool, out: &str, err: &str) {
        assert_withheld_exit_0(
            ok,
            out,
            err,
            "1 warning(s) above name a managed tool that may not run or cannot be checked",
        );
    }

    fn assert_withheld_exit_0(ok: bool, out: &str, err: &str, counted: &str) {
        assert!(ok, "nothing structural: exit 0\n{out}{err}");
        assert!(
            !healthy(out),
            "a tool that cannot run is not health:\n{out}"
        );
        assert!(
            out.contains(&format!(
                "doctor: not healthy — {counted}; none is a structural problem, so the exit \
                 code stays 0"
            )),
            "{out}"
        );
        assert!(!err.contains("FAIL"), "{err}");
    }

    /// The closing line counts each kind of withheld warn under its own words.
    #[test]
    fn the_not_healthy_line_counts_cannot_run_apart_from_unproven() {
        assert_eq!(
            withheld_summary(2, 0),
            "2 warning(s) above name a managed tool that cannot run"
        );
        assert_eq!(
            withheld_summary(0, 1),
            "1 warning(s) above name a managed tool that may not run or cannot be checked"
        );
        assert_eq!(
            withheld_summary(1, 1),
            "1 warning(s) above name a managed tool that cannot run, and 1 name one that may \
             not run or cannot be checked"
        );
    }

    /// (5f) THE VIEW. With no seam recorded the check has nothing to say. Once a seam
    /// is attached against a synthetic rustup home, the view's stock names are clones of the
    /// store's tools and the report is healthy. Two wrong views are named with the repair
    /// and withhold "healthy": a `rustc` hard-linked to the store's `trustc` (the
    /// construction before clones), and one with `trustc`'s length, mode and time but other
    /// bytes. `repair`'s re-assertion rebuilds the view and the report is healthy again.
    #[cfg(unix)]
    #[test]
    fn a_rustup_view_whose_stock_names_are_not_the_stores_tools_is_not_healthy() {
        let (l, home, path, bin) = tippy_sibling_store("view");
        for tool in ["trustc", "targo", "trustdoc", "tippy", "tippy-driver"] {
            std::fs::write(bin.join(tool), tool).unwrap();
        }
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");

        let rustup = home.join(".rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
        crate::seam::attach(&l, &rustup, "trust").unwrap();
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");
        assert!(!out.contains("is not a clone of the store's"), "{out}");
        let store_trustc = bin.join("trustc");
        let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
        assert!(presents(&store_trustc, &view_rustc));
        let warn = format!(
            "doctor: warn — rustup `trust`: {} is not a clone of the store's {} (absent, other \
             bytes, or the hard link a view held before clones), so `rustc +trust` runs \
             something other than the managed trustc, or nothing; fix: `aterm pkg repair` \
             rebuilds the view",
            view_rustc.display(),
            store_trustc.display()
        );

        // The hard link the view used to be.
        std::fs::remove_file(&view_rustc).unwrap();
        std::fs::hard_link(&store_trustc, &view_rustc).unwrap();
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert_not_healthy_exit_0(ok, &out, &err);
        assert!(out.contains(&warn), "{out}");
        // Rebuilt as clones, silently: it presents the same bytes it did.
        let lines = crate::seam::reassert(&l, &rustup);
        assert!(lines.is_empty(), "{lines:?}");
        assert!(presents(&store_trustc, &view_rustc));
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");

        // The bundle's shape: the Trust tool's length, mode and time, other bytes.
        let modified = std::fs::metadata(&store_trustc)
            .unwrap()
            .modified()
            .unwrap();
        std::fs::remove_file(&view_rustc).unwrap();
        std::fs::write(&view_rustc, b"trustC").unwrap();
        std::fs::set_permissions(
            &view_rustc,
            std::fs::metadata(&store_trustc).unwrap().permissions(),
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&view_rustc)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(
            crate::clone::is_clone_of(&store_trustc, &view_rustc),
            "the attributes cannot tell it apart"
        );
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert_not_healthy_exit_0(ok, &out, &err);
        assert!(out.contains(&warn), "the bytes can: {out}");

        // What repair does: re-assert, which rebuilds the view — and says so, since the
        // view's `rustc` changed.
        let lines = crate::seam::reassert(&l, &rustup);
        assert!(
            lines.len() == 1 && lines[0].contains("now presents"),
            "{lines:?}"
        );
        assert!(presents(&store_trustc, &view_rustc));
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");
    }

    /// (5f) The view follows a dev-link. With trust dev-linked to a sysroot checkout the
    /// view presents the checkout, and this check compares it against the checkout's tools —
    /// healthy, with no "is not the store's" line over a view right by construction. A view
    /// left presenting the store while the link stands is named, against the checkout's
    /// tool, with the repair. Unlinked, the store's view is right again.
    #[cfg(unix)]
    #[test]
    fn a_dev_linked_trusts_view_is_healthy_when_it_presents_the_checkout() {
        let (l, home, path, bin) = tippy_sibling_store("view-devlink");
        for tool in ["trustc", "targo", "trustdoc", "tippy", "tippy-driver"] {
            std::fs::write(bin.join(tool), tool).unwrap();
        }
        let rustup = home.join(".rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
        crate::seam::attach(&l, &rustup, "trust").unwrap();
        let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
        assert!(presents(&bin.join("trustc"), &view_rustc));

        // A checkout shaped like a sysroot, dev-linked — and the seam not yet re-asserted:
        // the view still presents the store, which is now the wrong compiler.
        let checkout = home.join("stage2");
        std::fs::create_dir_all(checkout.join("bin")).unwrap();
        std::fs::create_dir_all(checkout.join("lib")).unwrap();
        for tool in ["trustc", "targo", "tippy"] {
            std::fs::write(checkout.join("bin").join(tool), format!("dev {tool}")).unwrap();
        }
        crate::linkmode::link(
            &l,
            "trust",
            &checkout,
            &[std::path::PathBuf::from("bin/trustc")],
        )
        .unwrap();
        let (ok, out, err) = whole_report(&l, &home, &path);
        // Two stock names differ (`rustc` and `cargo`; the checkout ships no trustdoc,
        // so `rustdoc` is not compared), each a tool that cannot run.
        assert_withheld_exit_0(
            ok,
            &out,
            &err,
            "2 warning(s) above name a managed tool that cannot run",
        );
        assert!(
            out.contains(&format!(
                "doctor: warn — rustup `trust`: {} is not the dev-linked checkout's {}",
                view_rustc.display(),
                checkout.join("bin").join("trustc").display()
            )),
            "{out}"
        );

        // What `link` (and repair) does: re-assert, which lays the checkout's view.
        let lines = crate::seam::reassert(&l, &rustup);
        assert!(
            lines.len() == 1 && lines[0].contains("now presents"),
            "{lines:?}"
        );
        assert!(
            std::fs::symlink_metadata(&view_rustc).unwrap().is_file(),
            "the view's rustc is a stub, not a link"
        );
        assert_eq!(
            crate::platform::resolve_shim(&view_rustc).as_deref(),
            Some(checkout.join("bin").join("trustc").as_path()),
            "…that execs the checkout's trustc"
        );
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");
        assert!(!out.contains("is not the"), "{out}");

        crate::linkmode::unlink(&l, "trust").unwrap();
        let lines = crate::seam::reassert(&l, &rustup);
        assert!(
            lines.len() == 1 && lines[0].contains("now presents"),
            "{lines:?}"
        );
        assert!(presents(&bin.join("trustc"), &view_rustc));
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");
    }

    /// (5f) A store-less dev-link has a seam too, checked against the checkout, the only
    /// thing there is: a view that presents it is not warned about; a stub replaced by a
    /// copy is a tool that cannot run, named with the repair.
    #[cfg(unix)]
    #[test]
    fn a_store_less_dev_linked_seam_is_checked_against_the_checkout() {
        let l = layout("view-devlink-nostore");
        let home = synthetic_home("view-devlink-nostore");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let checkout = home.join("stage2");
        std::fs::create_dir_all(checkout.join("bin")).unwrap();
        std::fs::create_dir_all(checkout.join("lib")).unwrap();
        for tool in ["trustc", "targo"] {
            std::fs::write(checkout.join("bin").join(tool), format!("dev {tool}")).unwrap();
        }
        crate::linkmode::link(
            &l,
            "trust",
            &checkout,
            &[std::path::PathBuf::from("bin/trustc")],
        )
        .unwrap();
        let rustup = home.join(".rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
        let lines = crate::seam::reassert(&l, &rustup);
        assert!(
            lines.len() == 1 && lines[0].contains("attached"),
            "{lines:?}"
        );
        let (_, out, _) = whole_report(&l, &home, &path);
        assert!(!out.contains("is not the"), "{out}");

        let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
        std::fs::remove_file(&view_rustc).unwrap();
        std::fs::write(&view_rustc, b"a copy").unwrap();
        let (_, out, _) = whole_report(&l, &home, &path);
        assert!(
            out.contains(&format!(
                "doctor: warn — rustup `trust`: {} is not the dev-linked checkout's {}",
                view_rustc.display(),
                checkout.join("bin").join("trustc").display()
            )),
            "{out}"
        );

        // The checkout stops being a sysroot: the seam refuses, and with no store build
        // behind the view doctor says so with the one fix — silence here left
        // `rustc +trust` running a compiler with no sysroot.
        std::fs::remove_dir_all(checkout.join("lib")).unwrap();
        let lines = crate::seam::reassert(&l, &rustup);
        assert!(
            lines.len() == 1 && lines[0].contains("not a sysroot"),
            "{lines:?}"
        );
        let (_, out, _) = whole_report(&l, &home, &path);
        assert!(
            out.contains("which atpkg cannot present")
                && out.contains("fix: `aterm pkg unlink trust`"),
            "{out}"
        );
    }

    /// (5f) tippy runs `trustc` under its own name and refuses a symbolic link there —
    /// so a bundle whose `trustc` is a link is a tippy that cannot run, and the report
    /// says so with the one fix. A plain `trustc` says nothing. A bundle with no tippy
    /// has nothing to refuse.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_trust_compiler_is_a_tippy_that_cannot_run() {
        let (l, home, path, bin) = tippy_sibling_store("symlinked-trustc");
        let trustc = bin.join("trustc");
        std::fs::write(bin.join("targo"), b"targo").unwrap();
        std::fs::write(bin.join("real"), b"trustc").unwrap();
        std::os::unix::fs::symlink("real", &trustc).unwrap();
        std::fs::write(bin.join("tippy"), b"tippy").unwrap();
        std::fs::write(bin.join("tippy-driver"), b"tippy-driver").unwrap();
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert_not_healthy_exit_0(ok, &out, &err);
        let warn = tippy_warn(&out);
        assert!(
            warn.contains(&format!(
                "{} is a symbolic link, and tippy requires the selected compiler trustc to be a \
                 plain file",
                trustc.display()
            )),
            "{warn}"
        );

        // A plain compiler: healthy, nothing said.
        std::fs::remove_file(&trustc).unwrap();
        std::fs::write(&trustc, b"trustc").unwrap();
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(
            ok && healthy(&out) && !out.contains("tippy cannot run"),
            "{out}{err}"
        );

        // The link back, but no tippy in the bundle: nothing is refused.
        std::fs::remove_file(&trustc).unwrap();
        std::os::unix::fs::symlink("real", &trustc).unwrap();
        std::fs::remove_file(bin.join("tippy")).unwrap();
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(
            ok && healthy(&out) && !out.contains("tippy cannot run"),
            "{out}{err}"
        );
    }

    /// Trust build 8595 the way the installed bundles 8571/8589/8590/8595 ship it: `rustc`
    /// and `cargo` SEPARATE files that differ from `trustc` and `targo`, beside `tippy`,
    /// with a `lib/` (a driver dylib, a nested rlib, a symlink) and a `share/` — active
    /// through PLAIN shims for `trust`, `trustc`, `targo`, `tippy` and `alab-tippy`, written
    /// directly the way a client from before exec roots laid them. Returns the layout, the
    /// synthetic home, `PATH` and the build's `bin/`.
    #[cfg(unix)]
    fn affected_trust_store(label: &str) -> (Layout, PathBuf, std::ffi::OsString, PathBuf) {
        let l = layout(label);
        let dir = install_build_tree(&l, "trust", 8595);
        let bin = dir.join("bin");
        for (file, body) in [
            ("trustc", "frontend 8595 / code signature: trustc"),
            ("rustc", "frontend 8595 / code signature: rustc_"),
            ("targo", "targo 8595"),
            ("cargo", "cargo copy 8595"),
            ("tippy", "tippy 8595"),
            ("tippy-driver", "tippy-driver 8595"),
            ("targo-tippy", "targo-tippy 8595"),
        ] {
            std::fs::write(bin.join(file), body).unwrap();
        }
        let rustlib = dir.join("lib/rustlib/aarch64-apple-darwin/lib");
        std::fs::create_dir_all(&rustlib).unwrap();
        std::fs::write(dir.join("lib/librustc_driver-5cfd.dylib"), b"driver").unwrap();
        std::fs::write(rustlib.join("libstd.rlib"), b"std").unwrap();
        std::os::unix::fs::symlink("../../share", dir.join("lib/rustlib/share-link")).unwrap();
        std::fs::create_dir_all(dir.join("share")).unwrap();
        std::fs::write(dir.join("share/README"), b"readme").unwrap();
        std::fs::create_dir_all(l.bin_dir()).unwrap();
        for (name, file) in [
            ("trust", "trust"),
            ("trustc", "trustc"),
            ("targo", "targo"),
            ("tippy", "tippy"),
            ("alab-tippy", "tippy"),
        ] {
            write_plain_shim(&l, name, &bin.join(file));
        }
        activate_channel(&l, "stable", &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        let home = synthetic_home(label);
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        (l, home, path, bin)
    }

    /// Today's plain shim at `bin/<name>`, written directly so no route is decided.
    #[cfg(unix)]
    fn write_plain_shim(l: &Layout, name: &str, target: &Path) {
        let shim = l.shim(&tool(name));
        std::fs::write(
            &shim,
            crate::platform::sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Everything `lstat` says about `dirs` and every entry under them — path, inode, link
    /// count, mode, size, mtime and ctime to the nanosecond, symlink target — sorted: what
    /// any write, link, unlink, chmod or create would move.
    #[cfg(unix)]
    fn tree_snapshot(dirs: &[&Path]) -> Vec<String> {
        use std::os::unix::fs::MetadataExt;
        let mut out = Vec::new();
        let mut stack: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
        while let Some(path) = stack.pop() {
            let m = std::fs::symlink_metadata(&path).unwrap();
            out.push(format!(
                "{} ino={} nlink={} mode={:o} size={} mtime={}.{} ctime={}.{} link={:?}",
                path.display(),
                m.ino(),
                m.nlink(),
                m.mode(),
                m.size(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
                std::fs::read_link(&path).ok()
            ));
            if m.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    stack.push(entry.unwrap().path());
                }
            }
        }
        out.sort();
        out
    }

    /// The whole report, with proof that it wrote nothing: the prefix and the home are
    /// `lstat`-identical before and after.
    #[cfg(unix)]
    fn read_only_report(l: &Layout, home: &Path, path: &OsStr) -> (bool, String, String) {
        let before = tree_snapshot(&[&l.prefix, home]);
        let report = whole_report(l, home, path);
        assert_eq!(
            before,
            tree_snapshot(&[&l.prefix, home]),
            "doctor wrote to the fixture:\n{}",
            report.1
        );
        report
    }

    /// The lines of `out` about trust build 8595's exec root.
    fn exec_root_lines(out: &str) -> Vec<&str> {
        out.lines()
            .filter(|line| line.contains(" — trust build 8595: "))
            .collect()
    }

    /// (5f)(c)-(f) THE EXEC ROOT, over the whole report, each state read-only:
    ///
    /// * plain shims and no root on an affected build: the warn, with both inodes and the
    ///   one fix, and "not healthy" at exit 0;
    /// * the pass's reconcile lays the root and routes the shims: the note, and healthy;
    /// * an `alab-` alias laid plain again: a warn naming it, one of five;
    /// * a file planted in the root's `lib/`: a warn naming that path; repair's `Deep`
    ///   reconcile rebuilds the root and the report is healthy again;
    /// * `compat/trust` replaced by a symbolic link to the whole root's directory: a warn
    ///   naming the link and the manual fix (never `repair` alone); the reconcile refuses
    ///   with the same path and fix and lays the shims plain, `ensure_root`'s refusal line
    ///   says the same, and once the link is gone and the directory is back the shims
    ///   route again;
    /// * the build's `bin/rustc` unreadable: the (f) warn naming the failed read, not
    ///   healthy, and the root left standing;
    /// * roots no build needs — a build's gone, a symlink at a numeric name — warn with
    ///   `aterm pkg gc` and leave the report healthy, and gc's sweep takes exactly them;
    /// * the build stops needing a root (`rustc` a hard link of `trustc`): no (c) line at
    ///   all, and its leftover root is a (d) warn until swept.
    #[cfg(unix)]
    #[test]
    fn an_affected_trust_build_is_healthy_only_while_its_exec_root_routes_every_shim() {
        use std::os::unix::fs::MetadataExt;
        let (l, home, path, bin) = affected_trust_store("exec-root");
        let differing = crate::compat::needs_root(bin.parent().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(differing, 6, "`rustc_` against `trustc`: six positions");
        let ino = |f: &str| std::fs::symlink_metadata(bin.join(f)).unwrap().ino();
        let root = crate::compat::root_dir(&l, 8595);
        let copy = format!(
            "bin/rustc (inode {}) is a separate file from bin/trustc (inode {}; 6 byte(s) differ)",
            ino("rustc"),
            ino("trustc")
        );
        let fix = "fix: `aterm pkg repair` lays the exec root (a copy-on-write clone — the store \
                   is never modified) and routes the shims";

        // An older client's machine: plain shims, no root.
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert_not_healthy_exit_0(ok, &out, &err);
        assert_eq!(
            exec_root_lines(&out),
            vec![format!(
                "doctor: warn — trust build 8595: PATH tippy cannot run — {copy}, which this \
                 build's tippy refuses, and no exec root stands at {}; {fix}",
                root.display()
            )],
            "{out}"
        );
        assert!(
            std::fs::symlink_metadata(crate::compat::compat_dir(&l)).is_err(),
            "doctor lays nothing"
        );

        // The pass heals it: root laid, every trust shim routed.
        let report = crate::compat::reconcile(&l, crate::seam::Depth::Shallow);
        assert_eq!(report.built, vec![8595], "{report:?}");
        assert_eq!(report.routed.len(), 5, "{report:?}");
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");
        assert_eq!(
            exec_root_lines(&out),
            vec![format!(
                "doctor: note — trust build 8595: {copy}, which its tippy refuses; its 5 trust \
                 shim(s) run each tool from {}, where rustc holds trustc's bytes (a \
                 copy-on-write clone; the store is untouched) — removed with the build",
                root.display()
            )],
            "{out}"
        );

        // One alias laid plain again, as by a writer that decided no route.
        write_plain_shim(&l, "alab-tippy", &bin.join("tippy"));
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert_not_healthy_exit_0(ok, &out, &err);
        assert_eq!(
            exec_root_lines(&out),
            vec![format!(
                "doctor: warn — trust build 8595: PATH tippy cannot run through 1 of 5 trust \
                 shim(s) (e.g. alab-tippy) — {copy}, which this build's tippy refuses, and \
                 those shims do not route through the exec root {}; {fix}",
                root.display()
            )],
            "{out}"
        );
        let report = crate::compat::reconcile(&l, crate::seam::Depth::Shallow);
        assert!(
            report.built.is_empty() && report.routed.len() == 1,
            "{report:?}"
        );
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");

        // A file planted in the root's `lib/`: the shims still route, but what they run is
        // not the build. Only a Deep walk sees it, and only a Deep reconcile heals it.
        let planted = root.join("lib").join("planted.dylib");
        std::fs::write(&planted, b"not the build's").unwrap();
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert_unproven_exit_0(ok, &out, &err);
        assert_eq!(
            exec_root_lines(&out),
            vec![format!(
                "doctor: warn — trust build 8595: PATH tippy may not run — {copy}, which this \
                 build's tippy refuses, and the exec root {} is not the store build {} file for \
                 file (first difference: {}); {fix}",
                root.display(),
                bin.parent().unwrap().display(),
                planted.display()
            )],
            "{out}"
        );
        let report = crate::compat::reconcile(&l, crate::seam::Depth::Deep);
        assert_eq!(report.built, vec![8595], "{report:?}");
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out) && out.contains("doctor: note — trust build 8595: "));
        assert!(err.is_empty(), "{err}");

        // A symbolic link at `compat/trust`, naming the very directory that stood there. No
        // update, repair or gc follows or removes it, so `repair` refuses on every run: every
        // line names the link and says to remove it first (the e2e run of 2026-09-15 read
        // "update directory is a symlink; refusing" beside advice to run the repair that
        // refused).
        let roots = crate::compat::roots_dir(&l);
        let aside = l.prefix.with_file_name(format!(
            "{}-compat-trust-aside",
            l.prefix.file_name().unwrap().to_string_lossy()
        ));
        std::fs::rename(&roots, &aside).unwrap();
        std::os::unix::fs::symlink(&aside, &roots).unwrap();
        let link_fix = "fix: remove that link (only the link: whatever it names is left as it \
                        is), then `aterm pkg repair` lays the exec root";
        let blocked = format!(
            "{} is a symbolic link, not a directory — atpkg lays nothing through a link \
             there, and no update, repair or gc removes it, so no exec root is laid at {} and \
             no shim routes through one",
            roots.display(),
            root.display()
        );
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert_unproven_exit_0(ok, &out, &err);
        assert_eq!(
            exec_root_lines(&out),
            vec![format!(
                "doctor: warn — trust build 8595: PATH tippy may not run — {copy}, which this \
                 build's tippy refuses, and {blocked}; {link_fix}"
            )],
            "{out}"
        );
        let report = crate::compat::reconcile(&l, crate::seam::Depth::Deep);
        assert_eq!(
            report.errors,
            vec![format!(
                "trust build 8595: {blocked} — its trust shims render plain and run the store \
                 path; {link_fix}"
            )],
            "{report:?}"
        );
        assert_eq!(report.routed.len(), 5, "{report:?}");
        let refused =
            crate::compat::ensure_root(&l, bin.parent().unwrap(), crate::seam::Depth::Shallow)
                .unwrap_err();
        assert_eq!(
            crate::compat::not_laid_line(&l, 8595, &refused),
            format!(
                "atpkg: trust build 8595: {blocked} — this build's trust shims run the store \
                 path, where its tippy refuses to start; {link_fix}"
            )
        );
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert_not_healthy_exit_0(ok, &out, &err);
        assert_eq!(
            exec_root_lines(&out),
            vec![format!(
                "doctor: warn — trust build 8595: PATH tippy cannot run — {copy}, which this \
                 build's tippy refuses, and {blocked}; {link_fix}"
            )],
            "{out}"
        );
        assert!(
            aside.join("8595").join("bin").join("tippy").is_file(),
            "the directory behind the link is left whole"
        );
        std::fs::remove_file(&roots).unwrap();
        std::fs::rename(&aside, &roots).unwrap();
        let report = crate::compat::reconcile(&l, crate::seam::Depth::Deep);
        assert!(
            report.errors.is_empty() && report.built.is_empty() && report.routed.len() == 5,
            "{report:?}"
        );
        let (ok, out, err) = whole_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");

        // (f) The build's `bin/rustc` unreadable: an unanswered question is not "needs none".
        // A warn that withholds "healthy" and names the failed read — and the root, which
        // doctor never touches, still stands for the passes to leave alone.
        if crate::platform::our_uid() != 0 {
            use std::os::unix::fs::PermissionsExt;
            let rustc = bin.join("rustc");
            let mode = std::fs::metadata(&rustc).unwrap().permissions();
            std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o000)).unwrap();
            let (ok, out, err) = read_only_report(&l, &home, &path);
            std::fs::set_permissions(&rustc, mode).unwrap();
            assert_unproven_exit_0(ok, &out, &err);
            let lines = exec_root_lines(&out);
            assert_eq!(lines.len(), 1, "{out}");
            assert!(
                lines[0].starts_with(
                    "doctor: warn — trust build 8595: cannot tell whether PATH tippy can run — \
                     cannot compare "
                ) && lines[0].ends_with(
                    "; any exec root standing for the build is left as it is; fix: make that \
                     store file readable again (`aterm pkg verify trust` checks the build \
                     against its signed tree)"
                ),
                "{out}"
            );
            assert!(root.is_dir());
        }

        // Roots no build needs: advisory, named with gc, and exactly what gc's sweep takes.
        let gone = crate::compat::root_dir(&l, 9999);
        std::fs::create_dir_all(gone.join("bin")).unwrap();
        let link = crate::compat::root_dir(&l, 42);
        std::os::unix::fs::symlink(&home, &link).unwrap();
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert!(ok && healthy(&out), "advisory, never health:\n{out}{err}");
        let strays: Vec<&str> = out.lines().filter(|l| l.contains("aterm pkg gc")).collect();
        assert_eq!(
            strays,
            vec![
                format!(
                    "doctor: warn — {} is not a directory, and atpkg lays only exec root \
                     directories under that name; fix: `aterm pkg gc` removes it",
                    link.display()
                ),
                format!(
                    "doctor: warn — exec root {} outlives trust build 9999, which is no longer \
                     in the store, and its clones keep that build's reclaimed blocks \
                     allocated; fix: `aterm pkg gc` removes it",
                    gone.display()
                ),
            ],
            "{out}"
        );
        let mut swept = crate::compat::sweep(&l).swept;
        swept.sort();
        assert_eq!(swept, vec![link.clone(), gone.clone()]);
        assert!(home.is_dir(), "the symlink was unlinked, not followed");

        // The build stops needing a root: `rustc` a hard link of `trustc`. No (c) line; the
        // root it leaves is a (d) warn until the sweep takes it, then nothing at all.
        std::fs::remove_file(bin.join("rustc")).unwrap();
        std::fs::hard_link(bin.join("trustc"), bin.join("rustc")).unwrap();
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");
        assert!(exec_root_lines(&out).is_empty(), "{out}");
        assert_eq!(
            out.lines()
                .filter(|l| l.contains("aterm pkg gc"))
                .collect::<Vec<_>>(),
            vec![format!(
                "doctor: warn — exec root {} stands for trust build 8595, which needs none (its \
                 bin/rustc is not a separate file from bin/trustc that its tippy refuses); fix: \
                 `aterm pkg gc` removes it",
                root.display()
            )],
            "{out}"
        );
        assert_eq!(crate::compat::sweep(&l).swept, vec![root.clone()]);
        let (ok, out, err) = read_only_report(&l, &home, &path);
        assert!(ok && healthy(&out), "{out}{err}");
        assert!(
            !out.contains("exec root") && exec_root_lines(&out).is_empty(),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// (5f)(c)'s example name prefers `tippy` — the tool the line is about — over whichever
    /// unrouted shim sorts first; a root that differs names the root, the store build and the
    /// first difference each as what it is; and a blocked `compat` names the manual fix.
    #[test]
    fn the_exec_root_warn_names_tippy_first_and_the_first_difference() {
        use crate::compat::{Inspection, RootState};
        let mut i = Inspection {
            differing: 2428,
            rustc_ino: 11,
            trustc_ino: 12,
            build_dir: PathBuf::from("/p/store/trust/8595"),
            root: PathBuf::from("/p/compat/trust/8595"),
            state: RootState::Matches,
            shims: ["alab-tippy", "targo", "tippy", "trustc"]
                .map(String::from)
                .to_vec(),
            unrouted: ["alab-tippy", "targo", "tippy"].map(String::from).to_vec(),
        };
        let (line, claim) = exec_root_line("doctor", 8595, &i);
        assert_eq!(claim, ToolClaim::CannotRun);
        assert!(
            line.contains(
                "PATH tippy cannot run through 3 of 4 trust shim(s) (e.g. tippy) — bin/rustc \
                 (inode 11) is a separate file from bin/trustc (inode 12; 2428 byte(s) differ)"
            ),
            "{line}"
        );
        i.unrouted = vec!["targo".into()];
        assert!(
            exec_root_line("doctor", 8595, &i)
                .0
                .contains("(e.g. targo)")
        );
        // Each path labelled for what it is: the root, the store build, and the first
        // path inside the root that is not the build's (the e2e run of 2026-09-15 read
        // "differs from the build at <root>/bin/tippy", as if the build lived there).
        i.state = RootState::Differs(PathBuf::from("/p/compat/trust/8595/bin/tippy"));
        let (line, claim) = exec_root_line("status", 8595, &i);
        assert!(
            claim == ToolClaim::Unproven
                && line.starts_with("status: warn — trust build 8595: PATH tippy may not run — "),
            "{line}"
        );
        assert!(
            line.ends_with(
                "and the exec root /p/compat/trust/8595 is not the store build \
                 /p/store/trust/8595 file for file (first difference: \
                 /p/compat/trust/8595/bin/tippy); fix: `aterm pkg repair` lays the exec root \
                 (a copy-on-write clone — the store is never modified) and routes the shims"
            ),
            "{line}"
        );
        // Blocked: a link (or a file) at compat/ or compat/trust. The fix is the manual
        // step, never `repair` alone, which refuses there on every run; `cannot` while no
        // shim carries a route, `may not` while one routed earlier might still run.
        i.state = RootState::Blocked(crate::compat::Blocked {
            at: PathBuf::from("/p/compat/trust"),
            symlink: true,
            root: PathBuf::from("/p/compat/trust/8595"),
        });
        let (line, claim) = exec_root_line("doctor", 8595, &i);
        assert_eq!(claim, ToolClaim::Unproven, "{line}");
        assert_eq!(
            line,
            "doctor: warn — trust build 8595: PATH tippy may not run — bin/rustc (inode 11) is \
             a separate file from bin/trustc (inode 12; 2428 byte(s) differ), which this \
             build's tippy refuses, and /p/compat/trust is a symbolic link, not a directory — \
             atpkg lays nothing through a link there, and no update, repair or gc removes it, \
             so no exec root is laid at /p/compat/trust/8595 and no shim routes through one; \
             fix: remove that link (only \
             the link: whatever it names is left as it is), then `aterm pkg repair` lays the \
             exec root"
        );
        i.unrouted = i.shims.clone();
        i.state = RootState::Blocked(crate::compat::Blocked {
            at: PathBuf::from("/p/compat"),
            symlink: false,
            root: PathBuf::from("/p/compat/trust/8595"),
        });
        let (line, claim) = exec_root_line("doctor", 8595, &i);
        assert_eq!(claim, ToolClaim::CannotRun, "{line}");
        assert!(
            line.contains("PATH tippy cannot run — ")
                && line.ends_with(
                    "and /p/compat is not a directory — no update, repair or gc removes it, so no \
                     exec root is laid at /p/compat/trust/8595 and no shim routes through one; \
                     fix: move that file out of the way, then `aterm pkg repair` lays the exec \
                     root"
                ),
            "{line}"
        );
        assert!(
            !line.contains("repair` lays the exec root (a copy-on-write clone"),
            "{line}"
        );
        i.state = RootState::Matches;
        i.unrouted.clear();
        let (line, claim) = exec_root_line("doctor", 8595, &i);
        assert!(
            claim == ToolClaim::Runs && line.starts_with("doctor: note — "),
            "{line}"
        );
        // Every "cannot run" line counts as one; every "may not run" line as unproven.
        for (state, unrouted, claim) in [
            (RootState::Absent, vec![], ToolClaim::CannotRun),
            (
                RootState::Matches,
                vec!["tippy".to_string()],
                ToolClaim::CannotRun,
            ),
        ] {
            i.state = state;
            i.unrouted = unrouted;
            let (line, got) = exec_root_line("doctor", 8595, &i);
            assert_eq!(got, claim, "{line}");
            assert!(line.contains("PATH tippy cannot run "), "{line}");
        }
    }

    #[test]
    fn the_trust_table_is_found_on_the_nearest_ancestor_manifest() {
        let root = synthetic_home("wstable");
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\n\n[trust]\ncompiler_timeout_secs = 1\n",
        )
        .unwrap();
        let nested = root.join("crates").join("x");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_eq!(workspace_with_trust_table(&nested), Some(root.clone()));
        let plain = synthetic_home("wsplain");
        std::fs::write(plain.join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(workspace_with_trust_table(&plain), None);
    }

    /// A bundle whose `bin/` executables carry the tag, and a tagged shim, are each ONE
    /// warning line — exit stays 0 — naming the count, an example, what breaks (a release
    /// cut, after the claim) and the cure. Minted with a synthetic `user.*` attribute:
    /// `com.apple.provenance` cannot be set by hand, and a test process may itself be
    /// tracked, so the negative half uses an attribute nothing set rather than the real one.
    #[cfg(target_os = "macos")]
    #[test]
    fn provenance_tagged_executables_and_shims_warn_but_exit_zero() {
        let l = layout("provenance");
        install(&l, "trust", 8590);
        install(&l, "ay", 8256);
        let trust_exe = l.build_dir("trust", 8590).join("bin/trust");
        let shim = l.bin_dir().join("trust");
        assert!(
            trust_exe.is_file() && shim.is_file(),
            "fixture laid the tool and its shim"
        );
        crate::provenance::set_xattr_for_test(&trust_exe, "user.aterm.probe", b"1").unwrap();
        crate::provenance::set_xattr_for_test(&shim, "user.aterm.probe", b"1").unwrap();
        let home = synthetic_home("provenance");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-09-12T00:00:00Z").unwrap();
        let run = |attr: &'static str| {
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let probes = Probes {
                provenance_attr: attr,
                ..Probes::default()
            };
            let ok = run_with(
                &l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &probes,
                &mut out,
                &mut err,
            );
            (
                ok,
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
            )
        };

        let (ok, out, err) = run("user.aterm.probe");
        assert!(
            ok,
            "a tagged bundle is advisory, never structural:\n{out}\n{err}"
        );
        let bundle_line = out
            .lines()
            .find(|l| l.contains("trust build 8590 carries com.apple.provenance"))
            .unwrap_or_else(|| panic!("the bundle line is missing:\n{out}"));
        assert!(bundle_line.starts_with("doctor: warn — "), "{bundle_line}");
        assert!(bundle_line.contains("1 of 1 file(s)"), "{bundle_line}");
        assert!(bundle_line.contains("(e.g. trust)"), "{bundle_line}");
        assert!(
            bundle_line.contains("proof_snapshot.py"),
            "what it breaks: {bundle_line}"
        );
        assert!(
            bundle_line.contains("aterm pkg uninstall trust && aterm pkg install trust"),
            "the cure names the program: {bundle_line}"
        );
        assert!(bundle_line.contains("TRUST_STAGE2_BIN"), "{bundle_line}");
        // `ay` carries nothing and is not mentioned.
        assert!(!out.contains("ay build 8256 carries"), "{out}");
        let shim_line = out
            .lines()
            .find(|l| l.contains("shim(s) in"))
            .unwrap_or_else(|| panic!("the shim line is missing:\n{out}"));
        assert!(
            shim_line.starts_with("doctor: warn — 1 of 2 shim(s) in"),
            "{shim_line}"
        );
        assert!(shim_line.contains("(e.g. trust)"), "{shim_line}");
        assert!(shim_line.contains("aterm pkg repair"), "{shim_line}");
        assert!(err.is_empty(), "warnings go to out, never err:\n{err}");

        // An attribute nothing set: neither line.
        let (ok, out, _) = run("user.aterm.absent");
        assert!(ok);
        assert!(!out.contains("com.apple.provenance"), "{out}");
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The RECORD a tracked installer left when its lane could not run is reported as
    /// the cause on the tagged bundle's line; a record with no tagged file behind it is
    /// named as stale rather than presented as a tagged bundle.
    #[cfg(target_os = "macos")]
    /// The two freshness warnings (2026-09-15), separately: a listing unreached for
    /// days is named as such — every pass since ran on the cache — and an index build
    /// unchanged for a month WHILE the listing was reachable reads as a frozen
    /// publisher. A record from before the fields existed says nothing new, and an
    /// unreachable listing is never called a frozen publisher.
    #[test]
    fn doctor_names_an_unreached_listing_and_a_frozen_index_separately() {
        let l = layout("freshness");
        install(&l, "ay", 8256);
        let home = synthetic_home("freshness");
        let now = crate::flow::rfc3339_to_unix("2026-10-20T00:00:00Z").unwrap();
        let run = |status: crate::Status| -> String {
            crate::status::write(&l, &status).unwrap();
            let path = std::env::join_paths([l.bin_dir()]).unwrap();
            let (mut out, mut err): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
            let _ = run_with(
                &l,
                Some(&home),
                Some(&path),
                now,
                None,
                None,
                "doctor",
                &Probes::default(),
                &mut out,
                &mut err,
            );
            String::from_utf8_lossy(&out).into_owned()
        };
        let base = crate::Status {
            schema: 1,
            updated_at: "2026-10-19T00:00:00Z".into(),
            enabled: true,
            index_source: "alabsystems/aterm".into(),
            outcome: "up to date".into(),
            seams: Vec::new(),
            last_success_at: "2026-10-19T00:00:00Z".into(),
            last_index_reached_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            programs: Default::default(),
        };
        let out = run(base.clone());
        assert!(!out.contains("has not been reached"), "{out}");
        assert!(!out.contains("publishing looks frozen"), "{out}");
        let out = run(crate::Status {
            last_index_reached_at: "2026-10-10T00:00:00Z".into(),
            last_index_build: 32,
            index_build_changed_at: "2026-10-01T00:00:00Z".into(),
            ..base.clone()
        });
        assert!(
            out.contains("has not been reached for 10 day(s)"),
            "an unreached listing is named:\n{out}"
        );
        assert!(
            !out.contains("publishing looks frozen"),
            "unreachable is not frozen:\n{out}"
        );
        let out = run(crate::Status {
            last_index_reached_at: "2026-10-19T00:00:00Z".into(),
            last_index_build: 32,
            index_build_changed_at: "2026-09-10T00:00:00Z".into(),
            ..base
        });
        assert!(
            out.contains("index build 32 has not changed in 40 day(s)"),
            "a frozen publisher is named:\n{out}"
        );
        assert!(out.contains("publishing looks frozen"), "{out}");
        assert!(!out.contains("has not been reached"), "{out}");
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// macOS-gated like its sibling above: it mints the synthetic `user.*` attribute through
    /// [`crate::provenance::set_xattr_for_test`], which is
    /// `#[cfg(all(test, target_os = "macos"))]` because it calls the six-argument Darwin
    /// `setxattr`. Without the gate the whole unit-test binary fails to build off macOS
    /// (E0425); `crates/atpkg/tests/platform_cfg_parity.rs` is the standing guard.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tracked_install_record_is_reported_as_the_cause() {
        let l = layout("tracked-record");
        install(&l, "trust", 8590);
        install(&l, "ay", 8256);
        let trust_exe = l.build_dir("trust", 8590).join("bin/trust");
        crate::provenance::set_xattr_for_test(&trust_exe, "user.aterm.probe", b"1").unwrap();
        crate::store::record_tracked_install(
            &l.build_dir("trust", 8590),
            "the untracked lane could not run (launchctl submit failed)",
        )
        .unwrap();
        // `ay` carries a record but nothing tagged: stale.
        crate::store::record_tracked_install(&l.build_dir("ay", 8256), "an old record").unwrap();
        let home = synthetic_home("tracked-record");
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let now = crate::flow::rfc3339_to_unix("2026-09-12T00:00:00Z").unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let probes = Probes {
            provenance_attr: "user.aterm.probe",
            ..Probes::default()
        };
        let ok = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            None,
            "doctor",
            &probes,
            &mut out,
            &mut err,
        );
        let out = String::from_utf8_lossy(&out).into_owned();
        assert!(ok, "advisory, never structural:\n{out}");
        let bundle_line = out
            .lines()
            .find(|l| l.contains("trust build 8590 carries com.apple.provenance"))
            .unwrap_or_else(|| panic!("the bundle line is missing:\n{out}"));
        assert!(
            bundle_line.contains(
                "recorded cause: the untracked lane could not run (launchctl submit failed)"
            ),
            "{bundle_line}"
        );
        assert!(
            bundle_line.contains("fix: re-seed the bundle untagged"),
            "the cure still follows the cause: {bundle_line}"
        );
        let stale = out
            .lines()
            .find(|l| l.contains("ay build 8256 carries a tracked-install record"))
            .unwrap_or_else(|| panic!("the stale-record line is missing:\n{out}"));
        assert!(stale.contains("(an old record)"), "{stale}");
        assert!(stale.contains("stale record"), "{stale}");
        assert!(
            stale.contains("aterm pkg uninstall ay && aterm pkg install ay"),
            "{stale}"
        );
        assert!(
            !out.contains("ay build 8256 carries com.apple.provenance"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }
}
