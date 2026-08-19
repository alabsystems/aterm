// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo ship provision --id <machine-id>` — a checkout becomes a PUBLISHING machine,
//! with the paper phrase as the only human input.
//!
//! "Publishing" is the load-bearing word: a machine on the roster builds what it signs,
//! so the audit proves the whole stack — the self-hosted Trust stage2 toolchain (a real
//! smoke-compile under the native-lane rustflags, via the same `gates` probes the cut
//! runs), the trust-named gate drivers (`targo`/`tippy`/`ty`/`trustdoc`), the rustup
//! front door (`cargo` in this repo dispatches into the linked `trust` toolchain — the
//! link is a provisioned artifact, not an accident), the stable `x86_64-apple-darwin`
//! slice of the universal binary, Apple's packaging tools, the Developer ID identity, a
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
    let mode = if check_only { " --check (no writes)" } else { "" };
    println!("aterm-release · provision {id} (channel {slug}){mode}");
    // No table of contents above the phases. The five names it listed are the five phase
    // headers printed below it, one at a time, each already carrying `[n/5]` — so it was
    // the plan said twice, at the top, where the operator is scanning for the first real
    // line.
    phase(1, "roster", "the master-signed list of machines allowed to publish");

    // ---- 1. the roster pair: newest verified generation into dist/ ----------------
    let home = std::env::var("HOME").map_err(|_| Error::new("HOME is not set"))?;
    let roster_path = repo.join("dist").join(roster::ROSTER_ASSET);
    let kept_path = kept_roster_path(&home);
    // dist/ is gitignored and `git clean -xdf` sweeps it — but the generation a mint
    // writes is re-signed LOCALLY and is published only by a later cut, so between the
    // two it can exist nowhere else on earth. Losing it leaves a machine whose key no
    // roster names, and the only remedy this tool could offer was "restore from the
    // machine holding the newest generation", which names no machine on a one- or
    // two-machine fleet. So a proven copy is kept beside the key it authorizes (below),
    // and dist/ is restored from it here — re-verified under the paper master by
    // `read_local_candidate`, exactly like any other pair. A restore is a WRITE into
    // dist/, so `--check` declines it for the same reason it declines `install_pair`.
    if !check_only && restore_kept_pair(&kept_path, &roster_path)? {
        step(
            "roster",
            &format!("dist/ was empty — restored this machine's copy from {}", kept_path.display()),
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
    if install && !check_only {
        install_pair(&roster_path, &chosen)?;
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

    // ---- what this machine already is ---------------------------------------------
    // No phase header, and no line on success. Every failure here is a hard `Err` carrying
    // its own message, and the one green line this section printed — "already provisioned
    // as 'm2' — key read, pubkey matches machine.toml" — is a weaker restatement of the
    // `authority` line, which proves the same key through the cut's own gate. On an
    // unprovisioned machine, the case the verb exists for, the section was a numbered
    // header with nothing under it.
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
    phase(2, "build stack", "the toolchain and SDK a cut compiles with");
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
    record("x86 slice", x86_slice_check(), &mut checks);
    record("apple sdk", apple_clt_check(), &mut checks);

    phase(3, "Apple certificate", "this machine's own Developer ID identity");
    // The SHA-1 the profile pins comes back BESIDE the check, not out of it. It used to be
    // formatted into an English sentence and then scraped back out with a 40-hex regex over
    // the printed line — and on a machine with two certificates that regex pinned whichever
    // appeared first in the prose, while the sentence claimed the profile disambiguated.
    let (apple_check, apple_sha1) = apple_identity_check(id, !check_only);
    record(crate::apple::APPLE_LABEL, apple_check, &mut checks);

    phase(4, "notary", "the credential Apple's notarization service answers to");
    record("notary", notary_acquire(!check_only), &mut checks);

    phase(5, "credentials", "the tokens and profile a cut is handed");
    record("github", gh_check(), &mut checks);
    record("channel", channel_token_check(&slug, !check_only), &mut checks);

    // The profile is deliberately NOT audited here, and not yet written. It is the one
    // item provision PRODUCES, and it is produced from the key the mint below writes —
    // so counting it at this point is a deadlock rather than a gate: the absent profile
    // defers the mint, and the deferred mint is why the profile is absent. A fresh
    // machine could never finish, which is the whole claim of the verb. It is written
    // and reported once, after the mint, where its verdict is already true.
    let blocking = blockers(&checks);
    let host_cannot_cut = checks.iter().any(|(_, c)| matches!(c, Check::Skip(_)));

    // ---- 3. mint LAST -------------------------------------------------------------
    // A roster id is irreversible, so it is never consumed on a machine the audit just
    // proved cannot build or publish.
    //
    // A deferred mint prints NOTHING. It used to say "DEFERRED — minting '<id>' would
    // consume an irreversible roster id; fix the N item(s) above and re-run. Nothing was
    // written", four lines above a terminal error saying: fix N, re-run, nothing written.
    // The mint's absence is exactly what that error means, and the error is the line the
    // shell shows and the exit code carries.
    let minted = if already || check_only || blocking > 0 {
        false
    } else if let Some(why) = channel_unreadable.as_ref() {
        return Err(Error::new(format!(
            "refusing to mint a roster id while the public channel cannot be read ({why}): a \
             mint has to see the fleet's current roster generation, or two machines end up \
             minting the same one (a lineage fork). Retry when the channel answers"
        )));
    } else {
        mint(repo, id, &roster_path)?;
        true
    };

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
        record("profile", check, &mut checks);
    }
    let (fails, waiting) = tally(&checks);

    if !key_path.exists() {
        println!();
        if check_only {
            let open = gaps(fails, waiting);
            println!(
                "CHECK ONLY — unminted; {}",
                if open.is_empty() {
                    format!("a real `cargo ship provision --id {id}` would mint")
                } else {
                    format!("{open} before a real run mints")
                }
            );
            return Ok(());
        }
        // Mint deferred: there is no identity to bind, and the machine is not
        // provisioned — say so through the exit code too.
        return Err(Error::new(format!(
            "{} above — nothing was written and no roster id was spent; re-run `cargo ship \
             provision --id {id}` once fixed",
            gaps(fails, waiting)
        )));
    }

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
    step(
        "authority",
        &format!(
            "authorize_cut passes: '{}' signs under roster seq {}",
            attribution.machine_id, attribution.roster_seq,
        ),
    );

    // Keep this machine's own copy of the generation that just authorized it — written
    // only now, so what is kept is a pair `authorize_cut` accepted, never a guess. Step 1
    // restores dist/ from it, which is what makes a swept dist/ self-healing rather than
    // the one state this verb has no remedy for. Not under `--check`: it is a write.
    if !check_only {
        match keep_roster_pair(&roster_path, &kept_path) {
            Ok(true) => step(
                "roster",
                &format!("kept this generation at {} (dist/ is sweepable)", kept_path.display()),
            ),
            Ok(false) => {}
            Err(e) => step("roster", &format!("could not keep a copy: {e}")),
        }
    }

    println!();
    if fails + waiting > 0 {
        return Err(Error::new(format!(
            "{} above — re-run `cargo ship provision --id {id}` once fixed",
            gaps(fails, waiting)
        )));
    }
    if host_cannot_cut {
        // Non-macOS: the roster half is proven, the Apple half cannot exist here.
        println!(
            "ROSTERED — but cuts run on macOS (Tier APPLE): this machine can sign the \
             atpkg index, and can never say READY TO CUT"
        );
    } else {
        // One line, and the command as printed RUNS: the old text spelled the profile
        // `<profile>` even though the audit had just printed its real path two lines up,
        // so the one thing the operator came here to copy had to be assembled by hand.
        // The parenthetical went with it — it explained a flag and then said the cut
        // names that flag itself.
        println!(
            "READY TO CUT — next: cargo ship cut --dry-run --release-credentials {}",
            Path::new(&home).join(".aterm/release-credentials.toml").display()
        );
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
fn mint(repo: &Path, id: &str, roster_path: &Path) -> Result<()> {
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
    let pre = prov::preflight(prov::Verb::Join, id, prov::DEFAULT_HEAD_ID, &paths)
        .map_err(Error::new)?;
    let phrase = master::prompt_for_master(
        "master phrase (52 characters, echo off; spaces, case, and o/i/l are forgiven): ",
    )
    .map_err(Error::new)?;
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
    let report = prov::write_rest(planned).map_err(Error::new)?;
    for line in prov::render_report(&report) {
        println!("{line}");
    }
    Ok(())
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
    let verified = roster::verify_roster(master_pubkeys, bytes.clone(), &sig)
        .map_err(|e| format!("the roster did not verify under the committed paper master ({e:?})"))?;
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
    let c = admit_candidate(pins::PAPER_MASTER_PUBKEYS, doc.bytes, doc.signature)
        .map_err(|e| {
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
    let bytes = curl_fetch(&release_asset_url(slug, roster::ROSTER_ASSET), 65_536).map_err(|e| {
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

/// Write the chosen pair into `dist/`: BOTH halves staged and fsynced first, then the
/// signature promoted, then the body — so at every instant the on-disk pair is either
/// the old document, unverifiable (torn, self-healing: the next run hard-stops on it
/// and the operator re-copies or removes), or the new document. Never a silently
/// half-new pair.
#[cfg(unix)]
fn install_pair(roster_path: &Path, c: &Candidate) -> Result<()> {
    write_pair(roster_path, &c.bytes, &c.sig)
}

/// The write itself, over bytes that some caller has already proven — `install_pair` for
/// an admitted candidate, [`keep_roster_pair`] for a pair `authorize_cut` just accepted.
#[cfg(unix)]
fn write_pair(roster_path: &Path, bytes: &[u8], sig: &[u8]) -> Result<()> {
    if let Some(dir) = roster_path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::new(format!("create {}: {e}", dir.display())))?;
    }
    let sig_path = machines::RosterDocument::signature_path(roster_path);
    let staged_body = stage(roster_path, bytes)?;
    let staged_sig = stage(&sig_path, sig)?;
    promote(&staged_sig, &sig_path)?;
    promote(&staged_body, roster_path)?;
    Ok(())
}

/// This machine's durable copy of the roster generation its key is named in — beside the
/// key itself, in the directory that already holds everything this machine minted for
/// itself, and outside any checkout a `git clean` can sweep.
#[cfg(unix)]
fn kept_roster_path(home: &str) -> PathBuf {
    Path::new(home).join(".aterm").join("roster").join(roster::ROSTER_ASSET)
}

/// Copy the pair `authorize_cut` just accepted into `~/.aterm/roster`. `Ok(true)` when a
/// copy was written, `Ok(false)` when the kept pair is already byte-identical — so a
/// provisioned machine says this once, not on every audit.
#[cfg(unix)]
fn keep_roster_pair(roster_path: &Path, kept_path: &Path) -> Result<bool> {
    let doc = machines::RosterDocument::read(roster_path)?;
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
    write_pair(kept_path, &doc.bytes, &doc.signature)?;
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
fn restore_kept_pair(kept_path: &Path, roster_path: &Path) -> Result<bool> {
    if roster_path.exists() || machines::RosterDocument::signature_path(roster_path).exists() {
        return Ok(false);
    }
    let Ok(Some(kept)) = read_local_candidate(kept_path) else {
        return Ok(false);
    };
    install_pair(roster_path, &kept)?;
    Ok(true)
}

/// Write `bytes` to a staged sibling of `path` and fsync it.
#[cfg(unix)]
fn stage(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    use std::io::Write as _;
    let mut staged = path.as_os_str().to_owned();
    staged.push(".provision.tmp");
    let staged = PathBuf::from(staged);
    let mut f = std::fs::File::create(&staged)
        .map_err(|e| Error::new(format!("create {}: {e}", staged.display())))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| Error::new(format!("write {}: {e}", staged.display())))?;
    Ok(staged)
}

#[cfg(unix)]
fn promote(staged: &Path, path: &Path) -> Result<()> {
    std::fs::rename(staged, path)
        .map_err(|e| Error::new(format!("rename {} into place: {e}", staged.display())))
}

/// One audit line: proven, missing-with-remedy, or impossible on this host. A `Skip`
/// is NOT a pass — a host that skips Apple checks can never say READY TO CUT.
#[cfg(unix)]
enum Check {
    Pass(String),
    Fail { what: String, fix: String },
    /// Progress, waiting on the operator — the certificate request is at Apple. Distinct
    /// from `Fail` because "MISSING" reads as a fault to repair and this is a step to
    /// take, but it counts against READY TO CUT all the same, and it defers the mint for
    /// the same reason a Fail does: a roster id is irreversible.
    Todo { what: String, next: String },
    Skip(String),
}

#[cfg(unix)]
fn print_check(label: &str, c: &Check) {
    match c {
        Check::Pass(msg) => step(label, msg),
        Check::Skip(msg) => step(label, &format!("impossible here — {msg}")),
        Check::Fail { what, fix } => {
            step(label, &format!("MISSING — {what}"));
            step("", &format!("fix: {fix}"));
        }
        Check::Todo { what, next } => {
            step(label, what);
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
        // `trustc_probe`'s own error already lists every way to get a toolchain
        // (gates.rs:64). Repeating them here printed the same three remedies twice in
        // two consecutive lines, which is how a fixable problem starts looking like two.
        Err(e) => (
            Check::Fail {
                what: e.to_string(),
                fix: "tools/bootstrap-publisher.sh does it for you".into(),
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
        Check::Pass(format!("targo + tippy + ty + trustdoc present in {}", bin.display()))
    } else {
        // Not `bootstrap-publisher.sh`: it resolves an existing stage2 and stops, so it
        // would report success over exactly this gap. A stage2 missing its tools has to
        // be replaced or rebuilt.
        Check::Fail {
            what: format!("{} missing from {}", missing.join(" + "), bin.display()),
            fix: "`atpkg install trust`, or rebuild with `[build] tools` carrying clean \
                  + ty"
                .into(),
        }
    }
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
        Ok(()) => Check::Pass("stable x86_64-apple-darwin target installed (universal slice)".into()),
        // Only the first LINE of the probe's error. It ends with its own two-line
        // `fix:`/`or:` remedy, and `step` prints one line — so those arrived unlabelled and
        // mis-indented, breaking the two-column layout, directly above a `fix:` saying the
        // same two things again. One missing rustup target printed four lines and two
        // remedy markers.
        Err(e) => Check::Fail {
            what: e
                .to_string()
                .lines()
                .next()
                .unwrap_or("no x86_64-apple-darwin target")
                .trim()
                .to_string(),
            fix: "`rustup +stable target add x86_64-apple-darwin` — or cut with \
                  --arm64-only (an explicit, thinner artifact)"
                .into(),
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
                    )
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
    machines::authorize_cut(pins::PAPER_MASTER_PUBKEYS, bytes, sig, pubkey, now_unix()? as i64)
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
        Ok(out) if out.status.success() => {
            Check::Pass(format!("notarytool profile '{profile}' answers (live-checked)"))
        }
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
        Err(e) => return Check::Fail { what: e, fix: rewrite },
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
            what: format!("{} declares machine_id = \"{declared}\", not '{id}'", path.display()),
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
        _ => Check::Pass(format!("{} loads under the cut's own rules", path.display())),
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
    let fix = format!(
        "mint a fine-grained PAT (Contents: read/write on {slug}, short expiry) and \
         write it to {where_it_lives}, mode 600"
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
                return Check::Pass("channel token present, owner-only".into());
            }
            let tightened = may_change
                && path.is_some_and(|p| {
                    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).is_ok()
                });
            if tightened {
                Check::Pass("channel token present (tightened to 0600)".into())
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
    use base64::Engine as _;

    /// A synthetic paper phrase, built at runtime so no phrase-shaped literal exists in
    /// this file (grep_guard B7 scans it).
    fn seed() -> atpkg_keys::master::MasterSeed {
        let phrase = "2".repeat(52);
        atpkg_keys::master::parse_master(&phrase)
            .expect("synthetic phrase")
            .seed()
    }

    fn pubkey_of(byte: u8) -> String {
        base64::engine::general_purpose::STANDARD.encode([byte; 32])
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
        let (chosen, install, how) =
            choose_candidate(path, "o/r", Some(local), Ok(older)).unwrap();
        assert_eq!(chosen.roster.roster_seq, 9);
        assert!(!install);
        assert!(how.contains("AHEAD"), "{how}");

        // equal AND byte-identical → keep local
        let (_, b, s) = signed_candidate(4);
        let local = admit_candidate(&[&master], b.clone(), s.clone()).unwrap();
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, how) =
            choose_candidate(path, "o/r", Some(local), Ok(fetched)).unwrap();
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
    /// this machine actually holds.
    fn signing_key_b64() -> (String, String) {
        use ring::signature::KeyPair as _;
        let rng = ring::rand::SystemRandom::new();
        let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keypair");
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).expect("keypair");
        let b64 = base64::engine::general_purpose::STANDARD;
        (b64.encode(doc.as_ref()), b64.encode(pair.public_key().as_ref()))
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
            &format!("{good}machine_roster = \"{}\"\n", home.join("gone/roster.toml").display()),
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
        let fail = || Check::Fail { what: String::new(), fix: String::new() };
        let todo = || Check::Todo { what: String::new(), next: String::new() };
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

        // Nothing kept, nothing local: a no-op, not an error.
        assert!(!restore_kept_pair(&kept, &dist).unwrap());

        let (master, bytes, sig) = signed_candidate(6);
        let c = admit_candidate(&[&master], bytes, sig).unwrap();
        install_pair(&dist, &c).unwrap();

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
        assert!(!restore_kept_pair(&kept, &dist).unwrap());

        // Swept. The kept copy is re-admitted under `PAPER_MASTER_PUBKEYS` before it is
        // written — this fixture is signed by a synthetic master, so it is IGNORED rather
        // than installed, and rather than turned into a hard error: the channel is still
        // a source, and a cache that cannot be proven is not one.
        std::fs::remove_file(&dist).unwrap();
        std::fs::remove_file(machines::RosterDocument::signature_path(&dist)).unwrap();
        assert!(!restore_kept_pair(&kept, &dist).unwrap());
        assert!(!dist.exists());

        // A torn kept copy is ignored for the same reason, and never propagated.
        std::fs::remove_file(machines::RosterDocument::signature_path(&kept)).unwrap();
        assert!(!restore_kept_pair(&kept, &dist).unwrap());
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
    fn install_pair_writes_the_pair_and_a_rerun_admits_it() {
        let dir = std::env::temp_dir().join(format!("provision-pair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("aterm-machines.toml");
        let (master, bytes, sig) = signed_candidate(6);
        let c = admit_candidate(&[&master], bytes, sig).unwrap();
        install_pair(&path, &c).expect("install");
        let doc = machines::RosterDocument::read(&path).expect("pair on disk");
        let again = admit_candidate(&[&master], doc.bytes, doc.signature).expect("re-admits");
        assert_eq!(again.roster.roster_seq, 6);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
