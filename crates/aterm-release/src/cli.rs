// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI argument parsing for `aterm-release`.

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
      2 usage, or no checkout, HOME or log directory to run in · 3 a resumable
      stop (run it again) · 130 interrupted.
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
        --prepare          run every step through ready, then stop PREPARED,
                           nothing public. A bare `ship release` finishes it
                           after re-checking the prover, the provisioning, the
                           tag, the ledger tail and the engine (a mismatch
                           refuses before the claim); --prepare again redoes a
                           stale prepare. With --dry-run: the plan of both halves
        --forget           drop this clone's release journal, withdraw its open
                           asks, and exit; nothing public changes. Takes no flag
                           of a run
        --abandon vX.Y.Z   withdraw this clone's claimed, unpublished cut of
                           vX.Y.Z (its draft or uploads, its origin tag, its
                           journal; the claim commit stays). Only the claim's
                           own cutter can abandon its cut

  targo --unverified ship build
      The one build of a release bundle: both slices at once from a snapshot of
      the commit's git objects (the x86_64 slice run under Rosetta), laid out
      unsigned in the release cache — or found there, its bytes re-proved. The
      slices build one after the other when their last recorded memory peaks
      would not fit in three quarters of this Mac's memory. `ship release` runs
      it for the build its journal reserved. With no release in progress it
      builds HEAD under a provisional number (max(HEAD's ledger tail + 1, now),
      claimed nowhere) and prints the .app for `verify measure --app`.

  targo --unverified ship linux-build --version X.Y.Z --build-number N
                 --commit FULL-CLAIM-SHA --out EXISTING-PRIVATE-DIRECTORY
      On native x86_64 or aarch64 Linux, in a clean checkout at the claim commit:
      build aterm (and, when it can, its static bootstrap verifier) with the
      sealed Trust toolchain and write each with its provenance receipt into
      --out. Nothing is signed or published. Run it on both architectures and
      hand the directory to the cutter (--linux-artifacts), or let the cutter
      drive the hosts (--linux-worker).

  targo --unverified ship pin-verifier dist/aterm-bootstrap-verify-pin.toml
      Once a release is public: pin the static bootstrap verifiers its cut froze
      (the record its sign step wrote into dist/) in this checkout, as
      tools/install.sh's pin and crates/aterm-bootstrap-verify/pinned.toml, to
      commit together. Refuses a release this checkout's RELEASES.ledger does
      not claim, or one whose verifiers speak another interface than this tree's
      install.sh; then nothing is written.

  targo --unverified ship cut [--dry-run] [--resume] [--abandon vX.Y.Z]
                 [--min-build N] [--release-credentials <profile.toml>]
                 [--linux-artifacts DIRECTORY [--linux-target aarch64|x86_64]...
                  [--linux-worker ARCH=HOST:REPO]... | --mac-only]
      The cut step `ship release` runs; release with `ship release`. Gates, the
      claim of the release's reserved build, a byte copy of the verified bundle
      signed, notarized and packaged (the cut never builds), the tag, and one
      publication onto the release channel, its head made last. Only the release
      commit's own cutter cuts it. A real cut must name --linux-artifacts or
      --mac-only.
        --dry-run          the gates, then the verified bundle copied into dist/,
                           signed, notarized and self-checked under its reserved
                           number; nothing claimed, committed, published or built
        --resume           re-enter the journaled cut (dist/cut-state.toml) at
                           its first incomplete step
        --abandon vX.Y.Z   withdraw an unpublished cut: its own draft deleted, or
                           exactly its uploads taken back off the engine's source
                           release; its origin tag and the local journal deleted.
                           The claim commit stays; a later cut recuts
        --min-build N      raise the manifest's apply floor to at least N
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
                           and repairs rustup's trust link, audits the whole
                           publishing stack (the toolchains and Apple SDK, the
                           base and develop layers, LibreSSL, the publication
                           engine, the site checkout, node and firebase-tools,
                           the Apple identity, a live-tested notary credential,
                           the credentials profile, the channel token) and prints
                           every gap with its remedy. Only a clean pass mints
                           this machine's key, by the join ceremony: the paper
                           phrase, typed once, is the only input, and a roster id
                           is never spent on a machine that cannot release. A
                           provisioned machine is audited, never re-minted. Ends
                           READY TO CUT (ROSTERED on a host that cannot cut,
                           such as a non-Mac)
        --cert-dir <folder>
                           the one visible folder the Apple certificate errand
                           may use: the request is copied there and the issued
                           .cer looked for there and in ~/.aterm/apple. Without
                           it nothing outside ~/.aterm/apple is read, so no folder
                           macOS guards (~/Downloads) raises a dialog
        --check            audit only: nothing provisioned, minted or written;
                           exits non-zero while anything is open

  targo --unverified ship status        what a cut signs with · version · next cut ·
                           ledger tail · journaled cut · newest published build ·
                           dangling claims
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
                           over the export when --export judged one, else over
                           REV's own tree
      Exits 0 READY, 1 a gate would refuse, 2 no checkout or REV names no
      release, 3 a gate could not run (each line names its fix), 130 interrupted
  targo --unverified ship trust run (-p <crate>... | --whole-tree | --remeasure)
        [--jobs N|auto] [--target-root DIR] [--crate-budget SECONDS] [--prover BIN-DIR]
        [--update-ratchet] [--try-refused] [--obligations] [--stop-if-narrowed]
                           the Trust lane: `targo trust check -p <lib> --lib
                           --allow-l0-gaps` over the named libraries (-p <lib>
                           also selects its <lib>+<feature> rows, one for each
                           non-default feature that turns on a vendored fork),
                           every workspace library and vendored fork
                           (--whole-tree), or what this tree's stored facts
                           left undecided (--remeasure). Each check runs in its
                           own slot (--jobs; default sized from cores and
                           memory) under --target-root (default
                           <repo>/target-trust.noindex, else a per-user cache
                           directory), is ended OVER-BUDGET at --crate-budget
                           (default 21600 s), and is held to
                           crates/aterm-release/trust/floors.tsv. --prover names
                           another toolchain's bin/ (an absolute path). A
                           whole-tree run on a clean checkout files the facts
                           `trust judge` reads. --update-ratchet raises the floors
                           a run meets and keeps trust/memory.tsv current;
                           --try-refused verifies what this host would refuse for
                           memory; --stop-if-narrowed stops before any check when
                           memory would narrow the run; --obligations keeps each
                           check's per-obligation record. Every run also prints
                           the D27 gate's proved (calculus) counter from each
                           check's trust.obligations.v1 record (0, and
                           `ungraded`, from a prover that states no grade), held
                           to the current epoch's floor in
                           crates/aterm-release/trust/credit.tsv. Exit 0 pass, 1 fail,
                           2 undecided or could not run, 4 narrowed by memory
  targo --unverified ship trust judge [--commit REV] [--prover BIN-DIR] [--update-ratchet [--reason WHY]]
                           the lane's full report for the facts a whole-tree run
                           filed for REV (default HEAD) with the prover (--prover
                           names its bin/; default the cutter's Trust toolchain),
                           judged against the working tree's floors.
                           --update-ratchet writes what the facts meet (a first
                           floor, a raise, a corrected total, a re-base onto this
                           prover), never a lowering; with --reason WHY it lowers
                           onto this prover a floor another prover recorded that
                           the facts fall below, WHY in its fifth column. A
                           working floor the up-only rule refuses is named
                           LOWERED, and fails. Exit 0 pass, 1 fail or no facts,
                           2 undecided or could not judge, 4 narrowed by memory
  targo --unverified ship trust credit <record.ndjson>...
                           the proved (calculus) counter over those  
                           trust.obligations.v1 records (<target-dir>/trust/
                           obligations/<package>.ndjson), against the current
                           credit epoch. Exit 0 holds, 1 below the epoch's floor
                           or a record refused, 2 no readable credit epoch
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
  targo --unverified ship tap deploy <tap checkout> [--dry-run]
                           mirror distribution/homebrew-tap into a checkout of
                           alabsystems/homebrew-tap and commit it there; the push
                           is the owner's (`ship tap --help`)
  targo --unverified ship yank <build> [--release-credentials <profile.toml>]
                 (--linux-artifacts DIRECTORY [--linux-target aarch64|x86_64]...
                  [--linux-worker ARCH=HOST:REPO]... | --mac-only)
                           publish and verify a successor first (a real cut with
                           min_build = <build>+1, signed with
                           --release-credentials, declaring Linux as any cut
                           must), and only then delete the bad build's origin tag
                           and demote its channel release to a prerelease
";

/// A parsed invocation.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Help,
    LinuxBuild {
        version: String,
        build: u64,
        commit: String,
        out: std::path::PathBuf,
    },
    /// `pin-verifier <record>`: install.sh's verifier pin from a cut's receipts
    /// ([`crate::buildplan::linux::pin_verifier`]).
    PinVerifier(std::path::PathBuf),
    /// `build`: the release bundle, or HEAD's provisional bundle ([`crate::artifact::run_build`]).
    Build,
    Cut {
        opts: publish::CutOptions,
        abandon: Option<String>,
    },
    Provision {
        id: String,
        check: bool,
        /// `--cert-dir`: the visible folder the browser errand may use.
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
        /// `--no-draft-was-posted`: no create POST landed before the journal was lost.
        no_draft_posted: bool,
    },
    Verify {
        version: Option<String>,
    },
    /// `dev-app`: this checkout as `aterm (dev).app` ([`crate::dev_app`]).
    DevApp(crate::dev_app::Cmd),
    /// `trust run|judge`: the Trust lane and the judge of its stored facts ([`crate::trust_facts`]).
    Trust(crate::trust_facts::TrustCmd),
    /// `site`: alab.systems brought to a release ([`crate::site`]), which parses its flags.
    Site(Vec<String>),
    /// `hook <name>`: the publication engine's hooks ([`crate::hook`]).
    Hook(Vec<String>),
    /// `tap deploy …` — the Homebrew tap's structure, mirrored ([`crate::tap`]).
    Tap(Vec<String>),
    Yank {
        build: u64,
        /// Signing inputs for publishing the successor before deleting anything.
        opts: verify::YankOptions,
    },
}

/// Entry point for the binary, delegated from `main()`.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm-release: {e}\nusage: {} help", publish::SHIP_COMMAND);
            return 2;
        }
    };
    if let Cmd::Trust(t) = cmd {
        let root = repo_root()
            .ok()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let code = crate::trust_facts::run_cmd(&root, t);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        return code;
    }
    if let Cmd::Check(opts) = &cmd {
        return match repo_root() {
            Ok(root) => crate::release_steps::check(&root, opts),
            Err(e) => {
                eprintln!("aterm-release check: {e}");
                2
            }
        };
    }
    if let Some(code) = own_code(&cmd) {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        return code;
    }
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
    let _ = std::io::Write::flush(&mut std::io::stdout());
    match outcome {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", failure_line(&e.to_string()));
            1
        }
    }
}

const OWN_VERDICT: [&str; 5] = [
    "NOT DONE",
    "CHECK ONLY",
    "COULD NOT RUN",
    "NARROWED",
    "FAILED",
];

/// Format the final line printed by a failed verb.
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
        "pin-verifier" => match (it.next(), it.next()) {
            (Some(record), None) if !record.starts_with('-') => {
                Ok(Cmd::PinVerifier(std::path::PathBuf::from(record)))
            }
            _ => Err(format!(
                "pin-verifier takes the one record a cut wrote (dist/{})",
                crate::buildplan::linux::PIN_RECORD
            )),
        },
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

struct LinuxFlags<'o> {
    artifacts: &'o mut Option<std::path::PathBuf>,
    targets: &'o mut Vec<String>,
    workers: &'o mut Vec<String>,
    mac_only: &'o mut bool,
}

impl LinuxFlags<'_> {
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

    const fn declared(&self) -> bool {
        *self.mac_only || self.artifacts.is_some()
    }
}

const fn yank_linux(opts: &mut verify::YankOptions) -> LinuxFlags<'_> {
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
        Cmd::PinVerifier(record) => {
            let root = repo_root()?;
            let pin = crate::buildplan::linux::pin_verifier(&root, &record).map_err(Error::new)?;
            println!(
                "pinned {}'s bootstrap verifier from {} into tools/install.sh and {}; commit \
                 both:\n{}",
                pin.tag,
                pin.repo,
                aterm_bootstrap_verify::pin::RECORD,
                pin.lines()
            );
            Ok(())
        }
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

fn repo_root() -> ledger::Result<std::path::PathBuf> {
    let out = Command::new(aterm_uds::os_path!("git"))
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
