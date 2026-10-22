// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI surface (release spec §5): hand-rolled `std::env::args` parsing (no
//! third-party arg crate — same rule as `aterm-ctl`) for the whole command
//! surface: `release [--bump | --commit REV] (--mac-only | --linux-artifacts DIR …)
//! [--release-credentials <profile.toml>] [--engine DIR] [--dry-run] [--verbose]
//! [--prepare] [--forget | --abandon vX.Y.Z]`, `build` (the release bundle, or
//! HEAD's provisional bundle between releases), `cut [--dry-run]
//! [--resume] [--abandon vX.Y.Z] [--min-build N]
//! [--release-credentials <profile.toml>]` (the release's `cut`),
//! `recover vX.Y.Z <claim-sha> --old-publisher-stopped`, `status`,
//! `check [--commit REV] [--engine DIR] [--cut] [--export] [--wasm32]`,
//! `trust run|judge`,
//! `verify [vX.Y.Z]`, `site [--latest] [--dry-run] …` (alab.systems brought to a release),
//! `hook pre-promote|post-promote|transforms` (the publication engine's hooks),
//! `yank <build> [--release-credentials <profile.toml>] --mac-only|--linux-artifacts DIRECTORY`,
//! `dev-app [identity] …` ([`crate::dev_app::USAGE`]).

use std::process::Command;

use crate::ledger::{self, Error};
use crate::{publish, verify};

/// Why a real cut that says nothing about Linux is refused.
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
                 [--dry-run] [--verbose] [--prepare] [--forget | --abandon vX.Y.Z]
      The way to release aterm, from a clean clone of its own on main. Steps:
      checks, sync, commit, stage, build (the one build of the bundle), verify
      (`verify release` on those bytes), ready, publish (pub publish), cut (claim,
      then sign the verified bytes), after-cut (the published release's shape, the
      site, the handoff fixtures on main) and check (the channel's latest). A step
      whose work is already done for the release commit is skipped, so running it
      again resumes. A gap only the owner can close is asked once on the owner's
      aterm while the long steps run, and waited on at the publish. Ends in one
      line: RELEASED …, or REFUSED at <step>: <why> — <what is public>. <remedy>.
      Exit 0 released, prepared, or a dry run with nothing refused · 1 refused ·
      2 usage · 3 a resumable stop (run it again) · 130 interrupted.
        --bump             commit the next MINOR on origin/main's tip, push it,
                           release it
        --commit REV       release this commit of origin/main; replaces, out
                           loud, a candidate this clone has not published
        --mac-only / --linux-artifacts …
                           the Linux declaration the cut requires (see `cut`);
                           journaled, so a bare re-run keeps it
        --release-credentials <profile.toml>
                           the cut's signing profile (default
                           ~/.aterm/release-credentials.toml)
        --engine DIRECTORY the publication engine (default
                           $PUBLICATION_ENGINE, else ~/publication)
        --dry-run          the plan, the ETA, and every refusal a real run would
                           meet; nothing changes
        --verbose          stream every step's output, not only into its log
        --prepare          run through stage, build, verify and ready, then stop
                           PREPARED, nothing public. A bare `ship release`
                           finishes it after re-checking the prover, the
                           provisioning, the tag, the ledger tail and the engine
                           (a mismatch refuses before the claim); --prepare again
                           redoes a stale prepare. With --dry-run: both halves
        --forget           drop this clone's release journal, withdraw its open
                           asks, and exit; nothing public changes. Takes no flag
                           of a run
        --abandon vX.Y.Z   withdraw this clone's claimed, unpublished cut of
                           vX.Y.Z (its draft or uploads, its origin tag, its
                           journal; the claim commit stays). Run it from the
                           release's clone: only the claim's own cutter can

  targo --unverified ship build
      The one build of a release bundle: both slices at once from a snapshot of
      the commit's git objects (the x86_64 slice run under Rosetta), laid out
      unsigned in the release cache — or found there, its bytes re-proved. `ship
      release` runs it for the build its journal reserved. With no release in
      progress it builds HEAD under a provisional number (max(HEAD's ledger tail
      + 1, now), claimed nowhere) and prints the .app for `verify measure --app`.

  targo --unverified ship linux-build --version X.Y.Z --build-number N
                 --commit FULL-CLAIM-SHA --out EXISTING-PRIVATE-DIRECTORY
      On native x86_64 or aarch64 Linux, in a clean checkout at the claim commit:
      build aterm with the sealed Trust toolchain and write it with its provenance
      receipt into --out. Nothing is signed or published. Run it on both
      architectures and hand the directory to the cutter (--linux-artifacts), or
      let the cutter drive the hosts (--linux-worker).

  targo --unverified ship cut [--dry-run] [--resume] [--abandon vX.Y.Z]
                 [--min-build N] [--rehearse OWNER/REPO]
                 [--release-credentials <profile.toml>]
                 [--linux-artifacts DIRECTORY [--linux-target aarch64|x86_64]...
                  [--linux-worker ARCH=HOST:REPO]... | --mac-only]
      The cut step of `ship release`, which runs it; release with `ship release`.
      Gates, the claim of the release's reserved build, a byte copy of the
      verified bundle signed, notarized and packaged (the cut never builds), the
      tag, and one publication onto the release channel, made the head last. Only
      the release commit's own cutter cuts it. A real or rehearsal cut must name
      --linux-artifacts or --mac-only.
        --dry-run          the gates, then the verified bundle copied, signed into
                           dist/ and notarized under its reserved number; nothing
                           claimed, committed, published or built
        --resume           re-enter the journaled cut (dist/cut-state.toml) at
                           its first incomplete step
        --abandon vX.Y.Z   withdraw an unpublished cut: its own draft deleted, or
                           exactly its uploads taken back off the engine's source
                           release; its origin tag and the local journal deleted.
                           The claim commit stays; a later cut recuts
        --min-build N      raise the manifest's apply floor to at least N
        --rehearse O/R     a full real cut of the verified bundle, published to
                           the scratch channel O/R (which must be public) under
                           its reserved number; no ledger push, no tag on origin
        --linux-artifacts DIRECTORY
                           the Linux handoff directory, required complete before
                           signing; a missing handoff pauses the journaled cut
                           for --resume
        --linux-target ARCH
                           ship only these architectures (repeatable); default
                           both
        --linux-worker ARCH=HOST:REPO
                           after the claim, build ARCH on HOST over ssh (a
                           throwaway worktree of REPO at the claim commit) and
                           copy its pair into --linux-artifacts, checked like a
                           hand-carried one
        --mac-only         ship no Linux build, on purpose
        --release-credentials <profile.toml>
                           the signing profile: a rostered, unrevoked machine key
                           and its notary credential (`provision` writes
                           ~/.aterm/release-credentials.toml). Without one the
                           cut refuses before it claims

  targo --unverified ship provision --id <machine-id> [--check] [--cert-dir <folder>]
                           make this machine a publisher (without the Trust
                           toolchain, run `aterm pkg install trust` first). Seeds
                           the newest master-signed roster pair from the channel
                           into dist/, runs `targo --unverified verify provision`
                           (rustup, the stable slices, the wasm32 pin, fish,
                           pwsh, binaryen, the browsers) and rustup's trust link,
                           audits the whole publishing stack (the Trust toolchain
                           and its drivers, the stable slices, the base layer,
                           the develop layer, LibreSSL, the publication engine,
                           the site checkout, node and firebase-tools, the Apple
                           identity, a live-tested notary credential, the
                           credentials profile, gh auth, the channel token) and
                           prints every gap with its remedy. Only on a clean
                           pass does it mint this machine's key, by the join
                           ceremony: the paper phrase, typed once, is the only
                           input, and a roster id is never spent on a machine
                           that cannot release. A provisioned machine is audited,
                           never re-minted. Ends READY TO CUT
        --cert-dir <folder>
                           the one visible folder the Apple certificate errand
                           may use: the request is copied there and the issued
                           .cer looked for there and in ~/.aterm/apple. Without
                           it nothing outside ~/.aterm/apple is read, so no folder
                           macOS guards (~/Downloads) raises a dialog
        --check            audit only: nothing minted or written. On a machine
                           that holds its key it exits non-zero while anything is
                           open

  targo --unverified ship status        version · ledger tail · dangling claims · newest
                           published build
  targo --unverified ship check [--commit REV] [--engine DIRECTORY]
                 [--cut] [--export] [--wasm32]
                           would a release of REV (default HEAD) pass? Read-only;
                           naming sections narrows it to them, none is all three:
        --cut              the cut's pre-claim gates that read only the commit
                           and this clone's records (cutter identity, release
                           notes, the predecessor's handoff fixtures, the handoff
                           policy, the `verify release` record), in seconds.
                           Answered only by REV's own cutter (built from REV),
                           else it could not run. publish/pre-promote runs this
        --export           the publication engine's export guards (forbidden
                           content, private references, gitleaks, cargo metadata,
                           README versions, dead links): its own `pub stage aterm
                           --dry-run --no-check` over a scratch clone of REV,
                           about a minute. Judges only a REV on origin/main; needs
                           the network, the engine (--engine, else
                           $PUBLICATION_ENGINE, else ~/publication), gitleaks,
                           and rustup's `trust` toolchain
        --wasm32           publish/config.sh's wasm32 clause, on its stock pin,
                           over the export
      Exits 0 READY, 1 a gate would refuse, 2 REV names no release, 3 a gate
      could not run (each line names its fix), 130 interrupted
  targo --unverified ship trust run (-p <crate>... | --whole-tree | --remeasure)
        [--jobs N|auto] [--target-root DIR] [--crate-budget SECONDS] [--prover BIN-DIR]
        [--update-ratchet] [--try-refused] [--obligations] [--stop-if-narrowed]
                           the Trust lane: `targo trust check -p <lib> --lib
                           --allow-l0-gaps` over the named libraries (-p <lib>
                           also selects its feature rows, such as
                           aterm-link+sealed), every workspace library and
                           vendored fork (--whole-tree), or what this tree's
                           stored facts left undecided (--remeasure). Each check
                           runs in its own slot under --target-root (default
                           <repo>/target-trust), is ended OVER-BUDGET at
                           --crate-budget (default 21600 s), and is held to
                           crates/aterm-release/trust/floors.tsv. A whole-tree run
                           on a clean checkout files the facts `verdict`, `judge`
                           and `pending` read. --update-ratchet raises the floors
                           a run meets and keeps trust/memory.tsv current;
                           --try-refused verifies what this host would refuse for
                           memory; --stop-if-narrowed stops before any check when
                           it would; --obligations keeps each check's
                           per-obligation record. Exit 0 pass, 1 fail, 2 nothing
                           decided, 4 narrowed by memory
  targo --unverified ship trust verdict [--commit REV] [--prover BIN-DIR]
                           the whole-tree lane's verdict for REV (default HEAD),
                           judged now: the facts a whole-tree run filed for REV's
                           tree (its floors aside), by the prover (--prover names
                           its bin/; default the cutter's Trust toolchain), held to
                           REV's floors. A floors-only commit reuses the facts
                           filed before it, its floors held to the up-only rule.
                           One line; exit 0 VERIFIED, 1 not, 2 could not answer
  targo --unverified ship trust judge [--commit REV] [--prover BIN-DIR] [--update-ratchet [--reason WHY]]
                           the lane's full report for REV's stored facts, judged
                           against the working tree's floors. --update-ratchet
                           writes what the facts meet (a first floor, a raise, a
                           corrected total, a re-base onto this prover), never a
                           lowering; with --reason WHY it lowers onto this prover a
                           floor another prover recorded that the facts fall
                           below, WHY in its fifth column. A working floor the
                           up-only rule refuses is named LOWERED, and fails
  targo --unverified ship trust pending [--commit REV] [--prover BIN-DIR]
                           the libraries REV's stored facts leave undecided
                           (trustd did not admit them, or lapsed budgets could
                           cover their shortfall), one per line: what `trust run
                           --remeasure` measures again
  targo --unverified ship trust candidate --candidate <staged toolchain dir>
        [--base <bin> | --base-build N] [--crate-budget SECONDS] [--jobs N|auto]
        [--out DIR] [--min-free-gib N] [--no-guard | [--guard-footprint-gib G]
        [--guard-swap-gib G] [--guard-level PCT] [--guard-min-free-gib G]]
        [--measure-narrowed] [--public-repo OWNER/REPO] [--retractions FILE]
        [--trust-repo DIR] [--dry-run]
  targo --unverified ship trust candidate --admits <staged toolchain dir>
        [--receipts DIR] [--base-build N] [--public-repo OWNER/REPO]
                           the candidate gate for publishing a Trust toolchain:
                           two whole-tree lane runs of this clean checkout on this
                           machine, the published seal's (the build the public
                           channel pins) then the candidate's, compared by `trust
                           compare`. Each check runs under a memory guard that
                           ends it at a process-tree footprint of 0.6 × RAM, 8 GiB
                           of swap, memorystatus_level under 10, or free space
                           2 GiB below --min-free-gib (the --guard-* flags move
                           these). The RECEIPT under --out is what the trust pack
                           publisher admits. Exit 0 PUBLISH-OK, 1 PUBLISH-REFUSED,
                           2 could not compare, 3 no verdict (--dry-run).
                           --admits asks the publisher's question: did the newest
                           gate run for that directory pass within 7 days,
                           against the build the channel pins now, on an aterm
                           commit this checkout contains? Exit 0 admitted,
                           1 refused, 2 could not check
  targo --unverified ship trust compare --base <plan> --candidate <plan>
        [--base-logdir DIR] [--candidate-logdir DIR] [--tsv FILE] [--base-prover ID]
        [--ratchet FILE] [--retractions FILE] [--trust-repo DIR]
        [--candidate-trust-commit SHA] [--base-trust-commit SHA] [--obligations-tsv FILE]
                           the candidate gate's comparison of two whole-tree runs
                           of one aterm tree on one machine (each run's <target
                           root>/logs/.plan), the published seal's and a
                           candidate's, obligation by obligation. It refuses an
                           obligation the seal proved and the candidate does not,
                           whatever it gains elsewhere, unless the retraction
                           ledger (--retractions, read in --trust-repo) retracts
                           it as a false proof; a library with no result in its
                           budget or no usable record; and, where the seal's
                           check gave no result, a candidate below the floor the
                           seal's prover recorded (--ratchet, default the base
                           plan's) or with none. --obligations-tsv lists each
                           lost obligation by its ledger key. Exit 0 PUBLISH-OK,
                           1 PUBLISH-REFUSED, 2 could not compare (the line says
                           why: runs that differ in tree, machine, scope, budget
                           or slots, an unconcluded run, one prover twice, an
                           unreadable ledger)
  targo --unverified ship recover vX.Y.Z <full-claim-sha> --old-publisher-stopped
        [--release-credentials <profile.toml>] [--no-draft-was-posted]
                           finish or withdraw the cut of a machine that died
                           mid-cut: withdrawn if unpublished, verified and
                           finished if published. Run it only once the old
                           cutter process has exited; --old-publisher-stopped
                           says it has.
        --no-draft-was-posted
                           you checked the channel's releases page and it shows
                           no release for vX.Y.Z (needed when the journal is lost)
  targo --unverified ship verify [vX.Y.Z]
                           re-run the post-publish check of vX.Y.Z (default: the
                           release the channel serves now)
  targo --unverified ship site [--latest] [--dry-run] [--no-deploy] [--skip-site]
                 [--site-dir DIR] [--releases FILE]
                           bring alab.systems to a release: elect the downloads
                           from the channel (the Linux executables proved through
                           the signed chain), render the pages and /releases,
                           rebuild the /terminal engine from the public commit,
                           commit, push, deploy and check it live. The release's
                           after-cut step runs it as --latest. Flags and exit
                           codes: `ship site --help`
  targo --unverified ship hook pre-promote | post-promote | transforms
                           the publication engine's hooks, run by
                           publish/pre-promote, publish/post-promote and
                           publish/transforms.sh (`ship hook --help`)
  targo --unverified ship yank <build> [--release-credentials <profile.toml>]
                 (--linux-artifacts DIRECTORY [--linux-target aarch64|x86_64]...
                  [--linux-worker ARCH=HOST:REPO]... | --mac-only)
                           publish and verify a successor first (a real cut with
                           min_build = <build>+1, signed with
                           --release-credentials, declaring Linux as any cut
                           must), and only then delete the bad build's origin tag
                           and demote its channel release to a prerelease
";

/// A parsed invocation. `Cut.abandon` rides outside [`publish::CutOptions`] because
/// abandoning is not a cut and never reaches the pipeline.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Help,
    LinuxBuild {
        version: String,
        build: u64,
        commit: String,
        out: std::path::PathBuf,
    },
    /// `build`: the release bundle, or HEAD's provisional bundle between releases
    /// ([`crate::artifact::run_build`]).
    Build,
    Cut {
        opts: publish::CutOptions,
        abandon: Option<String>,
    },
    Provision {
        id: String,
        check: bool,
        /// `--cert-dir`: the one visible folder the browser errand may use (the request is
        /// copied there and the certificate looked for there). `None` keeps every read
        /// inside `~/.aterm/apple`; `~/Downloads` is never touched unasked.
        cert_dir: Option<std::path::PathBuf>,
    },
    /// `release`: the one release command ([`crate::release`]).
    Release(crate::release::ReleaseOptions),
    Status,
    /// `check`: would a release of one commit pass ([`crate::release_steps::check`]).
    Check(crate::release_steps::CheckOptions),
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
    /// `dev-app`: this checkout as `aterm (dev).app` ([`crate::dev_app`]).
    DevApp(crate::dev_app::Cmd),
    /// `trust <verb>`: the whole-tree Trust lane's facts and their judge
    /// ([`crate::trust_facts`]).
    Trust(crate::trust_facts::TrustCmd),
    /// `site`: alab.systems brought to a release ([`crate::site`]), which parses its flags.
    Site(Vec<String>),
    /// `hook <name>`: the publication engine's hooks ([`crate::hook`]).
    Hook(Vec<String>),
    /// `tap deploy …` — the Homebrew tap's structure, mirrored ([`crate::tap`]).
    Tap(Vec<String>),
    Yank {
        build: u64,
        /// A yank publishes a successor before it deletes anything, so it takes the cut's
        /// signing inputs. See [`verify::YankOptions`].
        opts: verify::YankOptions,
    },
}

/// Entry point for the whole binary: `main()` delegates here so every path, parsing
/// included, is reachable from tests. Exit codes: 0 ok, 1 pipeline failure, 2 usage error
/// (the trust, check, site, hook and release verbs answer with their own, below).
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm-release: {e}\nusage: {} help", publish::SHIP_COMMAND);
            return 2;
        }
    };
    // The trust verbs answer 0 / 1 / 2 (pass, fail, could not run), which the lane and the
    // release scripts read. `trust prover` and a `trust conclude` that files nothing read
    // no git, so a directory that is no checkout (the lane's self-test) is no refusal.
    if let Cmd::Trust(t) = cmd {
        let root = repo_root()
            .ok()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let code = crate::trust_facts::run_cmd(&root, t);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        return code;
    }
    // `check` answers 0 / 1 / 2 / 3 (ready, refused, no release named, a gate could not
    // run), 130 interrupted.
    if let Cmd::Check(opts) = &cmd {
        return match repo_root() {
            Ok(root) => crate::release_steps::check(&root, opts),
            Err(e) => {
                eprintln!("aterm-release check: {e}");
                2
            }
        };
    }
    // `site` and `hook` answer with their own codes (the engine and the release read them)
    if let Some(code) = own_code(&cmd) {
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
    // Verbs print to stdout, the failure line to stderr: under `2>&1 | tee` stdout
    // block-buffers, so without this flush the line lands above its evidence.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    match outcome {
        Ok(()) => 0,
        Err(e) => {
            // A message that names its own verdict ("NOT DONE — 1 waiting", "… COULD NOT
            // RUN") keeps it rather than gaining a contradicting FAILED; the exit code is
            // 1 either way, so a script never reads "waiting" as ready.
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

/// The parser, pure (tested in tests/it/resume.rs).
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
            let mut opts = crate::release_steps::CheckOptions::default();
            while let Some(flag) = it.next() {
                match flag {
                    "--commit" => {
                        if opts.commit.is_some() {
                            return Err("--commit given twice".to_string());
                        }
                        let rev = it
                            .next()
                            .ok_or("--commit needs a commit (a sha or a ref)")?;
                        if rev.is_empty() || rev.starts_with('-') {
                            return Err(format!("--commit needs a commit, got {rev:?}"));
                        }
                        opts.commit = Some(rev.to_string());
                    }
                    "--engine" => {
                        if opts.engine.is_some() {
                            return Err("--engine given twice".to_string());
                        }
                        let dir = it.next().ok_or("--engine needs a directory")?;
                        if dir.is_empty() || dir.starts_with('-') {
                            return Err(format!("--engine needs a directory, got {dir:?}"));
                        }
                        opts.engine = Some(std::path::PathBuf::from(dir));
                    }
                    "--cut" => opts.cut = true,
                    "--export" => opts.export = true,
                    "--wasm32" => opts.wasm32 = true,
                    other => {
                        return Err(format!(
                            "check takes --commit <REV>, --engine <DIR>, --cut, --export and \
                             --wasm32 (got {other:?})"
                        ));
                    }
                }
            }
            Ok(Cmd::Check(opts))
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
            // Recovery signs, so it takes the credentials flag: the key is named, never
            // ambient.
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
            // a yank's successor is (--dry-run publishes nothing to
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
        "dev-app" => crate::dev_app::parse(&mut it).map(Cmd::DevApp),
        "site" => Ok(Cmd::Site(it.map(str::to_string).collect())),
        "hook" => Ok(Cmd::Hook(it.map(str::to_string).collect())),
        "tap" => Ok(Cmd::Tap(it.map(str::to_string).collect())),
        other => Err(format!("unknown command {other:?}")),
    }
}

/// `release`'s flags. The Linux declaration is not required here: a release in progress
/// has it journaled, and step 1 refuses its absence with the remedy.
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
            "--prepare" => opts.prepare = true,
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
            || opts.prepare
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
    // --forget drops the journal and exits, so any flag of a run would be ignored.
    if opts.forget
        && (opts.bump
            || opts.commit.is_some()
            || opts.dry_run
            || opts.prepare
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
/// `--linux-artifacts DIRECTORY [--linux-target ARCH]... [--linux-worker ARCH=HOST:REPO]...
/// | --mac-only`.
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
            // The signing profile; without it the cut resolves this machine's
            // provisioned key ([`crate::sign::ReleaseCredentials::resolve`]).
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
            other => return Err(format!("unknown cut flag {other:?}")),
        }
    }
    // Abandon and resume are each a whole flow: silently ignoring a second flag would do
    // something unasked on the one command where that burns a ledger number.
    if abandon.is_some()
        && (opts.dry_run
            || opts.resume
            || opts.linux_artifacts.is_some()
            || !opts.linux_workers.is_empty()
            || opts.mac_only
            || opts.min_build.is_some())
    {
        return Err("--abandon combines with no other cut flag".to_string());
    }
    if opts.resume
        && (opts.dry_run
            || opts.linux_artifacts.is_some()
            || !opts.linux_workers.is_empty()
            || opts.mac_only
            || opts.min_build.is_some())
    {
        return Err(
            "--resume combines with no other cut flag (the journal already fixed the \
             cut's parameters)"
                .to_string(),
        );
    }
    LinuxFlags {
        artifacts: &mut opts.linux_artifacts,
        targets: &mut opts.linux_targets,
        workers: &mut opts.linux_workers,
        mac_only: &mut opts.mac_only,
    }
    .check()?;
    // Linux is declared, never defaulted away: a cut without `--linux-artifacts` would
    // leave every Linux install on the build it had, so it is refused here, before any
    // gate, lease or claim, unless it says `--mac-only`. A dry run publishes nothing, and a
    // resume or abandon already has its shape.
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

/// Accept "0.2.0" or "v0.2.0" and store the bare canonical MAJOR.MINOR.PATCH.
fn normalize_version(v: &str) -> std::result::Result<String, String> {
    let bare = v.strip_prefix('v').unwrap_or(v);
    ledger::check_version_shape(bare).map_err(|e| e.to_string())?;
    Ok(bare.to_string())
}

fn dispatch(cmd: Cmd) -> ledger::Result<()> {
    match cmd {
        Cmd::Help => {
            print!("{USAGE}{}", crate::dev_app::USAGE);
            Ok(())
        }
        Cmd::DevApp(crate::dev_app::Cmd::Help) => {
            print!("USAGE{}", crate::dev_app::USAGE);
            Ok(())
        }
        Cmd::DevApp(crate::dev_app::Cmd::Build(opts)) => {
            crate::dev_app::run_build(&repo_root()?, &opts).map_err(Error::new)
        }
        Cmd::DevApp(crate::dev_app::Cmd::Identity { remove, keychain }) => {
            crate::dev_app::run_identity(remove, keychain.as_deref()).map_err(Error::new)
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
        Cmd::Check(opts) => match crate::release_steps::check(&repo_root()?, &opts) {
            0 => Ok(()),
            code => Err(Error::new(format!("check exited {code}"))),
        },
        Cmd::Recover {
            version,
            owner,
            release_credentials,
            no_draft_posted,
        } => {
            // Recovery resolves credentials exactly as a fresh cut does, so a cut and its
            // recovery cannot sign as different machines.
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
        Cmd::Site(_) | Cmd::Hook(_) | Cmd::Tap(_) => match own_code(&cmd) {
            Some(0) | None => Ok(()),
            Some(code) => Err(Error::new(format!("exited {code}"))),
        },
    }
}

/// `site`, `hook` and `tap`, which answer with their own exit codes: the code, or `None`
/// for every other command.
fn own_code(cmd: &Cmd) -> Option<i32> {
    match cmd {
        Cmd::Site(args) => Some(match repo_root() {
            Ok(root) => crate::site::run_cli(args, &root),
            Err(e) => {
                eprintln!("aterm-release site: {e}");
                2
            }
        }),
        Cmd::Hook(args) => Some(crate::hook::run_cli(
            args,
            &std::env::current_dir().unwrap_or_default(),
        )),
        Cmd::Tap(args) => Some(match repo_root() {
            Ok(root) => crate::tap::run_cli(args, &root),
            Err(e) => {
                eprintln!("aterm-release tap: {e}");
                2
            }
        }),
        _ => None,
    }
}

/// The workspace root, from git: `ship` may run from any subdirectory, and every pipeline
/// path is repo-relative.
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
