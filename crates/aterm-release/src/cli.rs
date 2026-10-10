// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI surface (release spec §5): hand-rolled `std::env::args` parsing (no
//! third-party arg crate — same rule as `aterm-ctl`) for the whole command
//! surface: `release [--bump | --commit REV] (--mac-only | --linux-artifacts DIR …)
//! [--release-credentials <profile.toml>] [--engine DIR] [--dry-run] [--verbose]
//! [--forget | --abandon vX.Y.Z]`, `build` (the one build: the release's step 7, or
//! HEAD's provisional bundle between releases), `cut [--dry-run]
//! [--resume] [--abandon vX.Y.Z] [--min-build N] [--rehearse OWNER/REPO]
//! [--release-credentials <profile.toml>]` (the release's step 12),
//! `recover vX.Y.Z <claim-sha> --old-publisher-stopped`, `status`,
//! `check [--commit REV]`, `trust verdict|judge|pending|prover|admit|conclude|compare`,
//! `verify [vX.Y.Z]`,
//! `yank <build> [--release-credentials <profile.toml>] --mac-only|--linux-artifacts DIRECTORY`.

use std::process::Command;

use crate::ledger::{self, Error};
use crate::{publish, verify};

/// The retired pre-roster acknowledgement. A leftover spelling in a script or a
/// runbook is refused by name, in one sentence, rather than as an unknown flag.
pub const RETIRED_STRAND_FLAG: &str = "--strand-pre-roster-clients";

/// The one sentence [`RETIRED_STRAND_FLAG`] is refused with.
pub const RETIRED_STRAND_REFUSAL: &str = "--strand-pre-roster-clients is retired: installs older \
     than v0.21.0 are abandoned and every current client authorizes a release by the machine \
     roster alone, so a rostered key needs no acknowledgement — drop the flag";

/// The retired exit for a cut that went live on the private origin and could not be
/// mirrored. A cut publishes once now, onto the channel, so that state cannot exist.
pub const RETIRED_UNMIRRORED_FLAG: &str = "--retire-unmirrored";

/// The one sentence [`RETIRED_UNMIRRORED_FLAG`] is refused with.
pub const RETIRED_UNMIRRORED_REFUSAL: &str = "--retire-unmirrored is retired: a cut publishes \
     once, onto the channel; withdraw an unpublished cut with `ship release --abandon vX.Y.Z`";

/// Why a real or rehearsal cut that says nothing about Linux is refused.
pub const MAC_ONLY_REFUSAL: &str = "this cut says nothing about Linux: pass --linux-artifacts \
     DIRECTORY (with --linux-worker ARCH=HOST:REPO for each native host the cutter should drive, \
     or build there by hand) to ship Linux, or --mac-only to ship a Mac-only release on purpose. \
     A cut without Linux strands every Linux install on the build it has";

pub const USAGE: &str = "aterm-release — the `targo --unverified ship` release cutter

USAGE
  targo --unverified ship release [--bump | --commit REV]
                 (--mac-only | --linux-artifacts DIRECTORY [--linux-target aarch64|x86_64]...
                  [--linux-worker ARCH=HOST:REPO]...)
                 [--release-credentials <profile.toml>] [--engine DIRECTORY]
                 [--dry-run] [--verbose] [--forget | --abandon vX.Y.Z]
      THE way to release aterm, from a clean clone of its own on main: checks,
      sync, commit, preflight, the whole-tree Trust lane, this machine's gate
      lock, the ONE build (both slices at once, kept in the release cache),
      MEASURE of those bytes, preflight again, pub stage, pub publish, the cut
      (it claims, then signs the measured bytes), the published release's shape
      and the site, and the channel's latest — each step skipped when its work
      is already done for the release commit (the receipts, the built bundle,
      the attestation, the engine's ledger, the cut journal). Ends in
      one line: RELEASED …, or REFUSED at <step>: <why> — <what is public>.
      <remedy>. Run it again to resume; a gap only the owner can close is asked
      once on the owner's aterm and waited on, while the long steps run.
        --bump             commit the next MINOR on origin/main's tip, push it,
                           release it
        --commit REV       release this commit of origin/main; replaces a
                           candidate this clone has not published yet (said)
        --mac-only / --linux-artifacts …
                           the Linux declaration the cut requires (journaled:
                           a bare re-run resumes with it)
        --release-credentials <profile.toml>
                           the cut's signing profile (default
                           ~/.aterm/release-credentials.toml)
        --engine DIRECTORY the publication engine (default
                           $PUBLICATION_ENGINE, else ~/Developer/publication, or ~/publication until it moves there)
        --dry-run          the plan, the ETA, and every refusal a real run would
                           meet; nothing changes
        --verbose          stream every step's output, not only into its log
        --forget           drop this clone's release journal (nothing public
                           changes), withdraw its open asks, and exit — on its
                           own (no --dry-run, no flag of a run)
        --abandon vX.Y.Z   withdraw this clone's claimed, unpublished cut of
                           vX.Y.Z (its draft or uploads, its origin tag, its
                           journal; the claim commit stays) — the claim's own
                           cutter does it, so run it from the release's clone

  targo --unverified ship build
      THE ONE BUILD of a release bundle: both slices at once from a snapshot of
      its commit's git objects, the x86_64 slice driven under Rosetta, the
      unsigned bundle laid out — into the release cache, kept by what it was
      built as; or found built there, its bytes re-proved. STEP 7 OF `ship
      release`, which runs it for the release this clone's journal reserved
      (build n, the release commit). With no release in progress it builds
      HEAD under a provisional number (max(HEAD's ledger tail + 1, now),
      claimed nowhere) and prints the .app: what `tools/verify.sh --measure
      <app>` (or `--full <app>`) judges between releases.

  targo --unverified ship linux-build --version X.Y.Z --build-number N
                 --commit FULL-CLAIM-SHA --out EXISTING-PRIVATE-DIRECTORY
      Local native Linux worker: sealed Trust build + provenance handoff.
      No signing or publication. Run on both x86_64 and aarch64 Linux,
      then give the complete handoff directory to the canonical cutter.

  targo --unverified ship cut [--dry-run] [--resume] [--abandon vX.Y.Z]
                 [--min-build N] [--rehearse OWNER/REPO]
                 [--release-credentials <profile.toml>]
                 [--linux-artifacts DIRECTORY [--linux-target aarch64|x86_64]...
                  [--linux-worker ARCH=HOST:REPO]... | --mac-only]
      STEP 12 OF `ship release`, which runs it (a release is `ship release` and
      nothing else): gates → the claim of the release's reserved build → a
      byte copy of the MEASURED bundle signed, notarized and packaged (the cut
      never builds) → tag → ONE publication onto the release channel, made the
      head last — the binary of the commit `pub publish` published, by that
      commit's own cutter (the release re-runs as it). A real or rehearsal cut
      must name --linux-artifacts or --mac-only.
        --dry-run          gates + a byte copy of the MEASURED bundle this
                           clone's release built of the checkout's commit,
                           under its reserved number (claimed nowhere), signed
                           into dist/ and notarized by Apple; nothing committed,
                           published or built
        --resume           re-enter the journaled cut (dist/cut-state.toml) at
                           its first incomplete step
        --abandon vX.Y.Z   withdraw an unpublished cut: its own draft deleted, or
                           exactly its uploads taken back off the engine's source
                           release; its origin tag and the local journal deleted
                           (the claim commit stays; a later cut recuts)
        --min-build N      emit an operator apply floor into the manifest
        --rehearse O/R     full real cut of that measured bundle published to
                           the scratch channel O/R, which must be PUBLIC (its
                           reserved number, no ledger push, no tag on origin)
        --linux-artifacts DIRECTORY
                           require declared Linux handoffs before signing;
                           missing workers pause the journaled cut for resume
        --linux-target ARCH
                           explicit native subset (repeatable); defaults to both
        --linux-worker ARCH=HOST:REPO
                           after the claim, build ARCH on HOST over ssh (a
                           throwaway worktree of REPO at the claim commit) and
                           copy its pair into --linux-artifacts; the pair is
                           checked exactly like a hand-carried one
        --mac-only         ship no Linux build, on purpose; a real or rehearsal
                           cut must say this or --linux-artifacts
        --release-credentials <profile.toml>
                           the signing profile: a rostered, unrevoked machine
                           key and its notary credential (this machine's is
                           ~/.aterm/release-credentials.toml). A cut without one
                           refuses before it claims

  targo --unverified ship provision --id <machine-id> [--check] [--cert-dir <folder>]
                           ON A BARE MACHINE (no Trust toolchain) run
                           tools/bootstrap-publisher.sh --id <machine-id> instead:
                           it installs Trust, then runs provision.
                           Makes THIS machine a publisher. Seeds the newest
                           master-signed roster pair from the channel release
                           into dist/, audits the WHOLE publishing stack — Trust
                           stage2 (real smoke-compile), the targo/tippy/ty
                           drivers, the store's targo + trustc front door
                           (rustup's link reported informationally), the stable x86_64
                           slice, Apple identity + live-tested notary
                           credential, the credentials profile, gh auth, channel
                           token — each gap with its exact remedy, and only on a
                           CLEAN pass mints this machine's key via the join
                           ceremony (the paper phrase, typed once, is the only
                           input; a roster id is irreversible and is never
                           consumed on a machine that cannot release).
                           Idempotent: a provisioned machine is audited and
                           bound through the real authorize_cut gate, never
                           re-minted. Ends in a READY TO CUT verdict.
        --cert-dir <folder>
                           the one visible folder the Apple browser errand may
                           use: the certificate request is copied there for the
                           upload dialog, and the issued .cer is looked for there
                           (and in ~/.aterm/apple). Without it every read stays
                           inside ~/.aterm/apple — provision never touches
                           ~/Downloads or another folder macOS guards on its own
                           initiative; naming one is the consent, and a note says
                           so before the errand.
        --check            audit only: no mint, no dist/ writes. Exits non-zero
                           if anything is open, so a caller can gate on it. The
                           mode to run when something is wrong and you do not
                           yet want to touch the machine.

  targo --unverified ship status        version · ledger tail · dangling claims · newest
                           published build
  targo --unverified ship check [--commit REV]
                           would a real cut of REV (default HEAD) pass the
                           pre-claim gates that read only the commit and this
                           repository's receipt store — the release notes, the
                           predecessor's handoff fixtures, the handoff policy,
                           the MEASURE receipt, the whole-tree TRUST facts —
                           with the prover this machine's cut judges with?
                           The cut's own gate code, run for REV: no build of
                           aterm, seconds. Answers only as REV's own cutter
                           (built from REV) and refuses to answer otherwise.
                           Exits 1 naming each gate that would refuse and its
                           remedy
  targo --unverified ship trust verdict [--commit REV] [--prover BIN-DIR]
                           the whole-tree Trust lane's verdict for REV (default
                           HEAD), JUDGED NOW: the facts a whole-tree run filed
                           for REV's tree less its floors (the ratchet and
                           tools/trust-gate-memory.tsv), from the prover the
                           cut judges with (--prover names its bin/; default:
                           the cut's own), held to REV's own floors. The cut's
                           own judge, one line; exit 0 VERIFIED, 1 not, 2 could
                           not answer. A commit that changes only the floors
                           reuses the facts of the run before it — no new run —
                           its floors held to the merge contract's up-only rule
                           against the run's
  targo --unverified ship trust judge [--commit REV] [--prover BIN-DIR] [--update-ratchet [--reason WHY]]
                           the lane's whole report for REV's stored facts,
                           judged against the WORKING tree's
                           tools/trust-gate-ratchet.tsv; --update-ratchet writes
                           it as the lane's --update-ratchet would (a first
                           floor, a high-water mark, a total corrected, a
                           re-base onto this prover it meets — never a
                           lowering); with --reason WHY, a floor another prover
                           recorded that the facts fall below is lowered onto
                           this prover, WHY in its fifth column; working floors
                           the up-only rule refuses against the run's are named
                           LOWERED, and fail
  targo --unverified ship trust pending [--commit REV] [--prover BIN-DIR]
                           the libraries REV's stored facts leave undecided
                           (trustd did not admit them, or lapsed budgets could
                           cover their shortfall), one per line: what
                           tools/trust-gate-all.sh --remeasure runs again
  targo --unverified ship trust prover --driver <targo>
  targo --unverified ship trust admit --plan <file>
  targo --unverified ship trust conclude --plan <file> [--update-ratchet]
                           tools/trust-gate-all.sh's own calls: the pinned
                           driver's prover identity, the libraries this host
                           refuses for memory before the run, and the lane's
                           end (measure the logs, judge, file the facts)
  targo --unverified ship trust compare --base <plan> --candidate <plan>
        [--base-logdir DIR] [--candidate-logdir DIR] [--tsv FILE] [--base-prover ID]
        [--ratchet FILE] [--retractions FILE] [--trust-repo DIR]
        [--candidate-trust-commit SHA] [--base-trust-commit SHA] [--obligations-tsv FILE]
                           THE CANDIDATE GATE (D27; tools/trust-gate-candidate.sh
                           runs it): two whole-tree runs of one aterm tree on one
                           machine — the published seal's and a candidate
                           toolchain's — compared library by library and,
                           from each check's per-obligation record, obligation
                           by obligation. An obligation the seal proved that
                           the candidate does not refuses, whatever it gains
                           elsewhere, unless the retraction ledger
                           (--retractions) names it as a false proof retracted
                           by a trust commit the candidate's own contains (read
                           in --trust-repo); a library given no result in its
                           wall budget, or no usable record, refuses; where the
                           seal's own check gave none, the candidate is held to
                           the floor the seal's prover recorded (--ratchet,
                           default the base plan's) and refused when there is
                           none. --obligations-tsv lists each lost obligation
                           by its ledger key. Exit 0 PUBLISH-OK, 1
                           PUBLISH-REFUSED, 2 could not compare (also refused):
                           runs of different trees, machines, scopes, budgets
                           or slot counts, a run that did not conclude or
                           captured no records, the same prover twice, nothing
                           compared at all, a ledger that does not read or that
                           a candidate contradicts
  targo --unverified ship trust obligations --report <report.ndjson> --package <name>
        --repo <checkout> [--manifest <path>] --out <file>
                           tools/trust-gate-all.sh --obligations's own call: one
                           check's proof report reduced to its per-obligation
                           record (exit 1, and why, when it cannot vouch for it)
  targo --unverified ship recover vX.Y.Z <full-claim-sha> --old-publisher-stopped
        [--release-credentials <profile.toml>] [--no-draft-was-posted]
                           finish or withdraw the cut of a machine that died
                           mid-cut: withdraw it if unpublished, verify and finish
                           it if published. Run only once its old cutter process
                           has exited (--old-publisher-stopped says so).
        --no-draft-was-posted
                           you checked the channel's releases page and it shows
                           no release for vX.Y.Z (needed when the journal is lost)
  targo --unverified ship verify [vX.Y.Z]
                           re-run the post-publish check anytime
  targo --unverified ship yank <build> [--release-credentials <profile.toml>]
                 (--linux-artifacts DIRECTORY [--linux-target aarch64|x86_64]...
                  [--linux-worker ARCH=HOST:REPO]... | --mac-only)
                           publish and verify a successor FIRST (a real cut with
                           min_build = <build>+1, signed with
                           --release-credentials, declaring Linux as any cut
                           must); only then delete the bad build's origin tag
                           and demote its channel release to a prerelease
";

/// A parsed invocation. `Cut.abandon` rides outside [`publish::CutOptions`]
/// because abandoning is not a cut — it never reaches the pipeline.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Help,
    LinuxBuild {
        version: String,
        build: u64,
        commit: String,
        out: std::path::PathBuf,
    },
    /// `build` — the one build: the release's step 7, or HEAD's provisional bundle
    /// between releases ([`crate::artifact::run_build`]).
    Build,
    Cut {
        opts: publish::CutOptions,
        abandon: Option<String>,
    },
    Provision {
        id: String,
        check: bool,
        /// `--cert-dir`: the ONE visible folder the browser errand may use — the request
        /// is copied there and the certificate looked for there. `None` keeps every read
        /// inside `~/.aterm/apple`; `~/Downloads` is never touched unasked.
        cert_dir: Option<std::path::PathBuf>,
    },
    /// `release …` — the one release command ([`crate::release`]).
    Release(crate::release::ReleaseOptions),
    Status,
    /// `check [--commit REV]` — the cut's pre-claim gates that read only a
    /// commit and the receipt store, for one commit
    /// ([`crate::gates::precheck`]); `None` is HEAD.
    Check {
        commit: Option<String>,
    },
    Recover {
        version: String,
        owner: String,
        release_credentials: Option<std::path::PathBuf>,
        /// `--no-draft-was-posted`: the operator has checked the releases page and
        /// answers for a lost journal that no create POST ever landed.
        no_draft_posted: bool,
    },
    Verify {
        version: Option<String>,
    },
    /// `trust <verb>` — the whole-tree Trust lane's facts and their judge
    /// ([`crate::trust_facts`]).
    Trust(crate::trust_facts::TrustCmd),
    Yank {
        build: u64,
        /// A yank PUBLISHES a successor before it deletes anything, so it takes
        /// the cut's signing inputs — and since the paper master was armed it
        /// must, or that cut refuses pre-claim. See [`verify::YankOptions`].
        opts: verify::YankOptions,
    },
}

/// Entry point for the whole binary — `main()` delegates here so every code
/// path (parsing included) is reachable from tests without spawning a
/// process. Exit codes: 0 ok, 1 pipeline failure, 2 usage error.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm-release: {e}\nusage: {} help", publish::SHIP_COMMAND);
            return 2;
        }
    };
    // The trust verbs answer three ways (0 / 1 / 2: pass, fail, could not
    // run), which the lane and the release scripts read; every other verb is
    // ok or not.
    // `trust prover` and a `trust conclude` that files nothing read no git, so
    // a directory that is no checkout (the lane's self-test) is no refusal;
    // the verbs that do read git fail there on their own.
    if let Cmd::Trust(t) = cmd {
        let root = repo_root()
            .ok()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let code = crate::trust_facts::run_cmd(&root, t);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        return code;
    }
    // `release` answers 0 / 1 / 2 / 3 (released, refused, usage, a resumable stop);
    // `release --abandon` is the claim's withdrawal, ok or not.
    if let Cmd::Release(opts) = &cmd {
        let code = match (repo_root(), &opts.abandon) {
            (Ok(root), Some(version)) => match verify::run_abandon(&root, version) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{}", failure_line(&e.to_string()));
                    1
                }
            },
            (Ok(root), None) => crate::release_steps::run(&root, opts),
            (Err(e), _) => {
                eprintln!("aterm-release release: {e}");
                2
            }
        };
        let _ = std::io::Write::flush(&mut std::io::stdout());
        return code;
    }
    let outcome = dispatch(cmd);
    // The ONE flush in the binary, and it earns its place. Everything above prints to
    // stdout; this prints to stderr. Under `… 2>&1 | tee provision.log` — the obvious
    // thing to do with a run you may have to show someone — stdout block-buffers while
    // stderr does not, so the summary landed ABOVE the evidence it summarises. Two lines
    // of code and the log becomes the document the terminal showed.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    match outcome {
        Ok(()) => 0,
        Err(e) => {
            // A message that names its own verdict word KEEPS it: "NOT DONE — 1 waiting",
            // "CHECK ONLY — nothing was written", "… COULD NOT RUN", "… NARROWED", "…
            // FAILED — fix the tree". Printing FAILED in front of those contradicts the
            // message, or doubles it. The exit code stays 1 either way: a script must
            // never read "waiting" as ready.
            eprintln!("{}", failure_line(&e.to_string()));
            1
        }
    }
}

/// Verdict words a failure message may already carry; [`failure_line`] keeps them.
const OWN_VERDICT: [&str; 5] = [
    "NOT DONE",
    "CHECK ONLY",
    "COULD NOT RUN",
    "NARROWED",
    "FAILED",
];

/// The last line a failed verb prints: `FAILED —` in front of the message unless
/// the message names its own verdict.
pub fn failure_line(msg: &str) -> String {
    let own = OWN_VERDICT.iter().any(|w| match *w {
        "NOT DONE" | "CHECK ONLY" => msg.starts_with(w),
        _ => msg.contains(&format!(" {w} —")) || msg.contains(&format!(" {w}:")),
    });
    if own {
        format!("aterm-release: {msg}")
    } else {
        format!("aterm-release: FAILED — {msg}")
    }
}

/// The parser (unit-tested in tests/it/resume.rs), pure.
pub fn parse(args: &[String]) -> std::result::Result<Cmd, String> {
    let mut it = args.iter().map(String::as_str);
    let Some(cmd) = it.next() else {
        return Ok(Cmd::Help);
    };
    match cmd {
        "help" | "--help" | "-h" => Ok(Cmd::Help),
        "cut" => parse_cut(&mut it),
        "build" => match it.next() {
            None => Ok(Cmd::Build),
            Some(extra) => Err(format!(
                "build takes no arguments (got {extra:?}): it builds what this clone's release \
                 journal reserved"
            )),
        },
        "release" => parse_release(&mut it).map(Cmd::Release),
        "linux-build" => {
            let mut values = std::collections::BTreeMap::new();
            while let Some(flag) = it.next() {
                if !["--version", "--build-number", "--commit", "--out"].contains(&flag) {
                    return Err(format!("unknown linux-build flag {flag:?}"));
                }
                let value = it
                    .next()
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                if value.is_empty() || values.insert(flag, value).is_some() {
                    return Err(format!("empty or duplicated linux-build {flag}"));
                }
            }
            let version = normalize_version(
                values
                    .get("--version")
                    .ok_or("linux-build needs --version")?,
            )?;
            let build = values
                .get("--build-number")
                .ok_or("linux-build needs --build-number")?
                .parse::<u64>()
                .map_err(|_| "linux-build requires a positive u64 build number")?;
            let commit = values
                .get("--commit")
                .ok_or("linux-build needs --commit")?
                .to_string();
            crate::buildplan::linux::check_identity(&version, build, &commit)?;
            let out =
                std::path::PathBuf::from(values.get("--out").ok_or("linux-build needs --out")?);
            Ok(Cmd::LinuxBuild {
                version,
                build,
                commit,
                out,
            })
        }
        "provision" => {
            let mut id: Option<String> = None;
            let mut check = false;
            let mut cert_dir: Option<std::path::PathBuf> = None;
            while let Some(flag) = it.next() {
                match flag {
                    "--id" => {
                        if id.is_some() {
                            return Err("--id given twice".to_string());
                        }
                        id = Some(
                            it.next()
                                .ok_or("--id needs a machine id (e.g. m2)")?
                                .to_string(),
                        );
                    }
                    "--check" => check = true,
                    "--cert-dir" => {
                        if cert_dir.is_some() {
                            return Err("--cert-dir given twice".to_string());
                        }
                        let dir = it
                            .next()
                            .ok_or("--cert-dir needs a folder (e.g. --cert-dir ~/Downloads)")?;
                        if dir.is_empty() {
                            return Err(
                                "--cert-dir needs a folder, not an empty string".to_string()
                            );
                        }
                        cert_dir = Some(std::path::PathBuf::from(dir));
                    }
                    other => return Err(format!("unknown provision flag {other:?}")),
                }
            }
            let id = id.ok_or(
                "provision needs --id <machine-id> — the roster name this machine signs \
                 under (e.g. targo --unverified ship provision --id m2)",
            )?;
            Ok(Cmd::Provision {
                id,
                check,
                cert_dir,
            })
        }
        "status" => {
            if let Some(extra) = it.next() {
                return Err(format!("status takes no arguments (got {extra:?})"));
            }
            Ok(Cmd::Status)
        }
        "check" => {
            let mut commit: Option<String> = None;
            while let Some(flag) = it.next() {
                match flag {
                    "--commit" => {
                        if commit.is_some() {
                            return Err("--commit given twice".to_string());
                        }
                        let rev = it
                            .next()
                            .ok_or("--commit needs a commit (a sha or a ref)")?;
                        if rev.is_empty() || rev.starts_with('-') {
                            return Err(format!("--commit needs a commit, got {rev:?}"));
                        }
                        commit = Some(rev.to_string());
                    }
                    other => {
                        return Err(format!("check takes only --commit <REV> (got {other:?})"));
                    }
                }
            }
            Ok(Cmd::Check { commit })
        }
        "recover" => {
            let version = normalize_version(
                it.next()
                    .ok_or("recover needs a version and full claim SHA")?,
            )?;
            let owner = it
                .next()
                .ok_or("recover needs the full claim SHA (the cut's `lock` line prints it)")?
                .to_string();
            let acknowledgement = it.next().ok_or(
                "recover requires --old-publisher-stopped after independently proving the old publisher exited",
            )?;
            if acknowledgement != publish::RECOVERY_STOPPED_PROCESS_FLAG {
                return Err(format!(
                    "recover requires --old-publisher-stopped, got {acknowledgement:?}"
                ));
            }
            // Recovery signs, so it accepts the credentials flag: the key must be
            // named, never ambient.
            let mut release_credentials: Option<std::path::PathBuf> = None;
            let mut no_draft_posted = false;
            while let Some(arg) = it.next() {
                if arg == "--release-credentials" {
                    if release_credentials.is_some() {
                        return Err("--release-credentials given twice".to_string());
                    }
                    release_credentials = Some(std::path::PathBuf::from(
                        it.next().ok_or("--release-credentials needs a path")?,
                    ));
                } else if arg == publish::RECOVERY_NO_DRAFT_POSTED_FLAG {
                    if no_draft_posted {
                        return Err("--no-draft-was-posted given twice".to_string());
                    }
                    no_draft_posted = true;
                } else {
                    return Err(format!(
                        "recover takes a version, full claim SHA, --old-publisher-stopped, \
                         and optionally --release-credentials / --no-draft-was-posted \
                         (got {arg:?})"
                    ));
                }
            }
            Ok(Cmd::Recover {
                version,
                owner,
                release_credentials,
                no_draft_posted,
            })
        }
        "verify" => {
            let version = it.next().map(normalize_version).transpose()?;
            if let Some(extra) = it.next() {
                return Err(format!("verify takes at most one version (got {extra:?})"));
            }
            Ok(Cmd::Verify { version })
        }
        "yank" => {
            let build = it.next().ok_or(
                "yank needs the bad release's build number: targo --unverified ship yank <build> \
                 --release-credentials <profile.toml> --mac-only|--linux-artifacts DIRECTORY",
            )?;
            let build: u64 = build
                .parse()
                .map_err(|_| format!("yank: {build:?} is not a build number (u64)"))?;
            // With the paper master armed, the successor cut a yank publishes
            // refuses pre-claim unless it is told which credentials profile signs —
            // so a yank that cannot forward that answer cannot retire a bad build
            // at all. Same flag, same spelling, same meaning as `cut`.
            //
            // The Linux declaration is forwarded too: the successor is a real
            // cut, and a real cut that says nothing about Linux is refused.
            //
            // Nothing else: every other cut flag either contradicts what
            // a yank's successor is (--dry-run/--rehearse publish nothing to
            // prove, --resume belongs to the journal) or is the yank's own
            // decision (--min-build is fixed at bad build + 1), so accepting one
            // could only mean ignoring it.
            let mut opts = verify::YankOptions::default();
            while let Some(flag) = it.next() {
                match flag {
                    "--release-credentials" => {
                        if opts.release_credentials.is_some() {
                            return Err("--release-credentials given twice".to_string());
                        }
                        opts.release_credentials = Some(std::path::PathBuf::from(
                            it.next().ok_or("--release-credentials needs a path")?,
                        ));
                    }
                    "--linux-artifacts" | "--linux-worker" | "--mac-only" | "--linux-target" => {
                        yank_linux(&mut opts).take(flag, &mut it)?;
                    }
                    RETIRED_STRAND_FLAG => return Err(RETIRED_STRAND_REFUSAL.to_string()),
                    extra => {
                        return Err(format!(
                            "yank takes one build number, optionally --release-credentials \
                             <profile.toml>, and the cut's Linux flags (got {extra:?})"
                        ));
                    }
                }
            }
            let linux = yank_linux(&mut opts);
            linux.check()?;
            // The successor is a real cut, so it declares Linux like one.
            if !linux.declared() {
                return Err(format!("yank publishes a cut first; {MAC_ONLY_REFUSAL}"));
            }
            Ok(Cmd::Yank { build, opts })
        }
        "trust" => crate::trust_facts::parse_cmd(&mut it).map(Cmd::Trust),
        other => Err(format!("unknown command {other:?}")),
    }
}

/// `release [--bump | --commit REV] (--mac-only | --linux-artifacts DIR …)
/// [--release-credentials P] [--engine DIR] [--dry-run] [--verbose] [--forget]`.
/// The Linux declaration is not required here: a release in progress has it
/// journaled, and step 1 refuses its absence with the remedy.
fn parse_release<'a>(
    it: &mut impl Iterator<Item = &'a str>,
) -> std::result::Result<crate::release::ReleaseOptions, String> {
    let mut opts = crate::release::ReleaseOptions::default();
    let path = |flag: &str, value: Option<&str>| match value {
        Some(v) if !v.is_empty() && !v.starts_with('-') => Ok(std::path::PathBuf::from(v)),
        _ => Err(format!("{flag} needs a path")),
    };
    while let Some(flag) = it.next() {
        match flag {
            "--bump" => opts.bump = true,
            "--commit" => {
                let rev = it.next().filter(|r| !r.is_empty() && !r.starts_with('-'));
                let rev = rev.ok_or("--commit needs a revision")?;
                if opts.commit.replace(rev.to_string()).is_some() {
                    return Err("--commit given twice".into());
                }
            }
            "--linux-artifacts" | "--linux-worker" | "--mac-only" | "--linux-target" => {
                LinuxFlags {
                    artifacts: &mut opts.linux.artifacts,
                    targets: &mut opts.linux.targets,
                    workers: &mut opts.linux.workers,
                    mac_only: &mut opts.linux.mac_only,
                }
                .take(flag, it)?;
            }
            "--release-credentials" => {
                let p = path(flag, it.next())?;
                if opts.release_credentials.replace(p).is_some() {
                    return Err("--release-credentials given twice".into());
                }
            }
            "--engine" => {
                let p = path(flag, it.next())?;
                if opts.engine.replace(p).is_some() {
                    return Err("--engine given twice".into());
                }
            }
            "--dry-run" => opts.dry_run = true,
            "--verbose" | "-v" => opts.verbose = true,
            "--forget" => opts.forget = true,
            "--abandon" => {
                let v = it.next().ok_or("--abandon needs a version (vX.Y.Z)")?;
                if opts.abandon.replace(normalize_version(v)?).is_some() {
                    return Err("--abandon given twice".into());
                }
            }
            other => return Err(format!("unknown release flag {other:?}")),
        }
    }
    if opts.abandon.is_some()
        && (opts.bump
            || opts.commit.is_some()
            || opts.dry_run
            || opts.forget
            || opts.linux.declared()
            || !opts.linux.workers.is_empty()
            || !opts.linux.targets.is_empty())
    {
        return Err(
            "--abandon withdraws a claimed cut and takes no other release flag (but \
             --release-credentials, --engine, --verbose)"
                .into(),
        );
    }
    // --forget drops the journal and exits, so every flag of a run is one it would
    // ignore — a dry run's "nothing changes" among them
    if opts.forget
        && (opts.bump
            || opts.commit.is_some()
            || opts.dry_run
            || opts.linux.declared()
            || !opts.linux.workers.is_empty()
            || !opts.linux.targets.is_empty())
    {
        return Err(
            "--forget drops this clone's release journal and exits; it takes no other release \
             flag (but --release-credentials, --engine, --verbose) — a dry run changes nothing, \
             and a release after it is a run of its own"
                .into(),
        );
    }
    if opts.bump && opts.commit.is_some() {
        return Err(
            "--bump releases the bump it commits; --commit names another commit — pass one".into(),
        );
    }
    Ok(opts)
}

/// The Linux declaration a cut makes, and a yank forwards to its successor cut:
/// `--linux-artifacts DIRECTORY [--linux-target ARCH]... [--linux-worker
/// ARCH=HOST:REPO]... | --mac-only`.
struct LinuxFlags<'o> {
    artifacts: &'o mut Option<std::path::PathBuf>,
    targets: &'o mut Vec<String>,
    workers: &'o mut Vec<String>,
    mac_only: &'o mut bool,
}

impl LinuxFlags<'_> {
    /// Consume one of the four Linux flags (and its value).
    fn take<'a>(
        &mut self,
        flag: &str,
        it: &mut impl Iterator<Item = &'a str>,
    ) -> std::result::Result<(), String> {
        match flag {
            "--linux-artifacts" => {
                if self.artifacts.is_some() {
                    return Err("--linux-artifacts given twice".into());
                }
                *self.artifacts = Some(std::path::PathBuf::from(
                    it.next().ok_or("--linux-artifacts needs a directory")?,
                ));
            }
            "--linux-worker" => {
                let spec = it
                    .next()
                    .ok_or("--linux-worker needs ARCH=DESTINATION:REPOSITORY")?;
                crate::buildplan::linux::Worker::parse(spec)?;
                self.workers.push(spec.to_string());
            }
            "--mac-only" => *self.mac_only = true,
            _ => {
                let triple = match it.next().ok_or("--linux-target needs aarch64 or x86_64")? {
                    "aarch64" => "aarch64-unknown-linux-gnu",
                    "x86_64" => "x86_64-unknown-linux-gnu",
                    other => {
                        return Err(format!("unsupported Linux release architecture {other:?}"));
                    }
                };
                if self.targets.iter().any(|t| t == triple) {
                    return Err("duplicate --linux-target".into());
                }
                self.targets.push(triple.into());
            }
        }
        Ok(())
    }

    /// The flags must agree with each other.
    fn check(&self) -> std::result::Result<(), String> {
        if !self.targets.is_empty() && self.artifacts.is_none() {
            return Err("--linux-target requires --linux-artifacts".into());
        }
        if !self.workers.is_empty() && self.artifacts.is_none() {
            return Err("--linux-worker requires --linux-artifacts (where its pair lands)".into());
        }
        if *self.mac_only && self.artifacts.is_some() {
            return Err("--mac-only and --linux-artifacts contradict each other".into());
        }
        Ok(())
    }

    /// Whether a declaration was made at all.
    fn declared(&self) -> bool {
        *self.mac_only || self.artifacts.is_some()
    }
}

/// The Linux declaration a yank forwards to its successor cut.
fn yank_linux(opts: &mut verify::YankOptions) -> LinuxFlags<'_> {
    LinuxFlags {
        artifacts: &mut opts.linux_artifacts,
        targets: &mut opts.linux_targets,
        workers: &mut opts.linux_workers,
        mac_only: &mut opts.mac_only,
    }
}

fn parse_cut<'a>(it: &mut impl Iterator<Item = &'a str>) -> std::result::Result<Cmd, String> {
    let mut opts = publish::CutOptions::default();
    let mut abandon: Option<String> = None;
    while let Some(flag) = it.next() {
        match flag {
            RETIRED_UNMIRRORED_FLAG => return Err(RETIRED_UNMIRRORED_REFUSAL.to_string()),
            "--dry-run" => opts.dry_run = true,
            "--resume" => opts.resume = true,
            "--linux-artifacts" | "--linux-worker" | "--mac-only" | "--linux-target" => {
                LinuxFlags {
                    artifacts: &mut opts.linux_artifacts,
                    targets: &mut opts.linux_targets,
                    workers: &mut opts.linux_workers,
                    mac_only: &mut opts.mac_only,
                }
                .take(flag, it)?;
            }
            RETIRED_STRAND_FLAG => return Err(RETIRED_STRAND_REFUSAL.to_string()),
            // The ONE signing input. A path in the command, never an ambient file:
            // "what signed this?" is answered by reading the command that ran.
            "--release-credentials" => {
                if opts.release_credentials.is_some() {
                    return Err("--release-credentials given twice".to_string());
                }
                opts.release_credentials = Some(std::path::PathBuf::from(
                    it.next().ok_or("--release-credentials needs a path")?,
                ));
            }
            "--abandon" => {
                let v = it.next().ok_or("--abandon needs a version (vX.Y.Z)")?;
                abandon = Some(normalize_version(v)?);
            }
            "--min-build" => {
                let n = it.next().ok_or("--min-build needs a number")?;
                let n: u64 = n
                    .parse()
                    .map_err(|_| format!("--min-build: {n:?} is not a u64"))?;
                opts.min_build = Some(n);
            }
            "--rehearse" => {
                let slug = it.next().ok_or("--rehearse needs OWNER/REPO")?;
                let ok = matches!(slug.split('/').collect::<Vec<_>>().as_slice(),
                    [o, r] if !o.is_empty() && !r.is_empty());
                if !ok {
                    return Err(format!("--rehearse: {slug:?} is not OWNER/REPO"));
                }
                opts.rehearse = Some(slug.to_string());
            }
            other => return Err(format!("unknown cut flag {other:?}")),
        }
    }
    // Mode exclusivity: each of abandon/resume is a whole flow of its own —
    // silently ignoring a second flag would do something the operator did not
    // ask for, on the one command where that costs a burned ledger number.
    if abandon.is_some()
        && (opts.dry_run
            || opts.resume
            || opts.linux_artifacts.is_some()
            || !opts.linux_workers.is_empty()
            || opts.mac_only
            || opts.min_build.is_some()
            || opts.rehearse.is_some())
    {
        return Err("--abandon combines with no other cut flag".to_string());
    }
    if opts.resume
        && (opts.dry_run
            || opts.linux_artifacts.is_some()
            || !opts.linux_workers.is_empty()
            || opts.mac_only
            || opts.min_build.is_some()
            || opts.rehearse.is_some())
    {
        return Err(
            "--resume combines with no other cut flag (the journal already fixed the \
             cut's parameters)"
                .to_string(),
        );
    }
    if opts.dry_run && opts.rehearse.is_some() {
        return Err("--dry-run and --rehearse are mutually exclusive".to_string());
    }
    LinuxFlags {
        artifacts: &mut opts.linux_artifacts,
        targets: &mut opts.linux_targets,
        workers: &mut opts.linux_workers,
        mac_only: &mut opts.mac_only,
    }
    .check()?;
    // LINUX IS DECLARED, NEVER DEFAULTED AWAY. A cut without `--linux-artifacts`
    // would go out Mac-only in silence, leaving every Linux install on the build it
    // had. Refused here — before a gate, a
    // lease or a claim — unless the operator says `--mac-only` out loud. A dry
    // run publishes nothing and a resume or abandon already has its shape.
    if abandon.is_none()
        && !opts.resume
        && !opts.dry_run
        && !opts.mac_only
        && opts.linux_artifacts.is_none()
    {
        return Err(MAC_ONLY_REFUSAL.to_string());
    }
    Ok(Cmd::Cut { opts, abandon })
}

/// Accept "0.2.0" or "v0.2.0"; store the bare canonical MAJOR.MINOR.PATCH
/// everywhere. Two-component spellings are the retired scheme and are refused.
fn normalize_version(v: &str) -> std::result::Result<String, String> {
    let bare = v.strip_prefix('v').unwrap_or(v);
    ledger::check_version_shape(bare).map_err(|e| e.to_string())?;
    Ok(bare.to_string())
}

fn dispatch(cmd: Cmd) -> ledger::Result<()> {
    match cmd {
        Cmd::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Cmd::LinuxBuild {
            version,
            build,
            commit,
            out,
        } => crate::buildplan::linux::run_worker(&repo_root()?, &out, &version, build, &commit)
            .map_err(Error::new),
        Cmd::Build => crate::artifact::run_build(&repo_root()?),
        Cmd::Cut {
            abandon: Some(v), ..
        } => verify::run_abandon(&repo_root()?, &v),
        Cmd::Cut { opts, .. } => publish::run_cut(&repo_root()?, &opts),
        Cmd::Provision {
            id,
            check,
            cert_dir,
        } => crate::provision::run_provision(&repo_root()?, &id, check, cert_dir.as_deref()),
        Cmd::Release(opts) => match &opts.abandon {
            Some(version) => verify::run_abandon(&repo_root()?, version),
            None => match crate::release_steps::run(&repo_root()?, &opts) {
                0 => Ok(()),
                code => Err(Error::new(format!("release exited {code}"))),
            },
        },
        Cmd::Status => verify::run_status(&repo_root()?),
        Cmd::Check { commit } => {
            crate::gates::run_check(&repo_root()?, commit.as_deref().unwrap_or("HEAD"))
        }
        Cmd::Recover {
            version,
            owner,
            release_credentials,
            no_draft_posted,
        } => {
            // Recovery signs too, so it resolves credentials the SAME one-path way
            // as a fresh cut: the flag when given, else this machine's provisioned
            // identity. Different resolution here would mean a cut and its recovery
            // could sign as different machines.
            let creds = crate::sign::ReleaseCredentials::resolve(
                release_credentials.as_deref(),
                &repo_root()?,
            )?;
            publish::run_recover_lost(
                &repo_root()?,
                &version,
                &owner,
                true,
                no_draft_posted,
                creds.as_ref(),
            )
        }
        Cmd::Verify { version } => verify::run_verify(&repo_root()?, version),
        Cmd::Yank { build, opts } => verify::run_yank(&repo_root()?, build, &opts),
        Cmd::Trust(t) => match crate::trust_facts::run_cmd(&repo_root()?, t) {
            0 => Ok(()),
            code => Err(Error::new(format!("trust exited {code}"))),
        },
    }
}

/// The workspace root, from git — the `targo --unverified ship` alias may be invoked from
/// any subdirectory of the checkout, and every pipeline path is repo-relative.
fn repo_root() -> ledger::Result<std::path::PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| Error::new(format!("failed to run git rev-parse --show-toplevel: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(
            "not inside a git checkout — run `targo --unverified ship` from the aterm workspace"
                .to_string(),
        ));
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(root))
}
