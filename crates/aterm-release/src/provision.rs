// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo ship provision --id <machine-id>` — a checkout becomes a PUBLISHING machine,
//! with the paper phrase as the only human input.
//!
//! "Publishing" is the load-bearing word: a machine on the roster builds what it signs,
//! so the audit proves the whole stack — the self-hosted Trust stage2 toolchain (a real
//! smoke-compile under the native-lane rustflags, via the same `gates` probes the cut
//! runs), the trust-named gate drivers (`targo`/`tippy`/`ty`/`trustdoc`), the doc-tool
//! farm link (`~/.local/bin/trustdoc`, repaired in place when safely mechanical —
//! [`doc_tool_check`]), the rustup front door (`cargo` in this repo dispatches into the
//! linked `trust` toolchain — the link is a provisioned artifact, not an accident), the
//! stable `x86_64-apple-darwin` slice of the universal binary, Apple's packaging tools,
//! the Developer ID identity, a
//! LIVE-tested notarytool credential, the credentials profile, `gh` auth and the
//! channel token. On a machine with none of that, the front door is
//! `tools/bootstrap-publisher.sh`, which acquires the toolchain and hands off here.
//!
//! The order is the safety argument:
//!
//!   1. **Seed the roster pair.** `dist/` is gitignored, so a fresh clone has no
//!      `aterm-machines.toml` — the pair ships as assets on every channel release, and
//!      the channel is anonymously readable (a proven cut invariant), so an
//!      unauthenticated fetch seeds it before any token exists. Every candidate is
//!      verified under `pins::PAPER_MASTER_PUBKEYS` BEFORE it is compared; the newest
//!      generation wins and equal generations must be byte-identical (two different
//!      master-valid rosters at one sequence is a lineage fork — a hard stop, never a
//!      preference). An unverifiable or half local pair is a hard stop too: this verb
//!      never overwrites roster state it cannot prove, because the torn copy might be
//!      the front half of an incumbent's UNPUBLISHED newer generation. When the seed
//!      does come from the channel, that residual is said out loud: an incumbent
//!      machine may hold an unpublished edit the channel cannot show us.
//!   2. **Audit everything, in one pass.** Collect-all reporting: every gap is printed
//!      with its exact remedy, not just the first.
//!   3. **Mint LAST, and only on a clean pass.** A roster id is irreversible (an id
//!      leaves the roster only by revocation), so it is never consumed on a machine
//!      the audit just proved cannot build or publish. The mint is the `atpkg-keys`
//!      join ceremony run as a LIBRARY — `preflight → verify_master → plan →
//!      write_pins → write_rest` — one `/dev/tty` phrase prompt, every leak rule
//!      intact, no second binary.
//!
//! Idempotent: a provisioned machine is audited (its private key actually read and
//! its derived public key bound through the real `machines::authorize_cut` gate — the
//! same code a cut runs), never re-minted. `--check` is the explicit no-writes mode, and
//! that is a whole-verb property, not a flag on the mint: nothing is minted, installed,
//! imported, tightened or written, and the Apple and notary steps report what they can
//! observe rather than acquiring anything. Reading is not writing, so it still audits.

#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
use aterm_update_core::pins;
#[cfg(unix)]
use aterm_update_core::roster::{self, Roster};

#[cfg(unix)]
use crate::ledger::{Error, Result};
#[cfg(unix)]
use crate::publish::step;
#[cfg(unix)]
use crate::{gates, machines, mirror, publish, sign};

/// POSIX-only, exactly like the engine it drives: `atpkg-keys` compiles empty on
/// Windows (`#![cfg(unix)]`), because the master phrase is read from `/dev/tty`.
#[cfg(not(unix))]
pub fn run_provision(
    _repo: &std::path::Path,
    _id: &str,
    _check_only: bool,
) -> crate::ledger::Result<()> {
    Err(crate::ledger::Error::new(
        "provision is POSIX-only: the provisioning engine reads the master phrase from /dev/tty",
    ))
}

#[cfg(unix)]
pub fn run_provision(repo: &Path, id: &str, check_only: bool) -> Result<()> {
    // The same id rules the roster enforces, checked before anything network-shaped.
    atpkg_keys::provision::vet_machine_id(id).map_err(Error::new)?;

    let manifest = std::fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read {}/Cargo.toml: {e}", repo.display())))?;
    let slug = mirror::update_channel_slug(&manifest)?.ok_or_else(|| {
        Error::new(
            "no update channel is committed ([workspace.metadata.aterm] update_channel), so \
             there is no release to seed the roster from and no channel to provision for",
        )
    })?;
    let mode = if check_only {
        " --check (no writes)"
    } else {
        ""
    };
    println!("aterm-release · provision {id} (channel {slug}){mode}");
    // No table of contents above the phases. The five names it listed are the five phase
    // headers printed below it, one at a time, each already carrying `[n/5]` — so it was
    // the plan said twice, at the top, where the operator is scanning for the first real
    // line.
    phase(
        1,
        "roster",
        "the master-signed list of machines allowed to publish",
    );

    // ---- 1. the roster pair: newest verified generation into dist/ ----------------
    let home = std::env::var("HOME").map_err(|_| Error::new("HOME is not set"))?;
    // Set-but-empty (or relative) survives the var read and would silently derive
    // RELATIVE kept-roster/key/identity/farm paths — writes landing in the process cwd
    // instead of the home directory. The kept pair below is the first of those writes,
    // and it is the one whose loss has no remedy, so refuse before it.
    if !Path::new(&home).is_absolute() {
        return Err(Error::new(format!(
            "HOME is not an absolute path ({home:?}) — refusing to derive the kept roster \
             pair, machine key, identity, and farm-link paths from it"
        )));
    }
    let roster_path = repo.join("dist").join(roster::ROSTER_ASSET);
    let kept_path = kept_roster_path(&home);
    // ---- the roster phase's WRITER LOCK -------------------------------------------
    // Everything from here to the end of the block below reads and writes the same
    // `dist/aterm-machines.toml` + `.sig` pair the in-process mint re-signs later, so it
    // is serialized against exactly the same writers and published through the same redo
    // transaction (see `lock_roster_pair` and `write_pair`). Taking it here also
    // completes forward whatever a previous run's death left committed, BEFORE
    // `read_local_candidate` looks at the pair — a torn pair is otherwise this phase's
    // hard stop, and "the roster did not verify" is the sentence that sends an operator
    // to re-check a paper phrase that was never wrong.
    //
    // The block is a block so that the guard's lifetime is bounded by eye: `flock` is per
    // open file description, so holding it into `mint()` — whose `write_rest` takes the
    // same lock on the same path — would deadlock this process against itself.
    let (chosen, channel_unreadable) = {
        let roster_lock = if check_only {
            // `--check` promises no writes, and completing a redo transaction IS a write, so
            // this mode may not take the writer lock. It applies the reader's refusal
            // instead: a pending transaction is REPORTED, with the command that repairs it.
            // So it does not serialize against a concurrent writer, and does not pretend
            // to — an audit reads what is on disk at the moment it looks, and one that
            // blocked behind a mint would be a worse audit, not a safer one.
            atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(&roster_path)?)
                .map_err(Error::new)?;
            None
        } else {
            Some(lock_roster_pair(&roster_path)?)
        };
        // dist/ is gitignored and `git clean -xdf` sweeps it — but the generation a mint
        // writes is re-signed LOCALLY and is published only by a later cut, so between the
        // two it can exist nowhere else on earth. Losing it leaves a machine whose key no
        // roster names, and the only remedy this tool could offer was "restore from the
        // machine holding the newest generation", which names no machine on a one- or
        // two-machine fleet. So a proven copy is kept beside the key it authorizes (below),
        // and dist/ is restored from it here — re-verified under the paper master by
        // `read_local_candidate`, exactly like any other pair. A restore is a WRITE into
        // dist/, so `--check` declines it for the same reason it declines `install_pair`.
        if let Some(lock) = roster_lock.as_ref()
            && restore_kept_pair(lock, &kept_path, &roster_path)?
        {
            step(
                "roster",
                &format!(
                    "dist/ was empty — restored this machine's copy from {}",
                    kept_path.display()
                ),
            );
        }
        let local = read_local_candidate(&roster_path)?;
        let fetched = fetch_channel_candidate(&slug);
        let seeded_from_channel = local.is_none() && fetched.is_ok();
        // A MINT MUST SEE THE CHANNEL. "The fetch failed" is not "the channel has no
        // roster": a 429/403/timeout on the anonymous asset path says nothing about
        // what generation the fleet is on, and minting from a stale local pair while a
        // peer has already published the next generation forks the lineage — two
        // master-signed documents at one number, each de-authorizing the other's
        // successors (2026-08-19 round-3 audit). Only a clean 404 (no roster published
        // yet) lets a mint proceed from the local pair alone.
        let channel_unreadable = match &fetched {
            Err(e) if !e.starts_with(NO_CHANNEL_ROSTER) => Some(e.clone()),
            _ => None,
        };
        if let Some(why) = channel_unreadable.as_ref() {
            // Said HERE, at phase 1, so `--check` shows it and the real run refuses before
            // the acquiring phases (an Apple identity, a notary profile) are spent on a
            // mint that will be refused anyway.
            step(
                "roster",
                &format!(
                    "the public channel could not be read ({why}); a mint will be REFUSED until it \
                     answers — auditing the rest, minting nothing"
                ),
            );
        }
        let (chosen, install, how) = choose_candidate(&roster_path, &slug, local, fetched)?;
        if install && let Some(lock) = roster_lock.as_ref() {
            install_pair(lock, &roster_path, &chosen)?;
        }
        // `how` describes the DECISION, not the write, because under `--check` there is no
        // write and the line was claiming one ("seeded from the latest channel release") on a
        // run whose banner says "no writes".
        step(
            "roster",
            &if install && check_only {
                format!("{how} — not written (--check)")
            } else {
                how
            },
        );
        if seeded_from_channel {
            // The one thing a channel seed cannot see: an incumbent machine holding an
            // UNPUBLISHED roster edit. Joining the older public generation would mint a
            // same-sequence fork that de-authorizes the incumbent's — so the residual is
            // stated before the mint, where stopping is still free.
            step(
                "",
                "note: the channel cannot show an incumbent's UNPUBLISHED roster edit — if \
                 one exists, STOP and copy that machine's dist/ pair here instead",
            );
        }
        (chosen, channel_unreadable)
    };
    // The roster writer lock is released HERE, at the end of the phase that took it, and
    // deliberately before `mint()`.

    // ---- what this machine already is ---------------------------------------------
    // No phase header, and no line on success. Every failure here is a hard `Err` carrying
    // its own message, and the one green line this section printed — "already provisioned
    // as 'm2' — key read, pubkey matches machine.toml" — is a weaker restatement of the
    // `authority` line, which proves the same key through the cut's own gate. On an
    // unprovisioned machine, the case the verb exists for, the section was a numbered
    // header with nothing under it.
    //
    // `home` is bound (and proven absolute) in the roster phase above.
    let key_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_KEY_REL);
    let identity_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_PUB_REL);

    // On a provisioned machine, actually READ the key: `ReleaseCredentials::resolve`
    // enforces ownership + 0600 and derives the public key from the private bytes, so
    // a corrupt, world-readable, or unrelated key file fails HERE, not at a cut.
    let resolved = sign::ReleaseCredentials::resolve(None, repo)?;
    let mut attributed: Option<roster::Attribution> = None;
    let already = if key_path.exists() {
        let identity = machines::MachineIdentity::read(&identity_path)?.ok_or_else(|| {
            Error::new(format!(
                "{} exists but {} is missing — a half-provisioned machine. The pair is \
                 written together by the join ceremony; restore machine.toml from wherever \
                 this key came from, or move the key aside and re-run provision to mint a \
                 fresh identity under a NEW id",
                key_path.display(),
                identity_path.display(),
            ))
        })?;
        if identity.id != id {
            return Err(Error::new(format!(
                "this machine is already provisioned as '{}' — run `cargo ship provision \
                 --id {}` to audit it. A machine never re-mints under a second id: revoke \
                 the old id first if it must be replaced (`atpkg-keys machine-revoke`)",
                identity.id, identity.id,
            )));
        }
        let derived = resolved
            .as_ref()
            .map(sign::ReleaseCredentials::pubkey)
            .ok_or_else(|| {
                Error::new(format!(
                    "{} exists but did not resolve as this machine's signing identity",
                    key_path.display()
                ))
            })?;
        if derived != identity.pubkey {
            return Err(Error::new(format!(
                "the private key at {} derives a DIFFERENT public key than {} records — \
                 the pair is incoherent (a restored backup? a copied key?). Move both \
                 aside and provision a fresh id; never sign with a key whose identity \
                 file lies about it",
                key_path.display(),
                identity_path.display(),
            )));
        }
        // The verb's cheapest decisive proof, run BEFORE its two irreversible ones. A
        // machine that may never publish again — a revoked id, a roster that has lapsed —
        // used to walk the whole verb first: prompted to spend one of the team's five
        // permanent Developer ID slots, stored a notary credential, wrote a profile
        // carrying its signing key, and only then learned at `authority` that an id never
        // returns. Nothing is repeated by this: the freshly minted case still binds below,
        // and this attribution is the one printed there.
        //
        // Over the pair phase 1 chose, not a fresh read of dist/: they are the same
        // document — the chosen one was written there moments ago, or was already there —
        // but `--check` declines that write, so re-reading judged a pair this run did not
        // pick, and on a swept checkout it died inside the cut's own reader talking about
        // a `machine_roster` profile key the operator never touched.
        attributed = Some(authorize(chosen.bytes.clone(), &chosen.sig, derived)?);
        true
    } else {
        false
    };

    // ---- 2. the FULL audit, before any mint ---------------------------------------
    // Evaluated one at a time and printed as each finishes, never collected in an array
    // first: the Apple step ACQUIRES — it can ask before spending a permanent certificate
    // slot, and can wait for the operator's browser errand — and in an array literal every
    // check runs before any line is printed, so that question would arrive with nothing
    // above it to explain itself.
    //
    // Walked as numbered phases rather than a flat list, because this verb is a PROCESS
    // an operator is standing in the middle of, sometimes for minutes at a time: each
    // phase announces itself, with `[n/5]`, before the lines it owns.
    let mut checks: Vec<(&'static str, Check)> = Vec::new();
    let record = |label: &'static str, check: Check, into: &mut Vec<(&'static str, Check)>| {
        print_check(label, &check);
        into.push((label, check));
    };
    phase(
        2,
        "build stack",
        "the toolchain and SDK a cut compiles with",
    );
    // The two checks below run only behind a PROVEN stage2. Without one they had nothing
    // to look at and returned a Skip whose whole content was "see the toolchain line" —
    // two full lines carrying nothing, in the densest part of the output, directly under
    // the one line the operator has to act on. And a stage2 that resolves but cannot
    // compile the native lane has to be replaced either way, which is the same repair a
    // missing `tippy` would name. The bin dir comes back BESIDE the verdict — the shape
    // `apple_identity_check` uses — so it is not resolved a second and third time.
    let (stack, stage2_bin) = toolchain_check(repo);
    record("toolchain", stack, &mut checks);
    if let Some(bin) = &stage2_bin {
        record("verifiers", verifiers_check(bin), &mut checks);
        record("front door", front_door_check(bin), &mut checks);
    }
    // NOT gated on a proven stage2, unlike the two above: this one reads the farm link
    // and PATH, which exist (and can be wrong) whether or not a stage2 resolves — so it
    // always has something to look at, and a machine whose toolchain needs replacing
    // still gets told its doc driver is missing in the same pass.
    record("doc tool", doc_tool_check(&home, check_only), &mut checks);
    record("x86 slice", x86_slice_check(), &mut checks);
    record("apple sdk", apple_clt_check(), &mut checks);

    phase(
        3,
        "Apple certificate",
        "this machine's own Developer ID identity",
    );
    // The SHA-1 the profile pins comes back BESIDE the check, not out of it. It used to be
    // formatted into an English sentence and then scraped back out with a 40-hex regex over
    // the printed line — and on a machine with two certificates that regex pinned whichever
    // appeared first in the prose, while the sentence claimed the profile disambiguated.
    let (apple_check, apple_sha1) = apple_identity_check(id, !check_only);
    record(crate::apple::APPLE_LABEL, apple_check, &mut checks);

    phase(
        4,
        "notary",
        "the credential Apple's notarization service answers to",
    );
    record("notary", notary_acquire(!check_only), &mut checks);

    phase(5, "credentials", "the tokens and profile a cut is handed");
    record("github", gh_check(), &mut checks);
    record(
        "channel",
        channel_token_check(&slug, !check_only),
        &mut checks,
    );

    // The profile is deliberately NOT audited here, and not yet written. It is the one
    // item provision PRODUCES, and it is produced from the key the mint below writes —
    // so counting it at this point is a deadlock rather than a gate: the absent profile
    // defers the mint, and the deferred mint is why the profile is absent. A fresh
    // machine could never finish, which is the whole claim of the verb. It is written
    // and reported once, after the mint, where its verdict is already true.
    let blocking = blockers(&checks);
    let host_cannot_cut = checks.iter().any(|(_, c)| matches!(c, Check::Skip(_)));

    // RETIRED 2026-08-26: the `SEED` audit band (`dist/toolchain-seed` validation
    // and its READY-TO-CUT gate). aterm ships ONE lean self-provisioning
    // download, so a cut needs no staged seed and the audit has none to report.

    // ---- 3. mint LAST -------------------------------------------------------------
    // A roster id is irreversible, so it is never consumed on a machine the audit just
    // proved cannot build or publish.
    //
    // A deferred mint prints NOTHING. It used to say "DEFERRED — minting '<id>' would
    // consume an irreversible roster id; fix the N item(s) above and re-run. Nothing was
    // written", four lines above a terminal error saying: fix N, re-run, nothing written.
    // The mint's absence is exactly what that error means, and the error is the line the
    // shell shows and the exit code carries.
    let report = if already || check_only || blocking > 0 {
        None
    } else if let Some(why) = channel_unreadable.as_ref() {
        return Err(Error::new(format!(
            "refusing to mint a roster id while the public channel cannot be read ({why}): a \
             mint has to see the fleet's current roster generation, or two machines end up \
             minting the same one (a lineage fork). Retry when the channel answers"
        )));
    } else {
        // A band, not a `[6/6]` phase: the mint is skipped on `--check` and on an already
        // provisioned machine, so a sixth fraction would promise a phase that often never
        // arrives. `phase()`'s `[n/5]` carries a promise; a band carries a heading.
        band("MINT");
        // The word "irreversible" appears seven times in this file and every one of them
        // is a CODE COMMENT. The tool's most irreversible act was also its quietest —
        // quieter than the Apple slot, which at least has four siblings and a [y/N] gate.
        // Ctrl-C here really is free: `write_pins` is two statements after the prompt.
        step(
            "mint",
            &format!(
                "typing the phrase mints '{id}' an IRREVERSIBLE roster id: an id is never \
                 re-issued and never re-used, and a machine never re-mints under a second \
                 one. Ctrl-C now costs nothing — nothing has been written yet."
            ),
        );
        Some(mint(repo, id, &roster_path)?)
    };
    let minted = report.is_some();

    // Re-resolved after a mint, because `resolved` was read before the key existed. This
    // is the key a cut would sign with, so it is the one the profile below is held to.
    let derived_pubkey = if minted {
        sign::ReleaseCredentials::resolve(None, repo)?.map(|c| c.pubkey().to_string())
    } else {
        resolved.as_ref().map(|c| c.pubkey().to_string())
    };

    // The key exists by now — the mint just wrote it, or it always did — so the profile
    // can be written from it and reported ONCE. The write is silent: the check below names
    // the file and proves it loads, and a `wrote <path>` line above a `<path> loads …`
    // line is two lines for one fact. A write that FAILS is reported through that same
    // check, rather than as a first line whose consequence ("no credentials profile at
    // <path>") is then printed as a second.
    //
    // `--check` skips the WRITE and keeps the check: reading a profile is not writing one,
    // and the profile is the single item a cut is handed directly — an audit-only mode
    // that stayed silent about it would omit the one line the operator ran it for.
    if key_path.exists() {
        let write_failed = if apple_sha1.is_some() && !check_only {
            crate::apple::write_credentials_profile(id, &roster_path, apple_sha1.as_deref()).err()
        } else {
            None
        };
        let check = match write_failed {
            Some(e) => Check::Fail {
                what: e,
                fix: "check ownership of ~/.aterm, then re-run".into(),
            },
            None => profile_check(&home, id, apple_sha1.as_deref(), derived_pubkey.as_deref()),
        };
        // RECORDED, not printed here. `profile` and `authority` are results of phase 5,
        // and they used to print AFTER the mint's own closing report — which ends with a
        // `=== NEXT ===` list — so they read as further next steps. `roster kept at
        // ~/.aterm/roster/...` landing directly under "copy dist/aterm-machines.toml to
        // every publishing machine" reads as a third instruction naming a DIFFERENT
        // roster path, and an operator following NEXT literally copies the wrong file.
        // Nothing was flushing out of order; the order was the bug. They belong in the
        // DONE band, which is emitted once, below, at the end.
        checks.push(("profile", check));
    }
    let (fails, waiting) = tally(&checks);

    // ---- bind the DERIVED key through the real cut gate ---------------------------
    // `machines::authorize_cut` is the same admission a cut runs: master signature,
    // schema, freshness horizon, revocation, `not_after`, and the id↔key bind. Passing
    // it here is the honest meaning of "this machine may sign". A machine that arrived
    // already provisioned passed this gate before the Apple phase — over the pair phase 1
    // CHOSE, which `--check` may have declined to write — and carries the answer down to
    // here, so nothing is proved twice.
    //
    // Reaching this arm means the mint just ran, and the mint RE-SIGNS the roster into
    // dist/ with this machine added: the pair phase 1 chose is the previous generation and
    // does not name this key. So this one case reads what the join wrote.
    let authority = if key_path.exists() {
        let attribution = match attributed {
            Some(a) => a,
            None => {
                let pubkey = derived_pubkey.ok_or_else(|| {
                    Error::new("the join reported success but the minted key did not resolve")
                })?;
                let doc = machines::RosterDocument::read(&roster_path)?;
                authorize(doc.bytes, &doc.signature, &pubkey)?
            }
        };
        Some(format!(
            "authorize_cut passes: '{}' signs under roster seq {}",
            attribution.machine_id, attribution.roster_seq,
        ))
    } else {
        None
    };

    // Keep this machine's own copy of the generation that just authorized it — written
    // only now, so what is kept is a pair `authorize_cut` accepted, never a guess. Step 1
    // restores dist/ from it, which is what makes a swept dist/ self-healing rather than
    // the one state this verb has no remedy for. Not under `--check`: it is a write.
    let roster_kept = if key_path.exists() && !check_only {
        match keep_roster_pair(&roster_path, &kept_path) {
            Ok(true) => Some(format!(
                "kept at {} — dist/ is sweepable",
                kept_path.display()
            )),
            Ok(false) => None,
            Err(e) => Some(format!("could not keep a copy: {e}")),
        }
    } else {
        None
    };

    close(Closing {
        id,
        home: &home,
        check_only,
        report: report.as_ref(),
        profile: checks.iter().find(|(l, _)| *l == "profile").map(|(_, c)| c),
        open: checks
            .iter()
            .filter_map(|(l, c)| match c {
                Check::Fail { .. } => Some(format!("{l} (gap)")),
                Check::Todo { .. } => Some(format!("{l} (waiting)")),
                _ => None,
            })
            .collect(),
        authority,
        roster_kept,
        key_exists: key_path.exists(),
        fails,
        waiting,
        host_cannot_cut,
    })
}

/// Everything the closing structure needs, gathered rather than printed as it is learned.
///
/// The gathering IS the fix. These lines are results, and results have to arrive together:
/// printed as each became true, three of them landed underneath a `=== NEXT ===` heading
/// written by the mint, where they read as instructions.
#[cfg(unix)]
struct Closing<'a> {
    id: &'a str,
    home: &'a str,
    check_only: bool,
    /// The mint's own report, when this run minted. `None` on `--check` and on every
    /// re-run of an already-provisioned machine.
    report: Option<&'a atpkg_keys::provision::Report>,
    profile: Option<&'a Check>,
    authority: Option<String>,
    roster_kept: Option<String>,
    key_exists: bool,
    fails: usize,
    waiting: usize,
    /// The labels of every open item, with the word each is counted under. "once fixed"
    /// pointed at no item, no file and no act; `checks` has held the answer all along.
    open: Vec<String>,
    host_cannot_cut: bool,
}

/// The mint's DONE facts, re-rendered on THIS transcript's grid as `(label, value)`.
///
/// Pure, and separate from [`close`], so `tests/transcript_grid.rs` can construct a
/// `Report` with every conditional set and assert that every load-bearing clause survives.
/// That test is what licenses re-rendering locally instead of splicing `render_report`'s
/// own lines in on their narrower gutter: the fact-loss risk is handled by a test, not by
/// keeping a layout that reads as output from a different program.
#[cfg(unix)]
pub(crate) fn report_done(r: &atpkg_keys::provision::Report) -> Vec<(String, String)> {
    use atpkg_keys::provision::Verb;
    let mut out = Vec::new();
    if r.verb == Verb::Setup {
        out.push((
            "anchor".to_string(),
            format!(
                "pins::PAPER_MASTER_PUBKEYS = {}  fingerprint {}",
                r.master_pubkey, r.master_fingerprint
            ),
        ));
    } else {
        out.push((
            "anchor".to_string(),
            format!(
                "phrase verified against the committed master ({})",
                r.master_fingerprint
            ),
        ));
    }
    out.push((
        "key".to_string(),
        format!(
            "{}  0600, stays on this machine  (pub {})",
            r.paths.key, r.machine_pubkey
        ),
    ));
    let mut roster = format!(
        "{} + .sig  seq {}  ({})",
        r.paths.roster,
        r.roster_seq,
        r.roster_machines.join(", ")
    );
    // A sentinel expiry is not news. A real one is, so it still gets its clause.
    if r.roster_valid_until != atpkg_keys::roster_ops::VALID_UNTIL_FOREVER {
        roster.push_str(&format!("  valid until {}", r.roster_valid_until));
    }
    if r.roster_was_fresh {
        roster.push_str(
            " — the ONLY roster this master signs; a second at the same seq forks it and \
             de-authorizes machines silently",
        );
    }
    out.push(("roster".to_string(), roster));
    if let Some((head_id, head_key)) = &r.seeded_head {
        out.push((
            String::new(),
            format!(
                "'{head_id}' = the incumbent keyset head ({head_key}); rename only now, via \
                 --head-id — roster ids are revoke-only later"
            ),
        ));
    }
    out.push((
        "keyset".to_string(),
        format!(
            "pins::UPDATE_CHANNEL_PUBKEYS unchanged ({}) — the roster authorizes '{}'",
            r.channel_after.len(),
            r.id
        ),
    ));
    out
}

/// The mint's NEXT steps that must happen BEFORE a cut — a working-tree edit and its
/// commit. They exist only when there IS one: printing them on a join sends the operator
/// to an empty diff and a no-op commit.
#[cfg(unix)]
pub(crate) fn report_next_before_cut(r: &atpkg_keys::provision::Report) -> Vec<String> {
    use atpkg_keys::provision::Verb;
    let mut out = Vec::new();
    if r.pins_changed {
        out.push(format!("review: git diff -- {}", r.paths.pins));
    }
    if r.verb == Verb::Setup {
        out.push(
            "delete the tripwire tests that assert an empty anchor:\n  \
             crates/aterm-update-core/src/pins.rs::tests::\
             the_paper_master_is_unset_so_the_roster_tier_is_inert\n  \
             crates/atpkg-keys/tests/paper_master_to_client.rs::\
             the_shipped_master_anchor_is_still_empty"
                .to_string(),
        );
    }
    if r.pins_changed {
        out.push("commit — durable from here".to_string());
    }
    out
}

/// The mint's NEXT steps that follow the dry run: distribute the roster, and who may sign
/// a REAL cut.
///
/// The head-key requirement sits directly under the copy step so the two read as one
/// escalation. It used to be `render_report`'s step 1 while the last line on the screen —
/// `READY TO CUT — next: cargo ship cut --dry-run …` — named neither the head key nor the
/// flag. Both were true and they looked like a disagreement, and the last one is the one a
/// stressed operator copies.
#[cfg(unix)]
pub(crate) fn report_next_after_cut(r: &atpkg_keys::provision::Report) -> Vec<String> {
    let mut out = vec![format!(
        "copy {} + .sig to every other publishing machine — a cut from an older roster is \
         refused",
        r.paths.roster
    )];
    if r.machine_is_committed_head {
        out.push(format!(
            "a REAL cut may be signed here — '{}' holds the committed keyset head, the one \
             key pre-roster clients verify",
            r.id
        ));
    } else if let Some(head) = r.channel_after.first() {
        // V5: the caveat rides the step it guards, indented under it, rather than
        // floating in a paragraph five lines from the command it qualifies.
        let mut line = format!(
            "a REAL cut must be signed by the head key {head}:\n  \
             run it on that machine, or from '{}' with --strand-pre-roster-clients (asserts \
             no pre-roster client is left to strand)",
            r.id
        );
        if let Some((head_id, _)) = &r.seeded_head {
            line.push_str(&format!(
                "\n  that machine's roster id is '{head_id}', and its profile must set \
                 machine_id = \"{head_id}\" — a declared id that contradicts the roster \
                 refuses the cut"
            ));
        }
        out.push(line);
    }
    out
}

/// One band header: a heading and NOTHING else on the line.
///
/// Column 2, not column 0. Column 0 now belongs to the single closing verdict and to
/// nothing else in the whole run — that is the entire reason the verdict is findable by
/// an operator scrolling back through a hundred lines. Everything under a band is on the
/// same 13-column grid as everything above it: two grids that never interleave read as
/// two sections, two grids that alternate read as damage.
#[cfg(unix)]
fn band(name: &str) {
    println!();
    println!("  === {name} ===");
}

/// The closing structure, and the ONLY place this verb ends.
///
/// # Why it is unconditional
///
/// `=== DONE ===` / `=== NEXT ===` used to exist only on the once-per-machine mint run —
/// emitted from inside `atpkg-keys`, on its own 10-column gutter, in the middle of a run
/// that then kept printing for another 140 lines. The run an operator actually REPEATS —
/// a re-check before a release, or `--check` — ended with seven unheaded lines in which
/// `authority`, the single line proving this machine may sign, was indistinguishable from
/// the seventh chore in a list. The shape that answers "is this machine still good?" was
/// present only on the run that needs it least.
///
/// # Why the mint's report is re-rendered here rather than passed through
///
/// `render_report` returns its lines on `atpkg-keys`' own gutter, three columns narrower
/// than this transcript's. Splicing them in preserves that gutter, and four mint facts on
/// a different grid look like output from a different program — which is exactly what lets
/// an operator's eye skip them. Every field it reads is `pub`, so this re-renders from the
/// `Report` itself; `tests/transcript_grid.rs` pins every load-bearing clause so a field
/// added there cannot be silently dropped here.
#[cfg(unix)]
fn close(c: Closing<'_>) -> Result<()> {
    let done = match (c.check_only, c.report.map(|r| r.pins_changed)) {
        (true, _) => "DONE (--check: nothing written)".to_string(),
        (false, Some(true)) => "DONE (working tree only — a commit makes it durable)".to_string(),
        _ => "DONE".to_string(),
    };
    // Gather BEFORE printing the header. `band(&done)` was unconditional, so a run with
    // nothing to report — every payload here is gated on a mint having happened or on the
    // key existing — printed a bare `=== DONE ===` immediately above the verdict
    // `NOT DONE`. A heading that contradicts the line under it is worse than no heading.
    let mut done_lines: Vec<(String, String)> = Vec::new();
    if let Some(r) = c.report {
        done_lines.extend(report_done(r));
    }
    if let Some(kept) = &c.roster_kept {
        done_lines.push(("roster".to_string(), kept.clone()));
    }
    if let Some(a) = &c.authority {
        done_lines.push(("authority".to_string(), a.clone()));
    }
    if !done_lines.is_empty() || c.profile.is_some() {
        band(&done);
        for (label, msg) in &done_lines {
            step(label, msg);
        }
        if let Some(check) = c.profile {
            print_check("profile", check);
        }
    }

    // ---- NEXT -------------------------------------------------------------------
    // One list, because there used to be two answers to "how do I cut?" five lines apart:
    // the mint's own step 1 named the head key and `--strand-pre-roster-clients`, and the
    // last line on the screen said `READY TO CUT — next: cargo ship cut --dry-run …` with
    // neither. Both were true and they looked like a disagreement — and the LAST one is
    // the one a stressed operator copies, which makes dropping `--dry-run` from it the
    // obvious next move. So: the dry run is step 1, printed once; the head-key
    // requirement is step 3, adjacent, so the two read as one escalation; and the verdict
    // banner is the bare word.
    let mut next: Vec<String> = Vec::new();
    if let Some(r) = c.report {
        next.extend(report_next_before_cut(r));
    }
    let profile_path = Path::new(c.home).join(".aterm/release-credentials.toml");
    // `c.fails + c.waiting == 0` is the whole point: this list used to print
    // `cargo ship cut --dry-run …` on a run whose own verdict, four lines later, was
    // `NOT DONE`. An operator who copies the last runnable command on the screen — which
    // is what a stressed operator does — was handed the one command the machine was not
    // yet allowed to run.
    if c.key_exists && !c.host_cannot_cut && c.fails + c.waiting == 0 {
        // The command as printed RUNS. It used to spell the profile `<profile>` even
        // though the audit had printed its real path two lines up, so the one thing the
        // operator came here to copy had to be assembled by hand.
        next.push(format!(
            "cargo ship cut --dry-run --release-credentials {}",
            profile_path.display()
        ));
    }
    if let Some(r) = c.report {
        next.extend(report_next_after_cut(r));
    }
    if !next.is_empty() {
        band("NEXT");
        for (n, line) in next.iter().enumerate() {
            step(&format!("{}.", n + 1), line);
        }
    }

    // ---- the verdict, and the ONLY thing in this run at column 0 -----------------
    println!();
    if !c.key_exists {
        if c.check_only {
            let open = gaps(c.fails, c.waiting);
            println!(
                "CHECK ONLY — unminted; {}",
                if open.is_empty() {
                    format!("a real `cargo ship provision --id {}` would mint", c.id)
                } else {
                    format!("{open} before a real run mints")
                }
            );
            return Ok(());
        }
        // Mint deferred: there is no identity to bind, and the machine is not provisioned
        // — say so through the exit code too.
        return Err(Error::new(format!(
            "NOT DONE — {}: {}.\nNothing was written and no roster id was spent. Re-run \
             `cargo ship provision --id {}` once those are settled — it resumes exactly \
             there.",
            gaps(c.fails, c.waiting),
            c.open.join(", "),
            c.id
        )));
    }
    // `--check` answers BEFORE the generic failure arm, because that arm's remedy —
    // "re-run `cargo ship provision`" — is wrong here twice over: this run wrote nothing,
    // and re-running the audit changes nothing. An audit reports; it does not prescribe
    // its own repetition. It still exits non-zero when something is open, so a caller can
    // gate on it.
    if c.check_only {
        if c.fails + c.waiting > 0 {
            return Err(Error::new(format!(
                "CHECK ONLY — nothing was written. {}: {}.",
                gaps(c.fails, c.waiting),
                c.open.join(", ")
            )));
        }
        println!("CHECK ONLY — nothing was written; every item above is proven");
        return Ok(());
    }
    if c.fails + c.waiting > 0 {
        return Err(Error::new(format!(
            "NOT DONE — {}: {}.\nRe-run `cargo ship provision --id {}` once those are \
             settled — it resumes exactly there.",
            gaps(c.fails, c.waiting),
            c.open.join(", "),
            c.id
        )));
    }
    if c.host_cannot_cut {
        // Non-macOS: the roster half is proven, the Apple half cannot exist here.
        println!(
            "ROSTERED — but cuts run on macOS (Tier APPLE): this machine can sign the atpkg \
             index, and can never say READY TO CUT"
        );
    } else {
        println!("READY TO CUT");
    }
    Ok(())
}

/// The in-process join ceremony: the exact `preflight → verify_master → plan →
/// write_pins → write_rest` sequence the `atpkg-keys` CLI runs, so every rule that
/// tool enforces — the phrase from `/dev/tty` only, the mistype caught against the
/// committed anchor before any write, `create_new` on the key file — holds here
/// unchanged. RELEASE-KEYS.md's "signing is in-process" rule extends to minting: no
/// second binary, no key path on any command line.
#[cfg(unix)]
fn mint(repo: &Path, id: &str, roster_path: &Path) -> Result<atpkg_keys::provision::Report> {
    use atpkg_keys::{master, provision as prov};

    // Before anything sensitive exists in this process.
    master::forbid_core_dumps();

    let paths = prov::Paths {
        pins: utf8_path(&repo.join(prov::PINS_REL))?,
        roster: utf8_path(roster_path)?,
        key: prov::home_path(prov::MACHINE_KEY_REL).map_err(Error::new)?,
        machine_pub: prov::home_path(prov::MACHINE_PUB_REL).map_err(Error::new)?,
        // This is the checkout's own discovered anchor, not another tree's.
        pins_explicit: false,
    };
    let pre =
        prov::preflight(prov::Verb::Join, id, prov::DEFAULT_HEAD_ID, &paths).map_err(Error::new)?;
    let phrase = prompt_master_with_retries()?;
    let seed = phrase.seed();
    // The fingerprint is printed HERE and nowhere else. It used to be printed on the way
    // in, as `master fingerprint: <fp>  (compare with the paper)` — a manual comparison
    // the tool then performs itself two statements later (this verb only ever runs
    // `Verb::Join`, never `Setup`, so the operator's eye is never the only check), and a
    // value `render_report` reprints on its own `anchor  phrase verified against the
    // committed master (<fp>)` line. The one moment a human needs to read it is this one,
    // which is also where `verify_master`'s own text — "compare the fingerprint printed
    // above against the one on your paper" — was pointing at a line that had scrolled by.
    prov::verify_master(&pre, &seed).map_err(|e| {
        // Above the refusal, because the refusal's own words are "compare the fingerprint
        // printed above against the one on your paper".
        Error::new(match seed.fingerprint() {
            Ok(fp) => format!("the phrase you typed has fingerprint {fp}\n{e}"),
            Err(_) => e,
        })
    })?;
    let planned = prov::plan(pre, &seed, now_unix()?).map_err(Error::new)?;
    prov::write_pins(&planned).map_err(Error::new)?;
    // RETURNED, never printed. `render_report` is a TERMINAL-final report — its own doc
    // calls it "the closing output: DONE, then NEXT" — and this call sits in the middle
    // of a run that keeps printing results for another 140 lines. Printing it here put
    // `profile`, `authority` and `roster kept` underneath a heading that says NEXT, and
    // `roster kept at ~/.aterm/roster/aterm-machines.toml` directly under "copy
    // dist/aterm-machines.toml to every publishing machine" reads as a third instruction
    // naming a different roster path. An operator following NEXT literally copies the
    // wrong file. `close()` re-renders these facts on THIS transcript's grid, once,
    // at the end.
    prov::write_rest(planned).map_err(Error::new)
}

/// Ask for the paper master on the transcript's own grid, and give a mistype another go.
///
/// # The prompt
///
/// It used to start at column 0, breaking the two-space grid every other line of the run
/// uses, with no blank line above it. `prompt_master_attempt` writes the string verbatim,
/// so putting it through [`crate::publish::grid_block`] is a one-string change here.
///
/// "hyphens" is in the forgiveness list because `parse_master` strips them (it strips
/// whitespace and `b'-'`), and its own doc says the rule exists "because the owner
/// grouped the characters". The operator is reading a hyphenated paper master, typing
/// blind, against a list that presents itself as complete and omitted the one character
/// they are looking at.
///
/// # The retries
///
/// `prompt_for_master`'s own doc names its assumption: "a typo is a final error… The
/// right shape for `join` and `master-check`, where the command is cheap to re-run."
/// In `provision` the command is not cheap. By this point a PERMANENT Apple certificate
/// slot has been spent, an Account-Holder browser errand is done, and a notary credential
/// is stored. Fifty-two characters copied from paper with echo off — a mistype is the
/// expected case, not the exceptional one.
///
/// A retry is exactly equivalent to re-running the command: nothing is written until
/// `write_pins`, which is two statements after the caller's `verify_master`. Three
/// attempts, then an error that says so.
#[cfg(unix)]
fn prompt_master_with_retries() -> Result<atpkg_keys::master::MasterPhrase> {
    use atpkg_keys::master;
    const TRIES: usize = 3;
    let prompt = format!(
        "\n{}",
        crate::publish::grid_block(
            "master",
            "master phrase (52 characters, echo off; spaces, hyphens, case, and o/i/l are \
             forgiven): "
        )
    );
    // The prompt goes to /dev/tty, which is unbuffered, while everything above it went to
    // a block-buffered stdout under `| tee`. Without this the operator is asked for the
    // master phrase on an otherwise blank screen, with the MINT band explaining what they
    // are about to spend still sitting in a buffer.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    for left in (0..TRIES).rev() {
        // The outer Result is an I/O failure — no terminal, a read error — and retrying
        // that would just loop. Only the inner one is a typo.
        match master::prompt_master_attempt(&prompt).map_err(Error::new)? {
            Ok(phrase) => return Ok(phrase),
            Err(typo) if left > 0 => {
                step("master", &typo.message());
                step("", &format!("nothing written — try again ({left} left)"));
            }
            Err(typo) => {
                return Err(Error::new(format!(
                    "{} — {TRIES} attempts, and nothing was written: no key, no roster, no \
                     roster id spent. Re-run `cargo ship provision` and it resumes exactly \
                     here.",
                    typo.message()
                )));
            }
        }
    }
    unreachable!("the loop returns on every arm")
}

/// A roster pair that verified under the committed paper master and parsed.
/// Nothing here is secret — derived `Debug` is deliberate.
#[cfg(unix)]
#[derive(Debug)]
struct Candidate {
    bytes: Vec<u8>,
    sig: Vec<u8>,
    roster: Roster,
}

/// Verify-then-parse — the only admission path, same rule as every client: no
/// unverified roster bytes are ever compared, installed, or trusted for a seq.
#[cfg(unix)]
fn admit_candidate(
    master_pubkeys: &[&str],
    bytes: Vec<u8>,
    sig: Vec<u8>,
) -> std::result::Result<Candidate, String> {
    let verified = roster::verify_roster(master_pubkeys, bytes.clone(), &sig).map_err(|e| {
        format!("the roster did not verify under the committed paper master ({e:?})")
    })?;
    let parsed = Roster::parse(&verified)
        .map_err(|e| format!("the roster verified but did not parse ({e:?})"))?;
    Ok(Candidate {
        bytes,
        sig,
        roster: parsed,
    })
}

/// The pair already in `dist/`, admitted — or a HARD ERROR when any local roster
/// state exists that cannot be proven (a half pair, an orphan signature, a body that
/// fails verification). Local state is never silently overwritten: the torn copy
/// might be the front half of an incumbent's UNPUBLISHED newer generation, and
/// destroying it would set up a same-sequence lineage fork. The operator resolves it
/// by hand — re-copy BOTH files from the source machine, or remove both to accept
/// the channel's generation.
#[cfg(unix)]
fn read_local_candidate(roster_path: &Path) -> Result<Option<Candidate>> {
    let sig_path = machines::RosterDocument::signature_path(roster_path);
    let body = roster_path.exists();
    let sig = sig_path.exists();
    if !body && !sig {
        return Ok(None);
    }
    if body != sig {
        let (present, absent) = if body {
            (roster_path.display(), sig_path.display())
        } else {
            (sig_path.display(), roster_path.display())
        };
        return Err(Error::new(format!(
            "half a roster pair: {present} exists but {absent} is missing. The pair is \
             one authorization document — re-copy BOTH files from the machine that has \
             them, or remove the stray file to accept the channel release's pair",
        )));
    }
    let doc = machines::RosterDocument::read(roster_path)?;
    let c = admit_candidate(pins::PAPER_MASTER_PUBKEYS, doc.bytes, doc.signature).map_err(|e| {
        Error::new(format!(
            "the dist/ roster pair is unusable: {e}. Refusing to overwrite local \
                 roster state this tool cannot prove — if it was a hand-copy, re-copy \
                 BOTH files from the source machine; remove both files to accept the \
                 channel release's pair instead",
        ))
    })?;
    Ok(Some(c))
}

/// The latest channel release's pair, fetched anonymously and admitted.
#[cfg(unix)]
fn fetch_channel_candidate(slug: &str) -> std::result::Result<Candidate, String> {
    let bytes =
        curl_fetch(&release_asset_url(slug, roster::ROSTER_ASSET), 65_536).map_err(|e| {
            // Only the BODY's clean 404 means "no roster published yet"; everything else
            // (a missing signature beside a present body, 429/403, a timeout, a
            // wrongly-slugged or private repo answering 404 for the sig only) is "cannot
            // tell" — the mint gate treats those differently.
            if e.contains("returned error: 404") {
                format!("{NO_CHANNEL_ROSTER}: {e}")
            } else {
                e
            }
        })?;
    let sig = curl_fetch(&release_asset_url(slug, roster::ROSTER_SIG_ASSET), 4_096)
        .map_err(|e| format!("roster present but its signature could not be fetched: {e}"))?;
    admit_candidate(pins::PAPER_MASTER_PUBKEYS, bytes, sig)
}

/// Marker prefix `fetch_channel_candidate` puts on the one failure that is NOT
/// "cannot tell": the roster body answered a clean 404 — no roster published yet.
#[cfg(unix)]
const NO_CHANNEL_ROSTER: &str = "no roster on the channel";

/// `https://github.com/<slug>/releases/latest/download/<asset>` — the anonymous asset
/// path, deliberately not `gh`: a machine being provisioned has no tokens yet, and the
/// roster's authority is its master signature, never its transport.
#[cfg(unix)]
fn release_asset_url(slug: &str, asset: &str) -> String {
    format!("https://github.com/{slug}/releases/latest/download/{asset}")
}

/// One bounded anonymous download. The caps mirror the update client's own roster
/// limits (64 KiB body, 4 KiB signature) — a seed must never accept more than the
/// fleet would.
#[cfg(unix)]
fn curl_fetch(url: &str, cap: usize) -> std::result::Result<Vec<u8>, String> {
    let cap_s = cap.to_string();
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "60",
            "--max-filesize",
            &cap_s,
            url,
        ])
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("curl {url}: {}", err.trim()));
    }
    // --max-filesize can miss chunked transfers; the cap is the contract either way.
    if out.stdout.len() > cap {
        return Err(format!("{url}: exceeded the {cap}-byte cap"));
    }
    if out.stdout.is_empty() {
        return Err(format!("{url}: zero bytes"));
    }
    Ok(out.stdout)
}

/// Newest-generation-wins between what `dist/` holds and what the channel serves —
/// pure, so the rule is testable without a network. Equal generations must be
/// byte-identical: two DIFFERENT master-valid rosters at one sequence is a lineage
/// fork (each de-authorizes the other's successors), which no preference rule may
/// paper over. Returns the chosen candidate, whether it must be written into
/// `dist/`, and the transcript line saying why.
#[cfg(unix)]
fn choose_candidate(
    roster_path: &Path,
    slug: &str,
    local: Option<Candidate>,
    fetched: std::result::Result<Candidate, String>,
) -> Result<(Candidate, bool, String)> {
    match (local, fetched) {
        (None, Err(e)) => Err(Error::new(format!(
            "no roster pair anywhere: {} is absent and the channel fetch failed ({e}). \
             Copy aterm-machines.toml AND aterm-machines.toml.sig into dist/ from a \
             provisioned machine or from a release's assets ({} and …sig), then re-run",
            roster_path.display(),
            release_asset_url(slug, roster::ROSTER_ASSET),
        ))),
        (None, Ok(f)) => {
            let how = format!(
                "the latest channel release's pair is the only one here (roster_seq {})",
                f.roster.roster_seq
            );
            Ok((f, true, how))
        }
        (Some(l), Err(e)) => {
            let how = format!(
                "using the dist/ pair (roster_seq {}); channel fetch failed ({e})",
                l.roster.roster_seq
            );
            Ok((l, false, how))
        }
        (Some(l), Ok(f)) => {
            let (ls, fs) = (l.roster.roster_seq, f.roster.roster_seq);
            if fs > ls {
                Ok((
                    f,
                    true,
                    format!("the channel release is newer than dist/: roster_seq {ls} → {fs}"),
                ))
            } else if fs < ls {
                Ok((
                    l,
                    false,
                    format!(
                        "the dist/ pair (roster_seq {ls}) is AHEAD of the channel ({fs}) — an \
                         unpublished roster edit; keeping the newer generation"
                    ),
                ))
            } else if l.bytes == f.bytes {
                Ok((
                    l,
                    false,
                    format!("the dist/ pair is byte-identical to the channel's (roster_seq {ls})"),
                ))
            } else {
                // Same sequence, different bytes: a fork already exists. Nothing this
                // tool picks would be safe — the master's holder must decide which
                // lineage is real and republish it.
                Err(Error::new(format!(
                    "LINEAGE FORK: the dist/ pair and the channel's both carry roster_seq \
                     {ls} but their bytes differ — two master-signed rosters at one \
                     generation de-authorize each other's successors. Do not mint. \
                     Determine which document is authoritative (compare machine lists \
                     with the master's holder), put THAT pair in dist/ everywhere, and \
                     republish it before any further roster edit",
                )))
            }
        }
    }
}

/// Take the roster pair's WRITER lock — the same `flock` rendezvous, on the same path,
/// that every `atpkg-keys` roster ceremony takes — and complete forward any redo
/// transaction a previous run's death left committed.
///
/// This verb writes the roster pair BEFORE it mints (phase 1 seeds `dist/` from the kept
/// copy or the channel; the mint re-signs the same two files 400 lines later), so
/// "the transaction layer protects the mint" was never enough: a death between phase 1's
/// two renames left the new document beside the old signature with NO committed
/// transaction, which is the one state nothing downstream can repair and every operator
/// misreads as a mistyped phrase. Routing these writes through the same lock closes it,
/// and taking the lock at the top of the phase means what this run READS is already a
/// pair some earlier death cannot have torn.
///
/// The guard is scoped to its phase and never held into [`mint`]: `flock` is per open
/// file description, so this process taking the same lock a second time — which
/// `atpkg_keys::provision::write_rest` does — would block forever on itself.
#[cfg(unix)]
fn lock_roster_pair(roster_path: &Path) -> Result<atpkg_keys::provision::RosterLock> {
    atpkg_keys::provision::lock_roster(&utf8_path(roster_path)?).map_err(Error::new)
}

/// The pair a write is about to REPLACE, read under the lock that guards it: the redo
/// transaction's recorded predecessor and its compare-and-swap premise in one.
///
/// `None` is both halves absent — a first write. One half present is refused rather than
/// overwritten: it is the torn state this layer exists to eliminate, so if the layer did
/// not put it there, something outside it did, and the front half may be the only copy
/// of an incumbent's unpublished generation ([`read_local_candidate`] refuses it for the
/// same reason, in the same phase).
#[cfg(unix)]
fn pair_premise(roster_path: &Path) -> Result<Option<atpkg_keys::provision::RosterSnapshot>> {
    let sig_path = machines::RosterDocument::signature_path(roster_path);
    match (std::fs::read(roster_path), std::fs::read(&sig_path)) {
        (Ok(raw), Ok(sig)) => Ok(Some(atpkg_keys::provision::RosterSnapshot { raw, sig })),
        (Err(body), Err(signature))
            if body.kind() == std::io::ErrorKind::NotFound
                && signature.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        _ => Err(Error::new(format!(
            "the roster pair at {} is missing one half or unreadable; restore BOTH \
             files (or remove both) before this pair is replaced",
            roster_path.display()
        ))),
    }
}

/// Write the chosen pair into `dist/` under the caller's roster lock.
#[cfg(unix)]
fn install_pair(
    lock: &atpkg_keys::provision::RosterLock,
    roster_path: &Path,
    c: &Candidate,
) -> Result<()> {
    write_pair(lock, roster_path, &c.bytes, &c.sig)
}

/// The write itself, over bytes that some caller has already proven — `install_pair` for
/// an admitted candidate, [`keep_roster_pair`] for a pair `authorize_cut` just accepted.
///
/// It publishes through `atpkg-keys`' redo transaction: the complete new pair and the
/// exact predecessor it replaces are fsynced into a staging directory and published by
/// ONE rename, and only then do the two canonical names move. Before that rename a crash
/// loses litter; after it, the next run to take this lock installs the exact signed pair
/// — which is the guarantee staging-then-two-renames could state for each FILE and never
/// for the PAIR.
#[cfg(unix)]
fn write_pair(
    lock: &atpkg_keys::provision::RosterLock,
    roster_path: &Path,
    bytes: &[u8],
    sig: &[u8],
) -> Result<()> {
    if let Some(dir) = roster_path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::new(format!("create {}: {e}", dir.display())))?;
    }
    let path = utf8_path(roster_path)?;
    let expected = pair_premise(roster_path)?;
    atpkg_keys::provision::publish_roster_locked(lock, &path, expected.as_ref(), bytes, sig)
        .map_err(Error::new)
}

/// Publish a roster pair that the CALLER has already proven, taking the pair's writer
/// lock for the duration of this one write.
///
/// The locked [`write_pair`] is the form a phase uses when it reads, decides and writes
/// under ONE lock. This is for the writer that has no such span — `cargo ship recover`
/// rewriting `dist/` from an already-published release — so that it, too, publishes
/// through the redo transaction instead of two bare `write`s that a death can tear.
/// Never call it while another roster lock is held in this process: `flock` is per open
/// file description and would block on itself.
#[cfg(unix)]
pub(crate) fn publish_proven_pair(roster_path: &Path, bytes: &[u8], sig: &[u8]) -> Result<()> {
    let lock = lock_roster_pair(roster_path)?;
    write_pair(&lock, roster_path, bytes, sig)
}

/// The roster transaction layer is `atpkg-keys`, which is POSIX-only (the master phrase
/// comes from `/dev/tty`), so a non-POSIX build gets the per-file discipline and an
/// honest statement that the PAIR window is open there. Nothing in this workspace cuts,
/// provisions or recovers a release off POSIX.
#[cfg(not(unix))]
pub(crate) fn publish_proven_pair(
    roster_path: &std::path::Path,
    bytes: &[u8],
    sig: &[u8],
) -> crate::ledger::Result<()> {
    let mut sig_path = roster_path.as_os_str().to_owned();
    sig_path.push(".sig");
    std::fs::write(roster_path, bytes)
        .and_then(|()| std::fs::write(std::path::PathBuf::from(sig_path), sig))
        .map_err(|e| crate::ledger::Error::new(format!("write {}: {e}", roster_path.display())))
}

/// This machine's durable copy of the roster generation its key is named in — beside the
/// key itself, in the directory that already holds everything this machine minted for
/// itself, and outside any checkout a `git clean` can sweep.
#[cfg(unix)]
fn kept_roster_path(home: &str) -> PathBuf {
    Path::new(home)
        .join(".aterm")
        .join("roster")
        .join(roster::ROSTER_ASSET)
}

/// Copy the pair `authorize_cut` just accepted into `~/.aterm/roster`. `Ok(true)` when a
/// copy was written, `Ok(false)` when the kept pair is already byte-identical — so a
/// provisioned machine says this once, not on every audit.
#[cfg(unix)]
fn keep_roster_pair(roster_path: &Path, kept_path: &Path) -> Result<bool> {
    let doc = machines::RosterDocument::read(roster_path)?;
    // The KEPT pair's own lock, taken before this function reads it, so the compare that
    // decides "already identical" and the write that follows cannot straddle another
    // process's copy. Only one roster lock is ever held at a time in this verb: `dist/`'s
    // was released at the end of phase 1, and the pair being copied FROM was proved by
    // `authorize_cut` in this process moments ago.
    //
    // A TORN kept copy is now refused by `write_pair`'s premise rather than overwritten,
    // and that is the right way round: the no-downgrade rule below needs BOTH halves to
    // read a generation, so a copy missing one cannot be compared — and overwriting it
    // blind is exactly how the only witness to a newer generation would be destroyed.
    // The refusal is a line on the transcript, not a failed run.
    let kept_lock = lock_roster_pair(kept_path)?;
    if let Ok(kept) = machines::RosterDocument::read(kept_path) {
        if kept.bytes == doc.bytes && kept.signature == doc.signature {
            return Ok(false);
        }
        // Never DOWNGRADE the copy. If what is kept is a NEWER generation than the
        // checkout's — a second checkout, a hand-restored older pair — then the checkout
        // is the stale side and the copy is the only witness to the newer one, which is
        // precisely the loss this mechanism exists to prevent. Both sequences are read
        // through `admit_candidate`, so neither is ever taken from unverified bytes.
        let seq = |bytes: Vec<u8>, sig: Vec<u8>| {
            admit_candidate(pins::PAPER_MASTER_PUBKEYS, bytes, sig)
                .ok()
                .map(|c| c.roster.roster_seq)
        };
        let newer_kept = matches!(
            (
                seq(kept.bytes, kept.signature),
                seq(doc.bytes.clone(), doc.signature.clone()),
            ),
            (Some(k), Some(d)) if k > d
        );
        if newer_kept {
            return Ok(false);
        }
    }
    write_pair(&kept_lock, kept_path, &doc.bytes, &doc.signature)?;
    Ok(true)
}

/// Restore `dist/` from the kept copy, and only when dist/ holds NEITHER half.
///
/// Local roster state is never overwritten — a half or unverifiable dist/ pair stays
/// `read_local_candidate`'s hard stop, because the torn file might be the front half of an
/// incumbent's unpublished generation. In the other direction the kept copy is only a
/// cache of something the paper master signed, so one that does not verify is ignored
/// rather than fatal: the channel is still there, and step 1's rule is unchanged either
/// way. It is re-admitted under `PAPER_MASTER_PUBKEYS` before it is written, exactly like
/// a pair fetched from the channel.
#[cfg(unix)]
fn restore_kept_pair(
    lock: &atpkg_keys::provision::RosterLock,
    kept_path: &Path,
    roster_path: &Path,
) -> Result<bool> {
    if roster_path.exists() || machines::RosterDocument::signature_path(roster_path).exists() {
        return Ok(false);
    }
    let Ok(Some(kept)) = read_local_candidate(kept_path) else {
        return Ok(false);
    };
    install_pair(lock, roster_path, &kept)?;
    Ok(true)
}

/// One audit line: proven, missing-with-remedy, or impossible on this host. A `Skip`
/// is NOT a pass — a host that skips Apple checks can never say READY TO CUT.
#[cfg(unix)]
enum Check {
    Pass(String),
    Fail {
        what: String,
        fix: String,
    },
    /// Progress, waiting on the operator — the certificate request is at Apple. Distinct
    /// from `Fail` because "MISSING" reads as a fault to repair and this is a step to
    /// take, but it counts against READY TO CUT all the same, and it defers the mint for
    /// the same reason a Fail does: a roster id is irreversible.
    Todo {
        what: String,
        next: String,
    },
    Skip(String),
}

#[cfg(unix)]
/// One marker vocabulary, and it matches the word the SUMMARY counts in.
///
/// `gaps()` says "gap" and "waiting", so the lines say `GAP —` and `WAITING —`. "MISSING"
/// was false of five of this file's `Fail` messages — a token that exists but is
/// group-readable, a profile that declares the wrong `machine_id`, an identity that is
/// installed but cannot sign — and it disagreed with the summary that counts it.
///
/// An unmarked `Todo` was worse: it opened exactly like a `Pass`, so the one line holding
/// up the entire run read green until the eye reached its second line.
#[cfg(unix)]
fn print_check(label: &str, c: &Check) {
    match c {
        Check::Pass(msg) => step(label, msg),
        Check::Skip(msg) => step(label, &format!("impossible here — {msg}")),
        Check::Fail { what, fix } => {
            step(label, &format!("GAP — {what}"));
            step("", &format!("fix: {fix}"));
        }
        Check::Todo { what, next } => {
            step(label, &format!("WAITING — {what}"));
            step("", &format!("next: {next}"));
        }
    }
}

/// The Trust stage2 toolchain, proven by the same probe a cut runs: resolve it
/// (atpkg store → `$HOME/trust` → `TRUST_STAGE2_BIN`), run `trustc --version`, then
/// smoke-COMPILE a probe under the exact native-lane rustflags.
///
/// Returns the stage2 `bin` dir beside the verdict, because the two checks that follow
/// need exactly that and nothing else — and because without one there is nothing for them
/// to look at, so the caller does not run them at all.
#[cfg(unix)]
fn toolchain_check(repo: &Path) -> (Check, Option<PathBuf>) {
    match gates::trustc_probe(repo) {
        Ok(trustc) => {
            let bin = trustc.parent().map(Path::to_path_buf);
            (
                Check::Pass(format!(
                    "trust stage2 compiles the native lane ({})",
                    trustc.display()
                )),
                bin,
            )
        }
        // `trustc_probe` returns a FAULT (plus, for the missing-toolchain branch, the
        // one fact the operator cannot derive: where it looked). The remedies are laid
        // out HERE, on this transcript's grid, script first and the three manual routes
        // under `or:` — one act to try, three to fall back on, in one place.
        Err(e) => (
            Check::Fail {
                what: e.to_string(),
                fix: format!(
                    "tools/bootstrap-publisher.sh — it does the whole stack\n\
                     or, by hand:\n  {}",
                    gates::TRUST_TOOLCHAIN_REMEDIES.replace('\n', "\n  ")
                ),
            },
            None,
        ),
    }
}

/// The trust-named drivers the gate and the ship-build run beside `trustc`.
#[cfg(unix)]
fn verifiers_check(bin: &Path) -> Check {
    let missing: Vec<&str> = ["targo", "tippy", "ty", "trustdoc"]
        .iter()
        .copied()
        .filter(|t| !bin.join(t).is_file())
        .collect();
    if missing.is_empty() {
        Check::Pass(format!(
            "targo + tippy + ty + trustdoc present in {}",
            bin.display()
        ))
    } else {
        // Not `bootstrap-publisher.sh`: it resolves an existing stage2 and stops, so it
        // would report success over exactly this gap. A stage2 missing its tools has to
        // be replaced or rebuilt.
        Check::Fail {
            what: format!("{} missing from {}", missing.join(" + "), bin.display()),
            // The config key had no FILE and named a tool that is not in the missing
            // list, so neither half of this remedy could be acted on. Name the file and
            // the command.
            fix: "aterm pkg install trust   (then `aterm pkg doctor` to confirm the store)\n\
                  or, from source: set `tools` in $HOME/trust/bootstrap.toml to include every \
                  driver above, then `python3 x.py build --stage 2` in $HOME/trust"
                .into(),
        }
    }
}

#[cfg(unix)]
/// The remedy for "the driver is there, its directory is not on PATH", written once
/// because both checks in [`doc_tool_check`] hand out the identical one.
///
/// PERSISTENTLY. "add <dir> to PATH" gets done with an `export` that dies with the
/// shell, so the next run reports the identical gap and the operator concludes the tool
/// is wrong about the machine. Say the durable form, and say why it has to be durable.
fn path_persist_fix(dir: &Path) -> String {
    format!(
        "echo 'export PATH=\"{d}:$PATH\"' >> ~/.zshrc && exec zsh\n\
         it has to persist: a plain `cargo test`'s doctest lane resolves \
         `trustdoc` from PATH, in whatever shell it runs in",
        d = dir.display()
    )
}

/// The doc-tool FARM LINK. `.cargo/config.toml` names the workspace's doc driver
/// by BARE NAME (`[build] rustdoc = "trustdoc"`, resolved from PATH), so a plain
/// `cargo test` on this machine runs its doctest lane only where `trustdoc`
/// resolves — the verify gate binds `RUSTDOC` to the stage2's copy when the
/// stage2 carries one (and diagnoses when nothing exists anywhere), but a
/// direct cargo invocation has only PATH, and without the link it dies at exec
/// with a raw OS error that names no remedy. The convention that config
/// comment describes — `~/.local/bin/trustdoc` symlinked at the live stage2 —
/// is a provisioned artifact, audited like the rustup link. UNLIKE that link the
/// whole remedy is one mechanical symlink, so a full run REPAIRS it in place: a
/// missing entry or a DANGLING symlink (a swept stage2, say) is linked to the
/// stage2's trustdoc; anything else at that path is somebody's arrangement and
/// is never replaced. `--check` reports the remedy without writing, like every
/// other no-writes path.
#[cfg(unix)]
fn doc_tool_check(home: &str, check_only: bool) -> Check {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    if let Some(found) = resolve_on_path("trustdoc", &path_var) {
        return Check::Pass(format!("trustdoc resolves on PATH ({})", found.display()));
    }
    // A LIVE, working farm entry that PATH simply cannot see is the one state
    // where every other remedy is a dead end: link_farm rightly refuses to
    // replace it, `ln -sf` would recreate the identical link, and only the
    // PATH itself is wrong. Diagnose it FIRST, so the audit cannot loop
    // forever handing out fixes that change nothing.
    let farm = Path::new(home).join(".local/bin/trustdoc");
    if executable(&farm) {
        return Check::Fail {
            what: format!(
                "{} is a working doc driver, but its directory is not on PATH",
                farm.display()
            ),
            fix: path_persist_fix(farm.parent().unwrap_or(Path::new("~/.local/bin"))),
        };
    }
    let stage2 = match gates::trust_stage2_bin() {
        Ok(b) => b,
        Err(_) => return Check::Skip("no stage2 toolchain — see the toolchain line".into()),
    };
    let target = stage2.join("trustdoc");
    if !target.is_file() {
        return Check::Skip("the stage2 lacks trustdoc — see the verifiers line".into());
    }
    if !executable(&target) {
        // A farm link at a driver that cannot exec would turn this audit's
        // Pass into cargo's raw exec error — the defect is the stage2's, and
        // linking it would only relocate the death.
        return Check::Fail {
            what: format!("{} exists but is not executable", target.display()),
            fix: "an incomplete stage2 — reinstall it (`aterm pkg install trust`, then \
                  `aterm pkg doctor`) or, from source, rebuild it (`python3 x.py build \
                  --stage 2` in $HOME/trust)"
                .into(),
        };
    }
    // `-sf` so the remedy is runnable over the dangling link this check
    // repairs; on anything LIVE at that path the operator is choosing to
    // replace it, which is exactly what typing the command states.
    let fix = format!(
        "ln -sf {} {} — the farm link `[build] rustdoc = \"trustdoc\"` \
         (.cargo/config.toml) resolves; a full `cargo ship provision` run repairs it",
        target.display(),
        farm.display(),
    );
    if check_only {
        return Check::Fail {
            what: "no `trustdoc` resolves on PATH — a plain `cargo test`'s doctest lane \
                   dies at exec"
                .into(),
            fix,
        };
    }
    if let Err(why) = link_farm(&farm, &target) {
        return Check::Fail { what: why, fix };
    }
    if resolve_on_path("trustdoc", &path_var).is_some() {
        Check::Pass(format!("linked {} → {}", farm.display(), target.display()))
    } else {
        Check::Fail {
            what: format!(
                "linked {} → {}, but its directory is not on PATH",
                farm.display(),
                target.display()
            ),
            fix: path_persist_fix(farm.parent().unwrap_or(Path::new("~/.local/bin"))),
        }
    }
}

/// First executable `name` on `path` — the same PATH walk cargo's bare-name
/// `[build] rustdoc` key resolves through. Pure over the PATH value, so it is
/// testable with a scratch dir. The mode test is the any-exec-bit heuristic,
/// not `access(2)`'s uid/gid arithmetic: a candidate the invoking user cannot
/// actually exec (a root-only 0700, say) is accepted here and still fails at
/// spawn — the audit prefers a nameable near-miss over a permission model it
/// would get subtly wrong.
#[cfg(unix)]
fn resolve_on_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| executable(p))
}

/// A regular file with any exec bit set (following symlinks, as spawn does).
#[cfg(unix)]
fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Create `farm` → `target`, repairing only what is safely mechanical: nothing
/// there, or a symlink that resolves to NOTHING FOUND. An existing regular
/// file, directory, or LIVE symlink is never replaced — it is somebody's
/// arrangement, and silently overwriting it is how a provisioning verb becomes
/// a footgun. "Dangling" is judged on `NotFound` ALONE: a follow that fails
/// for any other reason (an unreadable directory on the link's path, a
/// symlink loop) proves nothing about the target's existence, so those refuse
/// too rather than destroy a link that may be live.
///
/// The write is stage-and-promote: the new symlink is born complete under a
/// staged name and RENAMED into place, so the farm entry is never half-made and
/// a dangling link is replaced in one atomic step rather than a
/// delete-then-create window. The classify→promote gap is the residual a
/// single-operator `~/.local/bin` accepts — an entry someone races into that gap
/// is either replaced by the promote or fails it (a raced-in directory makes the
/// rename error), never half-made.
///
/// One link is ONE name, so one rename is the whole story. The roster PAIR is
/// two names and no syscall renames two at once, which is why it publishes
/// through the redo transaction instead of this discipline (see [`write_pair`]).
#[cfg(unix)]
fn link_farm(farm: &Path, target: &Path) -> std::result::Result<(), String> {
    match std::fs::symlink_metadata(farm) {
        // Nothing there — the ordinary case. NotFound ONLY: a stat that fails
        // any other way (an unreadable ancestor, say) may be hiding a live
        // entry, so it refuses below rather than write blind.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "cannot inspect {}: {e} — refusing to write what it cannot see",
                farm.display()
            ));
        }
        // Dangling: following the link finds nothing, so replacing it destroys
        // nothing.
        Ok(m)
            if m.file_type().is_symlink()
                && matches!(&std::fs::metadata(farm),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound) => {}
        Ok(_) => {
            return Err(format!(
                "{} exists and is not a dangling symlink — refusing to replace it",
                farm.display()
            ));
        }
    }
    let dir = farm
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", farm.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut staged = farm.as_os_str().to_owned();
    staged.push(".provision.tmp");
    let staged = PathBuf::from(staged);
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged)
        .map_err(|e| format!("stage {} → {}: {e}", staged.display(), target.display()))?;
    std::fs::rename(&staged, farm).map_err(|e| {
        // A failed promote must not strand its staged link (the next run's
        // pre-clean would eat it, but nothing else ever reads the name).
        let _ = std::fs::remove_file(&staged);
        format!("rename {} into place: {e}", staged.display())
    })
}

/// The rustup front door: `cargo` in this repo must dispatch INTO the trust stage2 —
/// that is what makes `cargo ship …` a Trust invocation and not stock Cargo. The link
/// is a provisioned artifact (`rustup toolchain link trust <stage2>`), so it is
/// audited like one.
#[cfg(unix)]
fn front_door_check(bin: &Path) -> Check {
    let out = Command::new("rustup")
        .env("RUSTUP_TOOLCHAIN", "trust")
        .args(["which", "cargo"])
        .output();
    let fix = format!(
        "rustup toolchain link trust {} (then `cargo` in this repo dispatches into the \
         Trust toolchain via rust-toolchain.toml)",
        bin.parent().unwrap_or(bin).display()
    );
    match out {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // The link must point at a REAL trust toolchain, not a stray dir. The
            // canonical stage2 and the linked one may be different installs (atpkg
            // store vs $HOME/trust); what matters is that the linked dir carries trustc.
            let linked_ok = Path::new(&path)
                .parent()
                .map(|bin| bin.join("trustc").is_file() || bin.join("rustc").is_file())
                .unwrap_or(false);
            if linked_ok {
                Check::Pass(format!("rustup 'trust' toolchain linked ({path})"))
            } else {
                Check::Fail {
                    what: format!(
                        "rustup 'trust' resolves to {path}, which does not look like a \
                         Trust toolchain bin"
                    ),
                    fix,
                }
            }
        }
        _ => Check::Fail {
            what: "rustup has no 'trust' toolchain — `cargo` in this repo cannot \
                   dispatch (rust-toolchain.toml pins channel = \"trust\")"
                .into(),
            fix,
        },
    }
}

/// The x86_64 compat slice of the universal binary rides upstream stable.
#[cfg(unix)]
fn x86_slice_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("universal DMG builds run on macOS".into());
    }
    match gates::x86_target_probe() {
        Ok(()) => {
            Check::Pass("stable x86_64-apple-darwin target installed (universal slice)".into())
        }
        // The WHOLE fault, and the shared remedy text. The probe no longer carries a
        // hand-indented `fix:`/`or:` block of its own, so nothing has to be truncated to
        // keep this line on the grid — and truncating it used to throw away the rustup
        // branch's real explanation (that this is a choice about what to ship, not a
        // broken setup) on exactly the machine that needed it.
        Err(e) => Check::Fail {
            what: e.to_string(),
            fix: gates::X86_SLICE_REMEDIES.to_string(),
        },
    }
}

/// Apple's bundle/sign/package command-line tools — the cut shells all of them.
#[cfg(unix)]
fn apple_clt_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE)".into());
    }
    let missing: Vec<&str> = [
        "/usr/bin/codesign",
        "/usr/bin/dsymutil",
        "/usr/bin/hdiutil",
        "/usr/bin/lipo",
        "/usr/bin/ditto",
        "/usr/bin/xcrun",
    ]
    .iter()
    .copied()
    .filter(|t| !Path::new(t).is_file())
    .collect();
    if missing.is_empty() {
        Check::Pass("bundle/sign/package command-line tools present".into())
    } else {
        Check::Fail {
            what: format!("missing {}", missing.join(", ")),
            fix: "xcode-select --install".into(),
        }
    }
}

/// This machine's own Developer ID identity, ACQUIRED rather than copied: [`crate::apple`]
/// mints the keypair and CSR here and imports the certificate Apple issues against them.
/// A private key never crosses a machine boundary — the `.p12` transfer this check used to
/// prescribe is exactly what the rest of the design exists to prevent.
///
/// Apple allows no unattended path (see [`crate::apple`] for the three that were measured
/// and refused), so the acquisition waits for the operator's one browser errand rather
/// than making them re-run the command.
///
/// Returns the identity the credentials profile should pin beside the audit line, because
/// that is where it came from. It used to be recovered by regex from the printed sentence,
/// which meant the pin was decided by "whichever SHA-1 appeared first in some prose" — and
/// the prose it was scraped out of also contains a filesystem path.
#[cfg(unix)]
fn apple_identity_check(id: &str, may_change: bool) -> (Check, Option<String>) {
    match crate::apple::acquire(id, may_change) {
        crate::apple::Outcome::Ready { ids, note } => {
            // `ids` holds only SHA-1s, so this count is a count of certificates. `verdict`
            // proves ids[0] can actually sign, so ids[0] is the one to pin: a profile
            // naming any other would pin a certificate nothing has demonstrated.
            let mut msg = match ids.len() {
                0 => {
                    return (
                        Check::Fail {
                            what: "no Developer ID Application identity after acquisition".into(),
                            fix: format!("re-run `cargo ship provision --id {id}`"),
                        },
                        None,
                    );
                }
                1 => format!("Developer ID Application [{}]", ids[0]),
                n => format!(
                    "{n} Developer ID Application certificates; pinning [{}], the one \
                     proved to sign",
                    ids[0]
                ),
            };
            if let Some(note) = note {
                msg.push_str(&format!("; {note}"));
            }
            (Check::Pass(msg), Some(ids[0].clone()))
        }
        crate::apple::Outcome::Waiting { what, next }
        | crate::apple::Outcome::Todo { what, next } => (Check::Todo { what, next }, None),
        crate::apple::Outcome::Blocked { what, fix } => (Check::Fail { what, fix }, None),
        crate::apple::Outcome::Skipped(why) => (Check::Skip(why), None),
    }
}

/// Fail and Todo both block a cut: a machine still waiting on its certificate cannot
/// publish, and must not consume an irreversible roster id on a half-finished audit.
#[cfg(unix)]
fn blockers(checks: &[(&'static str, Check)]) -> usize {
    let (fails, waiting) = tally(checks);
    fails + waiting
}

/// Faults and things merely waiting, counted APART.
///
/// `Todo` exists to say "your certificate request is at Apple" — not a fault to repair —
/// and `print_check` prints it with `next:` rather than `fix:` for exactly that reason.
/// The summary then lumped both into "N item(s) above name their remedy", so the one place
/// an operator reads a verdict was the one place the distinction was thrown away: a
/// piped run with nobody to do the browser errand exited with the word FAILED, and "name
/// their remedy" is wrong for an item whose remedy is to wait.
#[cfg(unix)]
fn tally(checks: &[(&'static str, Check)]) -> (usize, usize) {
    let count = |f: fn(&Check) -> bool| checks.iter().filter(|(_, c)| f(c)).count();
    (
        count(|c| matches!(c, Check::Fail { .. })),
        count(|c| matches!(c, Check::Todo { .. })),
    )
}

/// "1 gap" · "2 gaps, 1 waiting" · "1 waiting" — the summary phrase, singular-aware
/// because the overwhelmingly common case is one of them and "1 item(s)" reads as a bug.
#[cfg(unix)]
fn gaps(fails: usize, waiting: usize) -> String {
    let mut parts = Vec::new();
    if fails > 0 {
        parts.push(format!("{fails} gap{}", if fails == 1 { "" } else { "s" }));
    }
    if waiting > 0 {
        parts.push(format!("{waiting} waiting"));
    }
    parts.join(", ")
}

/// The cut's own admission gate. One spelling, so the pre-Apple check and the post-mint
/// bind cannot drift apart — including the refusal, which is where the whole value is.
#[cfg(unix)]
fn authorize(bytes: Vec<u8>, sig: &[u8], pubkey: &str) -> Result<roster::Attribution> {
    machines::authorize_cut(
        pins::PAPER_MASTER_PUBKEYS,
        bytes,
        sig,
        pubkey,
        now_unix()? as i64,
    )
    .map_err(|e| {
        Error::new(format!(
            "{e}\nprovision: the machine key exists but the roster on disk does not \
             authorize it — if the join's re-signed pair was lost (dist/ is gitignored \
             and can be swept), restore aterm-machines.toml AND .sig from the machine \
             holding the newest generation; if the id was revoked, an id never returns \
             — move the key pair aside and mint a new one"
        ))
    })
}

/// One numbered phase header, and the only place the plan is stated. `provision` can sit
/// waiting for a browser errand for minutes, and a process that says where it is is a
/// process nobody has to guess about — `[3/5]` carries both the position and the length,
/// which is why the list that used to precede these was pure repetition.
#[cfg(unix)]
fn phase(n: usize, name: &str, what: &str) {
    println!();
    println!("  [{n}/5] {name} — {what}");
}

/// Store the notarytool credential if it is absent, then LIVE-check it either way — the
/// acquisition is only believed once Apple has answered through it.
#[cfg(unix)]
fn notary_acquire(may_change: bool) -> Check {
    match notary_check() {
        live @ (Check::Pass(_) | Check::Skip(_)) => live,
        // `--check` keeps the LIVE verdict. Routing it into `ensure_notary` returned a
        // blind "no notarytool credential in the keychain" — a statement that is FALSE
        // about the machine when a credential exists and Apple refused it — and threw
        // away both Apple's own error text and the pasteable `store-credentials` command.
        // `--check` is the mode an operator runs to find out what is wrong BEFORE
        // touching anything; it is the last place to substitute a guess for a measurement.
        live if !may_change => live,
        _ => match crate::apple::ensure_notary(may_change) {
            // Re-run the live check rather than trusting store-credentials' exit status:
            // the thing a cut needs is a credential Apple answers to.
            crate::apple::Outcome::Ready { .. } => notary_check(),
            crate::apple::Outcome::Waiting { what, next }
            | crate::apple::Outcome::Todo { what, next } => Check::Todo { what, next },
            crate::apple::Outcome::Blocked { what, fix } => Check::Fail { what, fix },
            crate::apple::Outcome::Skipped(why) => Check::Skip(why),
        },
    }
}

/// LIVE-tested, not presence-tested: `notarytool history` proves the stored
/// credential actually authenticates against Apple — a stale password passes a
/// keychain presence check and fails at minute twenty of a cut.
///
/// The profile name is [`crate::apple::NOTARY_PROFILE`], not a literal: it is one name,
/// written by `write_credentials_profile`, stored by `ensure_notary`, read by the cut and
/// checked here, and it was spelled out by hand in every one of those places.
#[cfg(unix)]
fn notary_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    let profile = crate::apple::NOTARY_PROFILE;
    match Command::new("xcrun")
        .args(["notarytool", "history", "--keychain-profile", profile])
        .output()
    {
        Ok(out) if out.status.success() => Check::Pass(format!(
            "notarytool profile '{profile}' answers (live-checked)"
        )),
        Ok(out) => Check::Fail {
            what: format!(
                "notarytool profile '{profile}' did not authenticate: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .next()
                    .unwrap_or("(no error line)")
            ),
            fix: format!(
                "xcrun notarytool store-credentials {profile} --apple-id <your-apple-id> \
                 --team-id {} (password: an app-specific password — mint at https://account.apple.com → Sign-In and Security → App-Specific Passwords)",
                pins::APPLE_TEAM_ID
            ),
        },
        Err(e) => Check::Fail {
            what: format!("could not run xcrun notarytool: {e}"),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        },
    }
}

/// The credentials profile a Tier APPLE cut names on its command line. There is no
/// ambient path at cut time — but provisioning needs a real verdict, so the audit checks
/// the conventional location and holds what it finds to the CUT'S OWN admission rules.
///
/// Validated by calling `sign::ReleaseCredentials::load` — literally the call the cut
/// makes — instead of re-scanning for a couple of key names. The two-key scan this
/// replaced proved only that `notary_profile` and `signing_identity_sha1` parsed, while
/// `load` additionally hard-requires `signing_key` (sign.rs: "the one key of record"),
/// demands the file be owner-owned with no group/other access (`check_credentials_perms`,
/// because it holds a private key), and refuses a profile naming two notary credentials.
/// So a hand-written or restored profile could pass this audit — READY TO CUT, every item
/// green — and be refused by `cargo ship cut` on its first line. An audit that admits what
/// the cut rejects is the exact false green this verb exists to abolish, so there is now
/// one validator, not two.
#[cfg(unix)]
fn profile_check(
    home: &str,
    id: &str,
    apple_sha1: Option<&str>,
    machine_pubkey: Option<&str>,
) -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE)".into());
    }
    let path = Path::new(home).join(".aterm/release-credentials.toml");
    // provision WRITES this file. It is not one the operator can hand-write: `signing_key`
    // is a copy of `~/.aterm/machine.key`, which on the run that needs this remedy may not
    // exist yet. And an existing profile is never clobbered (`create_new`, apple.rs), so
    // the remedy for a bad one is to move it aside, not to edit it.
    let rewrite = format!(
        "move {} aside and re-run `cargo ship provision --id {id}` — provision writes it",
        path.display()
    );
    if !path.exists() {
        // The profile is written only when `apple_sha1.is_some()`, so on the run that
        // reaches this arm with no certificate the prescribed re-run writes NOTHING. The
        // operator would follow the tool's own advice into a loop, and the run would
        // invent a phantom second item to fix. Report the DEPENDENCY, and count it as
        // waiting rather than as a gap — it still blocks, and the mint gate is unchanged.
        if apple_sha1.is_none() {
            return Check::Todo {
                what: "not written yet: it pins the Developer ID certificate, and the \
                       apple id line above has not produced one"
                    .into(),
                next: "nothing to do here — settle the apple id line and this writes itself".into(),
            };
        }
        return Check::Fail {
            what: format!("no credentials profile at {}", path.display()),
            fix: format!("re-run `cargo ship provision --id {id}` — provision writes it"),
        };
    }
    let creds = match sign::ReleaseCredentials::load(&path) {
        Ok(c) => c,
        // `load`'s error already names the file and the exact defect — the missing
        // `signing_key`, the mode the cut refuses, a key that is not PKCS#8 Ed25519 — in
        // the words the cut itself would use.
        Err(e) => {
            return Check::Fail {
                what: e,
                fix: rewrite,
            };
        }
    };
    // Everything below is what `load` cannot know: the world this run just measured.
    if creds.notary().is_none() {
        return Check::Fail {
            what: format!("{} names no notarytool credential", path.display()),
            fix: rewrite,
        };
    }
    // Declaring `machine_id` is optional; declaring it WRONG is fatal at cut time, so a
    // profile carried over from another machine is refused here instead of there.
    if let Some(declared) = creds.machine_id().filter(|d| *d != id) {
        return Check::Fail {
            what: format!(
                "{} declares machine_id = \"{declared}\", not '{id}'",
                path.display()
            ),
            fix: rewrite,
        };
    }
    // `signing_key` is a COPY of ~/.aterm/machine.key taken at first write, and the file is
    // never rewritten (`create_new`, apple.rs) — so nothing re-compares the two. After a
    // re-mint (the remedy this verb itself prescribes) the profile still holds the RETIRED
    // key while `authority` below proves the new one, and the cut loads the profile's copy,
    // not machine.key. So the copy is what has to match.
    if let Some(mine) = machine_pubkey.filter(|m| creds.pubkey() != *m) {
        return Check::Fail {
            what: format!(
                "{} signs as {}, but this machine's key is {mine}",
                path.display(),
                creds.pubkey()
            ),
            fix: rewrite,
        };
    }
    // `machine_roster` is frozen at first write as an absolute path into one checkout, and
    // the cut reads it. dist/ is gitignored and sweepable, so a profile can outlive the
    // roster it names — this run restored or installed the pair moments ago, and the cut
    // will look wherever the profile says.
    if let Some(named) = creds.machine_roster().filter(|r| !r.exists()) {
        return Check::Fail {
            what: format!(
                "{} names a machine_roster that is not there: {}",
                path.display(),
                named.display()
            ),
            fix: rewrite,
        };
    }
    match (creds.signing_identity_sha1(), apple_sha1) {
        // The pin names a different certificate than the one this run happened to prove
        // first. With more than one valid Developer ID identity installed that is NOT yet a
        // gap — `security find-identity` orders them unstably (measured 2026-08-18: two
        // certificates, alternate runs blessed each), and the cut accepts any valid pinned
        // identity that can sign (`select_devid_identity`). So prove the PINNED one; only a
        // pin that names nothing valid, or a certificate that cannot sign unattended (a
        // renewal, a profile carried over from a re-certified machine), is the failure the
        // audit exists to move to the front.
        (Some(pinned), Some(proved)) if !pinned.eq_ignore_ascii_case(proved) => {
            match crate::apple::pinned_identity_proves(pinned) {
                Ok(()) => Check::Pass(format!(
                    "{} pins signing_identity_sha1 {pinned} — installed, valid and proved \
                     to sign (this run's first-listed identity was {proved}; both are \
                     usable)",
                    path.display()
                )),
                Err(why) => Check::Fail {
                    what: format!(
                        "{} pins signing_identity_sha1 {pinned}, which cannot sign here \
                         ({why}); this machine signs with {proved}",
                        path.display()
                    ),
                    fix: rewrite,
                },
            }
        }
        // An absent pin is NOT a gap: the cut accepts it whenever the keychain holds
        // exactly one Developer ID Application certificate, and refusing here would be the
        // mirror of the defect above — the audit rejecting what the cut admits.
        //
        // The line claims exactly what was demonstrated: `load` accepted this file. It
        // does not list the keys, because listing them is what the old check did instead
        // of proving them.
        _ => Check::Pass(format!(
            "{} loads under the cut's own rules",
            path.display()
        )),
    }
}

#[cfg(unix)]
fn gh_check() -> Check {
    match Command::new("gh").args(["auth", "status"]).output() {
        Ok(out) if out.status.success() => Check::Pass("gh is authenticated".into()),
        Ok(_) => Check::Fail {
            what: "gh is not authenticated".into(),
            fix: "gh auth login".into(),
        },
        Err(e) => Check::Fail {
            what: format!("could not run gh: {e}"),
            fix: "install the GitHub CLI (`brew install gh`), then `gh auth login`".into(),
        },
    }
}

/// The channel repo is a different org than the dev remote, so `gh`'s default account
/// cannot publish there — the cut threads a dedicated token from a file. Presence AND
/// mode are checked; push permission itself is proven by the cut's own preflight.
///
/// A group/other-readable token is TIGHTENED rather than reported. This verb already
/// downloads an Apple intermediate CA and imports it into the login keychain, generates
/// RSA keys, and writes ~/.aterm — against that autonomy budget, printing `chmod 600 <p>`
/// and then deferring an irreversible mint over one mode bit costs the operator a full
/// re-run, including a live round trip to Apple's notary service, for one syscall. Under
/// `--check` it is still only reported: that flag's whole promise is that nothing changes.
#[cfg(unix)]
fn channel_token_check(slug: &str, may_change: bool) -> Check {
    let where_it_lives = publish::channel_token_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.secrets/gh_access_token_alabsystems".into());
    // "mint", not "copy": a per-machine token revokes per-machine, the same reason the
    // signing key never travels. That is the design's reason, not the operator's next
    // action, so it is a comment here and not a second sentence on the remedy line.
    // The URL matters more here than anywhere else in the crate: the CLASSIC-token page
    // is the easy wrong turn from a bare "mint a PAT", and it produces the wrong scopes
    // for a fine-grained-only org — a token that exists, reads as done, and fails at the
    // upload step of a real cut. Every other credential remedy in this file gives a full
    // click path; this one gave none.
    let fix = format!(
        "1. https://github.com/settings/personal-access-tokens/new  (FINE-GRAINED, not \
         the classic-token page)\n\
         2. Contents: read/write on {slug}, short expiry\n\
         3. (umask 077; cat > {where_it_lives})   (then paste it and press ^D)"
    );
    match publish::channel_token() {
        None => Check::Fail {
            what: format!("no channel token at {where_it_lives}"),
            fix,
        },
        Some(_) => {
            // The token resolved; now hold its file to the same owner-only standard
            // as every other credential.
            use std::os::unix::fs::PermissionsExt as _;
            let path = publish::channel_token_path();
            let open = path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.permissions().mode() & 0o077 != 0)
                .unwrap_or(false);
            if !open {
                // Names the FILE. "channel token present" spent its first two words
                // restating the label and never said WHERE, so an operator with a stale
                // token in one location and a fresh one in another could not tell which
                // the cut would use without reading `publish::channel_token_path`.
                return Check::Pass(format!("{where_it_lives}  0600, owner-only"));
            }
            let tightened = may_change
                && path.is_some_and(|p| {
                    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).is_ok()
                });
            if tightened {
                Check::Pass(format!("{where_it_lives}  tightened to 0600"))
            } else {
                Check::Fail {
                    what: format!("{where_it_lives} is group/other-accessible"),
                    fix: format!("chmod 600 {where_it_lives}"),
                }
            }
        }
    }
}

/// The Developer ID Application identities for `team_id` in a `security find-identity
/// -v -p codesigning` listing — pure over the captured output, so it is testable
/// without a keychain. Returns the 40-hex SHA-1 of each matching line.
#[cfg(unix)]
pub(crate) fn devid_identities(listing: &str, team_id: &str) -> Vec<String> {
    let team_tag = format!("({team_id})");
    listing
        .lines()
        .filter(|l| l.contains("Developer ID Application:") && l.contains(&team_tag))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|t| t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
fn utf8_path(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::new(format!("{} is not valid UTF-8", p.display())))
}

/// Unix seconds, failing rather than guessing — the mint stamps `added_at` from this.
#[cfg(unix)]
fn now_unix() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::new("the system clock is before the unix epoch"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use aterm_update_core::roster::{Machine, SUPPORTED_SCHEMA};

    /// A synthetic paper phrase, built at runtime so no phrase-shaped literal exists in
    /// this file (grep_guard B7 scans it).
    fn seed() -> atpkg_keys::master::MasterSeed {
        let phrase = "2".repeat(52);
        atpkg_keys::master::parse_master(&phrase)
            .expect("synthetic phrase")
            .seed()
    }

    fn pubkey_of(byte: u8) -> String {
        aterm_codec::base64::encode(&[byte; 32]).expect("32-byte key")
    }

    fn roster_with(seq: u64, machines: Vec<Machine>, revoked: Vec<String>) -> Roster {
        Roster {
            schema: SUPPORTED_SCHEMA,
            roster_seq: seq,
            valid_until: "9999-12-31T00:00:00Z".into(),
            machines,
            revoked,
        }
    }

    fn machine(id: &str, key_byte: u8) -> Machine {
        Machine {
            id: id.into(),
            pubkey: pubkey_of(key_byte),
            added_at: "2026-01-01T00:00:00Z".into(),
            not_after: None,
        }
    }

    fn signed_candidate(seq: u64) -> (String, Vec<u8>, Vec<u8>) {
        signed_candidate_with(seq, vec![machine("m3", 0x42)])
    }

    fn signed_candidate_with(seq: u64, machines: Vec<Machine>) -> (String, Vec<u8>, Vec<u8>) {
        let s = seed();
        let r = roster_with(seq, machines, vec![]);
        let bytes = r.to_toml().expect("valid roster").into_bytes();
        let sig = s.sign(&bytes).expect("sign");
        (s.pubkey_b64().expect("pubkey"), bytes, sig)
    }

    #[test]
    fn admission_requires_the_master_signature() {
        let (master, bytes, sig) = signed_candidate(7);
        let ok = admit_candidate(&[&master], bytes.clone(), sig.clone()).expect("admits");
        assert_eq!(ok.roster.roster_seq, 7);

        let mut tampered = bytes.clone();
        tampered[0] ^= 1;
        assert!(admit_candidate(&[&master], tampered, sig.clone()).is_err());

        let other = pubkey_of(0x99);
        assert!(admit_candidate(&[&other], bytes, sig).is_err());
    }

    #[test]
    fn the_newest_generation_wins_and_never_downgrades() {
        let (master, b, s) = signed_candidate(3);
        let older = admit_candidate(&[&master], b, s).unwrap();
        let (_, b, s) = signed_candidate(5);
        let newer = admit_candidate(&[&master], b, s).unwrap();
        let path = Path::new("dist/aterm-machines.toml");

        // channel newer → install
        let (_, b, s) = signed_candidate(3);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (chosen, install, how) = choose_candidate(path, "o/r", Some(local), Ok(newer)).unwrap();
        assert_eq!(chosen.roster.roster_seq, 5);
        assert!(install);
        assert!(how.contains("3 → 5"), "{how}");

        // local ahead → keep, never downgrade
        let (_, b, s) = signed_candidate(9);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (chosen, install, how) = choose_candidate(path, "o/r", Some(local), Ok(older)).unwrap();
        assert_eq!(chosen.roster.roster_seq, 9);
        assert!(!install);
        assert!(how.contains("AHEAD"), "{how}");

        // equal AND byte-identical → keep local
        let (_, b, s) = signed_candidate(4);
        let local = admit_candidate(&[&master], b.clone(), s.clone()).unwrap();
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, how) = choose_candidate(path, "o/r", Some(local), Ok(fetched)).unwrap();
        assert!(!install);
        assert!(how.contains("byte-identical"), "{how}");

        // nothing local, fetch ok → install
        let (_, b, s) = signed_candidate(2);
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, _) = choose_candidate(path, "o/r", None, Ok(fetched)).unwrap();
        assert!(install);

        // fetch failed, local present → keep with the failure named
        let (_, b, s) = signed_candidate(2);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, how) =
            choose_candidate(path, "o/r", Some(local), Err("offline".into())).unwrap();
        assert!(!install);
        assert!(how.contains("offline"), "{how}");

        // nothing anywhere → the error names the manual remedy and the URL
        let err = choose_candidate(path, "owner/repo", None, Err("offline".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("aterm-machines.toml.sig"), "{err}");
        assert!(
            err.contains("github.com/owner/repo/releases/latest/download"),
            "{err}"
        );
    }

    #[test]
    fn equal_generation_with_different_bytes_is_a_lineage_fork_and_a_hard_stop() {
        let (master, b, s) = signed_candidate_with(4, vec![machine("m3", 0x42)]);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (_, b, s) = signed_candidate_with(4, vec![machine("m3", 0x42), machine("mx", 0x43)]);
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let err = choose_candidate(
            Path::new("dist/aterm-machines.toml"),
            "o/r",
            Some(local),
            Ok(fetched),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("LINEAGE FORK"), "{err}");
        assert!(err.contains("Do not mint"), "{err}");
    }

    #[test]
    fn local_roster_state_that_cannot_be_proven_is_a_hard_stop_not_a_reseed() {
        let dir = std::env::temp_dir().join(format!("provision-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm-machines.toml");

        // Half pair: body without signature.
        std::fs::write(&path, b"schema = 1\n").unwrap();
        let err = read_local_candidate(&path).unwrap_err().to_string();
        assert!(err.contains("half a roster pair"), "{err}");

        // Orphan signature: signature without body.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(machines::RosterDocument::signature_path(&path), [0u8; 64]).unwrap();
        let err = read_local_candidate(&path).unwrap_err().to_string();
        assert!(err.contains("half a roster pair"), "{err}");

        // Full pair that does not verify: hard stop, never treated as absent.
        std::fs::write(&path, b"schema = 1\n").unwrap();
        let err = read_local_candidate(&path).unwrap_err().to_string();
        assert!(err.contains("Refusing to overwrite"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn devid_parsing_filters_by_kind_and_team() {
        let listing = concat!(
            "  1) 1111111111111111111111111111111111111111 \"Developer ID Application: A (TEAMIDXXXX)\"\n",
            "  2) 2222222222222222222222222222222222222222 \"Apple Development: A (TEAMIDXXXX)\"\n",
            "  3) 3333333333333333333333333333333333333333 \"Developer ID Application: A (OTHERTEAMX)\"\n",
            "  4) 4444444444444444444444444444444444444444 \"Developer ID Application: B (TEAMIDXXXX)\"\n",
            "     4 valid identities found\n",
        );
        assert_eq!(
            devid_identities(listing, "TEAMIDXXXX"),
            vec![
                "1111111111111111111111111111111111111111".to_string(),
                "4444444444444444444444444444444444444444".to_string(),
            ]
        );
        assert!(devid_identities("", "TEAMIDXXXX").is_empty());
    }

    /// A fresh Ed25519 keypair as base64 PKCS#8 — the shape `signing_key` carries — with
    /// the public identity it derives, which is what the audit compares against the key
    /// this machine actually holds. Its only callers are the macOS-gated audit tests.
    #[cfg(target_os = "macos")]
    fn signing_key_b64() -> (String, String) {
        use ring::signature::KeyPair as _;
        let rng = ring::rand::SystemRandom::new();
        let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keypair");
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).expect("keypair");

        (
            aterm_codec::base64::encode(doc.as_ref()).expect("pkcs8"),
            aterm_codec::base64::encode(pair.public_key().as_ref()).expect("32-byte key"),
        )
    }

    /// The audit must refuse exactly what the cut refuses. Each profile below is a way
    /// the old two-key scan reported "carries the Tier APPLE keys" — and READY TO CUT —
    /// about a file `cargo ship cut` rejects on its first line.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_profile_audit_refuses_what_the_cut_refuses() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = std::env::temp_dir().join(format!("provision-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".aterm")).unwrap();
        let path = home.join(".aterm/release-credentials.toml");
        let home_s = home.to_str().unwrap().to_string();
        let sha1 = "0".repeat(40);

        let write = |body: &str, mode: u32| {
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let refusal = |c: Check| match c {
            Check::Fail { what, .. } => what,
            Check::Pass(msg) => panic!("expected a refusal, got PASS: {msg}"),
            _ => panic!("expected a refusal"),
        };

        let (key, pubkey) = signing_key_b64();
        let mine = Some(pubkey.as_str());

        // Absent: the remedy is this tool, never a hand-written file.
        let Check::Fail { what, fix } = profile_check(&home_s, "m9", Some(&sha1), mine) else {
            panic!("an absent profile is a gap");
        };
        assert!(what.contains("no credentials profile"), "{what}");
        assert!(fix.contains("provision writes it"), "{fix}");

        // What the OLD remedy told the operator to write: its two keys, and nothing
        // else. It passed the audit and died at `credentials_signing_key`.
        write(
            &format!("notary_profile = \"notary\"\nsigning_identity_sha1 = \"{sha1}\"\n"),
            0o600,
        );
        let what = refusal(profile_check(&home_s, "m9", Some(&sha1), mine));
        assert!(what.contains("signing_key"), "{what}");

        let good = format!(
            "signing_key = \"{key}\"\nmachine_id = \"m9\"\nnotary_profile = \"notary\"\n\
             signing_identity_sha1 = \"{sha1}\"\n"
        );
        // The mode the cut refuses — it holds a private key (a restore under umask 022).
        write(&good, 0o644);
        let what = refusal(profile_check(&home_s, "m9", Some(&sha1), mine));
        assert!(what.contains("group/other-accessible"), "{what}");

        // A pin naming a certificate this machine does not hold: the renewal case, which
        // `select_devid_identity` refuses twenty minutes into a cut.
        write(&good, 0o600);
        let what = refusal(profile_check(&home_s, "m9", Some(&"1".repeat(40)), mine));
        assert!(what.contains("pins signing_identity_sha1"), "{what}");

        // A profile carried over from another machine.
        let what = refusal(profile_check(&home_s, "m8", Some(&sha1), mine));
        assert!(what.contains("machine_id"), "{what}");

        // The profile's `signing_key` is a COPY, frozen at first write. After a re-mint it
        // is the RETIRED key — and it is the one the cut loads, so the roster that
        // authorizes ~/.aterm/machine.key does not authorize this file.
        let (_, other) = signing_key_b64();
        let what = refusal(profile_check(&home_s, "m9", Some(&sha1), Some(&other)));
        assert!(what.contains("this machine's key is"), "{what}");

        // A roster path that no longer exists: dist/ is gitignored and sweepable, and the
        // cut reads whatever the profile names.
        write(
            &format!(
                "{good}machine_roster = \"{}\"\n",
                home.join("gone/roster.toml").display()
            ),
            0o600,
        );
        let what = refusal(profile_check(&home_s, "m9", Some(&sha1), mine));
        assert!(what.contains("machine_roster"), "{what}");

        // Everything the loader demands, agreeing with the world this run measured.
        write(&good, 0o600);
        assert!(matches!(
            profile_check(&home_s, "m9", Some(&sha1), mine),
            Check::Pass(_)
        ));
        // No pin is not a gap: the cut accepts that whenever one certificate is installed.
        write(
            &format!("signing_key = \"{key}\"\nnotary_profile = \"notary\"\n"),
            0o600,
        );
        assert!(matches!(
            profile_check(&home_s, "m9", Some(&sha1), mine),
            Check::Pass(_)
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `Todo` is "your certificate request is at Apple", not a fault, and the summary is
    /// the one line an operator reads as a verdict. It used to call both FAILED.
    #[test]
    fn waiting_on_apple_is_counted_apart_from_a_fault() {
        let fail = || Check::Fail {
            what: String::new(),
            fix: String::new(),
        };
        let todo = || Check::Todo {
            what: String::new(),
            next: String::new(),
        };
        let checks = vec![
            ("a", fail()),
            ("b", todo()),
            ("c", Check::Pass(String::new())),
        ];
        assert_eq!(tally(&checks), (1, 1));
        assert_eq!(gaps(1, 1), "1 gap, 1 waiting");
        // Singular-aware: "1 item(s)" in the commonest case reads as a bug.
        assert_eq!(gaps(1, 0), "1 gap");
        assert_eq!(gaps(2, 0), "2 gaps");
        assert_eq!(gaps(0, 1), "1 waiting");
        // Both still defer the mint — a roster id is irreversible either way.
        assert_eq!(blockers(&checks), 2);
    }

    /// The generation a mint writes lives only in gitignored `dist/` until some later cut
    /// publishes it, so a `git clean -xdf` in between can destroy the only copy of a
    /// roster whose certificate slot is already spent.
    #[test]
    fn the_authorized_generation_is_kept_outside_the_sweepable_checkout() {
        let dir = std::env::temp_dir().join(format!("provision-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dist = dir.join("dist").join("aterm-machines.toml");
        let home = dir.join("home");
        let kept = kept_roster_path(home.to_str().unwrap());
        // The phase's own writer lock, held across its reads and writes exactly as
        // `run_provision` holds it — a `dist/` write cannot be spelled without one.
        let lock = lock_roster_pair(&dist).unwrap();

        // Nothing kept, nothing local: a no-op, not an error.
        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());

        let (master, bytes, sig) = signed_candidate(6);
        let c = admit_candidate(&[&master], bytes, sig).unwrap();
        install_pair(&lock, &dist, &c).unwrap();

        // The pair `authorize_cut` accepted is copied beside the key it authorizes…
        assert!(keep_roster_pair(&dist, &kept).unwrap());
        let (a, b) = (
            machines::RosterDocument::read(&dist).unwrap(),
            machines::RosterDocument::read(&kept).unwrap(),
        );
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(a.signature, b.signature);
        // …once: a provisioned machine does not repeat the line on every audit.
        assert!(!keep_roster_pair(&dist, &kept).unwrap());

        // dist/ still holds a pair, so nothing is restored over it.
        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());

        // A TORN kept copy is REFUSED, not overwritten. The no-downgrade rule needs both
        // halves to read a generation, so a copy missing one cannot be compared — and
        // replacing it blind is exactly how the only witness to a newer generation would
        // be destroyed. The refusal names both files and the run continues.
        let kept_sig = machines::RosterDocument::signature_path(&kept);
        let sig_bytes = std::fs::read(&kept_sig).unwrap();
        std::fs::remove_file(&kept_sig).unwrap();
        let refused = keep_roster_pair(&dist, &kept).unwrap_err().to_string();
        assert!(
            refused.contains("missing one half or unreadable"),
            "{refused}"
        );
        assert!(!kept_sig.exists(), "a refused copy wrote nothing");
        std::fs::write(&kept_sig, &sig_bytes).unwrap();
        assert!(
            !keep_roster_pair(&dist, &kept).unwrap(),
            "and it is whole again"
        );

        // Swept. The kept copy is re-admitted under `PAPER_MASTER_PUBKEYS` before it is
        // written — this fixture is signed by a synthetic master, so it is IGNORED rather
        // than installed, and rather than turned into a hard error: the channel is still
        // a source, and a cache that cannot be proven is not one.
        std::fs::remove_file(&dist).unwrap();
        std::fs::remove_file(machines::RosterDocument::signature_path(&dist)).unwrap();
        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());
        assert!(!dist.exists());

        // A torn kept copy is ignored for the same reason, and never propagated.
        std::fs::remove_file(machines::RosterDocument::signature_path(&kept)).unwrap();
        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_asset_url_is_the_anonymous_latest_release_path() {
        assert_eq!(
            release_asset_url("alabsystems/aterm", "aterm-machines.toml"),
            "https://github.com/alabsystems/aterm/releases/latest/download/aterm-machines.toml"
        );
    }

    #[test]
    fn path_resolution_finds_only_a_real_executable_and_in_path_order() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("provision-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let joined = std::env::join_paths([&a, &b]).unwrap();

        // Nothing anywhere.
        assert_eq!(resolve_on_path("trustdoc", &joined), None);

        // A non-executable file is not a doc driver — exec would still fail.
        std::fs::write(b.join("trustdoc"), b"").unwrap();
        assert_eq!(resolve_on_path("trustdoc", &joined), None);

        // Executable in the later dir resolves…
        std::fs::set_permissions(b.join("trustdoc"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert_eq!(
            resolve_on_path("trustdoc", &joined),
            Some(b.join("trustdoc"))
        );

        // …and the earlier dir wins once it has one, exactly like exec.
        std::fs::write(a.join("trustdoc"), b"").unwrap();
        std::fs::set_permissions(a.join("trustdoc"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert_eq!(
            resolve_on_path("trustdoc", &joined),
            Some(a.join("trustdoc"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_farm_link_is_created_repairs_a_dangling_link_and_replaces_nothing_else() {
        let dir = std::env::temp_dir().join(format!("provision-farm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("stage2-trustdoc");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();
        let farm = dir.join("farm/bin/trustdoc");

        // Fresh: parent dirs and the link are created, no staged residue left.
        link_farm(&farm, &target).expect("create");
        assert_eq!(std::fs::read_link(&farm).unwrap(), target);
        let staged = {
            let mut s = farm.as_os_str().to_owned();
            s.push(".provision.tmp");
            std::path::PathBuf::from(s)
        };
        assert!(
            std::fs::symlink_metadata(&staged).is_err(),
            "the staged name never survives a promote"
        );

        // Dangling (a swept stage2): repaired in place.
        std::fs::remove_file(&farm).unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), &farm).unwrap();
        link_farm(&farm, &target).expect("repair dangling");
        assert_eq!(std::fs::read_link(&farm).unwrap(), target);

        // A link whose follow fails for a NON-NotFound reason (an unreadable
        // directory on its path) proves NOTHING about its target — refused,
        // never treated as dangling. (Under root the follow succeeds instead
        // and the link is simply live — refused by the same arm.)
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::remove_file(&farm).unwrap();
            let sealed = dir.join("sealed");
            std::fs::create_dir_all(&sealed).unwrap();
            std::fs::write(sealed.join("trustdoc"), b"#!/bin/sh\n").unwrap();
            std::os::unix::fs::symlink(sealed.join("trustdoc"), &farm).unwrap();
            std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
            let refused = link_farm(&farm, &target);
            std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
            let err = refused.unwrap_err();
            assert!(err.contains("refusing to replace"), "{err}");
        }

        // A LIVE symlink is somebody's arrangement — refused, and untouched.
        std::fs::remove_file(&farm).unwrap();
        std::os::unix::fs::symlink(&target, &farm).unwrap();
        let other = dir.join("other-trustdoc");
        std::fs::write(&other, b"#!/bin/sh\n").unwrap();
        let err = link_farm(&farm, &other).unwrap_err();
        assert!(err.contains("refusing to replace"), "{err}");
        assert_eq!(std::fs::read_link(&farm).unwrap(), target);

        // A regular file too.
        std::fs::remove_file(&farm).unwrap();
        std::fs::write(&farm, b"not a link").unwrap();
        let err = link_farm(&farm, &target).unwrap_err();
        assert!(err.contains("refusing to replace"), "{err}");
        assert_eq!(std::fs::read_to_string(&farm).unwrap(), "not a link");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A PROCESS DEATH IN PHASE 1 CONVERGES FORWARD.
    ///
    /// `cargo ship provision` writes the roster pair TWICE: phase 1 seeds `dist/` from
    /// the kept copy or the channel, and the in-process mint re-signs the same two files
    /// at the end of the same run. Protecting only the mint therefore left the window
    /// open in the write that happens FIRST — a death between phase 1's two renames left
    /// seq N+1's document beside seq N's signature, with no transaction for any later run
    /// to complete, on a gitignored file `git checkout` cannot restore and every operator
    /// reads as a mistyped phrase.
    ///
    /// The crash below is the REAL commit path, stopped: `commit_roster_pair` is what
    /// `write_pair` calls, and dropping its handle instead of completing it is what a
    /// killed process leaves, byte for byte. Writing one canonical half by hand is the
    /// rename that landed before the death.
    ///
    /// MUTATION: drop `lock_roster_pair` from phase 1, or give `write_pair` back a bare
    /// stage-both-then-rename-both, and the torn pair below is never repaired — every
    /// later run hard-stops in `read_local_candidate` instead.
    #[test]
    fn a_death_between_the_phase_one_pair_renames_recovers_forward() {
        let dir =
            std::env::temp_dir().join(format!("provision-phase1-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dist = dir.join("dist").join("aterm-machines.toml");
        let sig_path = machines::RosterDocument::signature_path(&dist);

        // Phase 1's ordinary write, under the phase's own lock.
        let (master, six, six_sig) = signed_candidate(6);
        let chosen = admit_candidate(&[&master], six.clone(), six_sig.clone()).unwrap();
        let lock = lock_roster_pair(&dist).expect("the phase's writer lock");
        install_pair(&lock, &dist, &chosen).expect("phase 1 installs the chosen pair");
        assert_eq!(std::fs::read(&dist).unwrap(), six);
        assert_eq!(std::fs::read(&sig_path).unwrap(), six_sig);

        // THE CRASH: seq 7 committed durably and whole, then the process dies with one
        // canonical rename done and the other not.
        let (_, seven, seven_sig) = signed_candidate(7);
        let premise = atpkg_keys::provision::RosterSnapshot {
            raw: six.clone(),
            sig: six_sig.clone(),
        };
        let committed = atpkg_keys::provision::commit_roster_pair(
            &lock,
            dist.to_str().unwrap(),
            Some(&premise),
            &seven,
            &seven_sig,
        )
        .expect("the redo transaction commits");
        let transaction = committed.transaction_path().to_string();
        drop(committed);
        std::fs::write(&dist, &seven).unwrap();
        drop(lock);

        // The state that used to be permanent: both halves present, from two different
        // generations, verifying as nothing.
        let torn = machines::RosterDocument::read(&dist).expect("both halves are present");
        assert_eq!(torn.bytes, seven);
        assert_eq!(torn.signature, six_sig);
        assert!(
            admit_candidate(&[&master], torn.bytes, torn.signature).is_err(),
            "a torn pair is exactly what no client — and no operator — can make sense of"
        );
        assert!(
            std::path::Path::new(&transaction).exists(),
            "the redo log committed before either rename, so it survived the death"
        );

        // The next run's phase 1 takes the same lock, and taking it IS the recovery: the
        // exact committed pair is installed before anything is allowed to read it.
        let lock = lock_roster_pair(&dist).expect("the phase-1 lock replays the committed redo");
        assert_eq!(std::fs::read(&dist).unwrap(), seven);
        assert_eq!(std::fs::read(&sig_path).unwrap(), seven_sig);
        let recovered = machines::RosterDocument::read(&dist).expect("a whole pair");
        assert_eq!(
            admit_candidate(&[&master], recovered.bytes, recovered.signature)
                .expect("the recovered pair verifies")
                .roster
                .roster_seq,
            7,
            "forward, not sideways: the generation the dead run signed"
        );
        assert!(
            !std::path::Path::new(&transaction).exists(),
            "a completed transaction is retired, so it cannot replay a second time"
        );

        // NEGATIVE CONTROL: the phase writes normally over the recovered pair, and leaves
        // no transaction behind when nothing goes wrong.
        let (_, eight, eight_sig) = signed_candidate(8);
        write_pair(&lock, &dist, &eight, &eight_sig).expect("phase 1 writes on");
        assert_eq!(std::fs::read(&dist).unwrap(), eight);
        assert_eq!(std::fs::read(&sig_path).unwrap(), eight_sig);
        assert!(!std::path::Path::new(&transaction).exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_pair_writes_the_pair_and_a_rerun_admits_it() {
        let dir = std::env::temp_dir().join(format!("provision-pair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("aterm-machines.toml");
        let (master, bytes, sig) = signed_candidate(6);
        let c = admit_candidate(&[&master], bytes, sig).unwrap();
        let lock = lock_roster_pair(&path).expect("the pair's writer lock");
        install_pair(&lock, &path, &c).expect("install");
        let doc = machines::RosterDocument::read(&path).expect("pair on disk");
        let again = admit_candidate(&[&master], doc.bytes, doc.signature).expect("re-admits");
        assert_eq!(again.roster.roster_seq, 6);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
