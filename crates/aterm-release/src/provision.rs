// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine provisioning for `targo --unverified ship provision --id <machine-id>`.

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
use crate::{channel, gates, machines, publish, sign};

#[cfg(unix)]
#[path = "provision/layer.rs"]
mod layer;

/// POSIX-only, exactly like the engine it drives.
#[cfg(not(unix))]
pub fn run_provision(
    _repo: &std::path::Path,
    _id: &str,
    _check_only: bool,
    _cert_dir: Option<&std::path::Path>,
) -> crate::ledger::Result<()> {
    Err(crate::ledger::Error::new(
        "provision is POSIX-only: the provisioning engine reads the master phrase from /dev/tty",
    ))
}

#[cfg(unix)]
pub fn run_provision(
    repo: &Path,
    id: &str,
    check_only: bool,
    cert_dir: Option<&Path>,
) -> Result<()> {
    atpkg_keys::provision::vet_machine_id(id).map_err(Error::new)?;

    let manifest = std::fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read {}/Cargo.toml: {e}", repo.display())))?;
    let slug = channel::update_channel_slug(&manifest)?.ok_or_else(|| {
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
    phase(
        1,
        "roster",
        "the master-signed list of machines allowed to publish",
    );

    let home = std::env::var("HOME").map_err(|_| Error::new("HOME is not set"))?;
    if !Path::new(&home).is_absolute() {
        return Err(Error::new(format!(
            "HOME is not an absolute path ({home:?}) — refusing to derive the kept roster \
             pair, machine key, identity, and farm-link paths from it"
        )));
    }
    let roster_path = repo.join("dist").join(roster::ROSTER_ASSET);
    let kept_path = kept_roster_path(&home);
    let (chosen, channel_unreadable) = {
        let roster_lock = if check_only {
            atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(&roster_path)?)
                .map_err(Error::new)?;
            None
        } else {
            Some(lock_roster_pair(&roster_path)?)
        };
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
        let channel_unreadable = match &fetched {
            Err(e) if !e.starts_with(NO_CHANNEL_ROSTER) => Some(e.clone()),
            _ => None,
        };
        if let Some(why) = channel_unreadable.as_ref() {
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
        step(
            "roster",
            &if install && check_only {
                format!("{how} — not written (--check)")
            } else {
                how
            },
        );
        if seeded_from_channel {
            step(
                "",
                "note: the channel cannot show an incumbent's UNPUBLISHED roster edit — if \
                 one exists, STOP and copy that machine's dist/ pair here instead",
            );
        }
        (chosen, channel_unreadable)
    };

    let key_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_KEY_REL);
    let identity_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_PUB_REL);

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
                "this machine is already provisioned as '{}' — run `{SHIP} provision \
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
        attributed = Some(authorize(chosen.bytes.clone(), &chosen.sig, derived)?);
        true
    } else {
        false
    };

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
    if !check_only {
        match aterm_verify::provision::this_machine(repo) {
            Some(machine) => {
                aterm_verify::provision::run(
                    &aterm_verify::provision::Options::default(),
                    &machine,
                    &aterm_verify::provision::System::new(repo),
                    &mut |line| step("provision", line),
                );
            }
            None => step(
                "provision",
                "COULD NOT RUN — no $HOME names this user's cache",
            ),
        }
        seam(Path::new(&home), &mut |line| step("acquire", line));
    }
    let (stack, stage2_bin) = toolchain_check(repo);
    record("toolchain", stack, &mut checks);
    if let Some(bin) = &stage2_bin {
        record("verifiers", verifiers_check(bin), &mut checks);
        record("front door", front_door_check(bin), &mut checks);
    }
    record("doc tool", doc_tool_check(&home, check_only), &mut checks);
    record("rust shims", rust_shim_check(), &mut checks);
    record("x86 slice", x86_slice_check(), &mut checks);
    record("apple sdk", apple_clt_check(), &mut checks);
    let host: std::sync::Arc<dyn layer::ReleaseHost> =
        std::sync::Arc::new(layer::System::new(repo, Path::new(&home)));
    for (label, check) in layer::audit(&host) {
        record(label, check, &mut checks);
    }
    if !check_only && let Some(line) = layer::settle_site_key(&host) {
        step("release.site-key", &line);
    }

    phase(
        3,
        "Apple certificate",
        "this machine's own Developer ID identity",
    );
    let (apple_check, apple_sha1) =
        apple_identity_check(id, !check_only, leave(check_only), cert_dir);
    record(crate::apple::APPLE_LABEL, apple_check, &mut checks);
    record(
        KEYCHAIN_LABEL,
        keychain_check(id, leave(check_only)),
        &mut checks,
    );

    phase(
        4,
        "notary",
        "the credential Apple's notarization service answers to",
    );
    record("notary", notary_acquire(!check_only), &mut checks);

    phase(5, "credentials", "the tokens and profile a cut is handed");
    record(
        "channel",
        channel_token_check(&slug, !check_only),
        &mut checks,
    );

    let blocking = blockers(&checks);
    let host_cannot_cut = checks.iter().any(|(_, c)| matches!(c, Check::Skip(_)));

    let report = if already || check_only || blocking > 0 {
        None
    } else if let Some(why) = channel_unreadable.as_ref() {
        return Err(Error::new(format!(
            "refusing to mint a roster id while the public channel cannot be read ({why}): a \
             mint has to see the fleet's current roster generation, or two machines end up \
             minting the same one (a lineage fork). Retry when the channel answers"
        )));
    } else {
        band("MINT");
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

    let derived_pubkey = if minted {
        sign::ReleaseCredentials::resolve(None, repo)?.map(|c| c.pubkey().to_string())
    } else {
        resolved.as_ref().map(|c| c.pubkey().to_string())
    };

    if key_path.exists() {
        let write_failed = if apple_sha1.is_some() && !check_only {
            crate::apple::write_credentials_profile(id, &roster_path, apple_sha1.as_deref()).err()
        } else {
            None
        };
        let check = match write_failed {
            Some(e) => Check::Fail {
                what: e,
                fix: "check ownership of ~/.aterm/release-ring, then re-run".into(),
            },
            None => profile_check(
                &home,
                id,
                apple_sha1.as_deref(),
                derived_pubkey.as_deref(),
                &crate::dev_identity::Bounded,
                leave(check_only),
            ),
        };
        checks.push(("profile", check));
    }
    let (fails, waiting) = tally(&checks);

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
        open: open_labels(&checks),
        authority,
        roster_kept,
        key_exists: key_path.exists(),
        fails,
        waiting,
        host_cannot_cut,
    })
}

/// The label of every open item.
#[cfg(unix)]
fn open_labels(checks: &[(&'static str, Check)]) -> Vec<String> {
    checks
        .iter()
        .filter_map(|(l, c)| match c {
            Check::Fail { .. } | Check::Owner { .. } => Some(format!("{l} (gap)")),
            Check::Todo { .. } => Some(format!("{l} (waiting)")),
            Check::CouldNotRun(_) => Some(format!("{l} (could not run)")),
            _ => None,
        })
        .collect()
}

/// rustup's `trust` entry through atpkg's own seam pass (`aterm pkg repair`).
#[cfg(unix)]
fn seam(home: &Path, report: &mut dyn FnMut(&str)) {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let layout = atpkg::Layout {
        prefix: atpkg::toolchain::atpkg_prefix(home, xdg.as_deref()),
    };
    let rustup_home =
        atpkg::toolchain::rustup_home(std::env::var_os("RUSTUP_HOME").as_deref(), home);
    for event in atpkg::seam::repair(&layout, &rustup_home) {
        report(&format!("rustup trust: {event}"));
    }
}

/// Everything the closing structure needs.
#[cfg(unix)]
struct Closing<'a> {
    id: &'a str,
    home: &'a str,
    check_only: bool,
    /// The mint's own report, when this run minted.
    report: Option<&'a atpkg_keys::provision::Report>,
    profile: Option<&'a Check>,
    authority: Option<String>,
    roster_kept: Option<String>,
    key_exists: bool,
    fails: usize,
    waiting: usize,
    /// The label of every open item, with the word it is counted under.
    open: Vec<String>,
    host_cannot_cut: bool,
}

/// The mint's DONE facts, re-rendered on THIS transcript's grid as `(label, value)`.
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
    out.push((
        "authority".to_string(),
        format!("the master-signed roster authorizes '{}'", r.id),
    ));
    out
}

/// The mint's NEXT steps before a cut.
#[cfg(unix)]
pub(crate) fn report_next_before_cut(r: &atpkg_keys::provision::Report) -> Vec<String> {
    use atpkg_keys::provision::Verb;
    let mut out = Vec::new();
    if r.pins_changed {
        out.push(format!("review: git diff -- {}", r.paths.pins));
    }
    if r.verb == Verb::Setup {
        out.push("delete any test that asserts the anchor is empty".to_string());
    }
    if r.pins_changed {
        out.push("commit — durable from here".to_string());
    }
    out
}

/// The mint's NEXT step that follows the dry run.
#[cfg(unix)]
pub(crate) fn report_next_after_cut(r: &atpkg_keys::provision::Report) -> Vec<String> {
    vec![format!(
        "copy {} + .sig to every other publishing machine — a cut from an older roster is \
         refused",
        r.paths.roster
    )]
}

/// The cutter's command in every remedy this audit prints.
pub(crate) const SHIP: &str = crate::publish::SHIP_COMMAND;

/// The line an operator copies off a READY TO CUT transcript.
fn cut_dry_run_next(profile_path: &Path) -> String {
    format!(
        "{} --dry-run --bump --mac-only --release-credentials {}",
        crate::release::RELEASE_COMMAND,
        profile_path.display()
    )
}

/// One band header.
#[cfg(unix)]
fn band(name: &str) {
    println!();
    println!("  === {name} ===");
}

/// The closing structure, and the ONLY place this verb ends.
#[cfg(unix)]
fn close(c: Closing<'_>) -> Result<()> {
    let done = match (c.check_only, c.report.map(|r| r.pins_changed)) {
        (true, _) => "DONE (--check: nothing written)".to_string(),
        (false, Some(true)) => "DONE (working tree only — a commit makes it durable)".to_string(),
        _ => "DONE".to_string(),
    };
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

    let mut next: Vec<String> = Vec::new();
    if let Some(r) = c.report {
        next.extend(report_next_before_cut(r));
    }
    let profile_path = Path::new(c.home).join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL);
    if c.key_exists && !c.host_cannot_cut && c.fails + c.waiting == 0 {
        next.push(cut_dry_run_next(&profile_path));
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

    println!();
    if !c.key_exists {
        if c.check_only {
            let open = gaps(c.fails, c.waiting);
            if open.is_empty() {
                println!(
                    "CHECK ONLY — unminted; a real `{SHIP} provision --id {}` would mint",
                    c.id
                );
                return Ok(());
            }
            return Err(Error::new(format!(
                "CHECK ONLY — unminted; nothing was written. {open} before a real run mints: {}.",
                c.open.join(", ")
            )));
        }
        return Err(Error::new(format!(
            "NOT DONE — {}: {}.\nNothing was written and no roster id was spent. Re-run \
             `{SHIP} provision --id {}` once those are settled — it resumes exactly \
             there.",
            gaps(c.fails, c.waiting),
            c.open.join(", "),
            c.id
        )));
    }
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
            "NOT DONE — {}: {}.\nRe-run `{SHIP} provision --id {}` once those are \
             settled — it resumes exactly there.",
            gaps(c.fails, c.waiting),
            c.open.join(", "),
            c.id
        )));
    }
    if c.host_cannot_cut {
        println!(
            "ROSTERED — but cuts run on macOS (Tier APPLE): this machine can sign the atpkg \
             index, and can never say READY TO CUT"
        );
    } else {
        println!("READY TO CUT");
    }
    Ok(())
}

/// The in-process join ceremony.
#[cfg(unix)]
fn mint(repo: &Path, id: &str, roster_path: &Path) -> Result<atpkg_keys::provision::Report> {
    use atpkg_keys::{master, provision as prov};

    master::forbid_core_dumps();

    let paths = prov::Paths {
        pins: utf8_path(&repo.join(prov::PINS_REL))?,
        roster: utf8_path(roster_path)?,
        key: prov::home_path(prov::MACHINE_KEY_REL).map_err(Error::new)?,
        machine_pub: prov::home_path(prov::MACHINE_PUB_REL).map_err(Error::new)?,
        pins_explicit: false,
    };
    let pre = prov::preflight(prov::Verb::Join, id, &paths).map_err(Error::new)?;
    let phrase = prompt_master_with_retries()?;
    let seed = phrase.seed();
    prov::verify_master(&pre, &seed).map_err(|e| {
        Error::new(match seed.fingerprint() {
            Ok(fp) => format!("the phrase you typed has fingerprint {fp}\n{e}"),
            Err(_) => e,
        })
    })?;
    let planned = prov::plan(pre, &seed, now_unix()?).map_err(Error::new)?;
    prov::write_pins(&planned).map_err(Error::new)?;
    prov::write_rest(planned).map_err(Error::new)
}

/// Ask for the paper master on the transcript's grid, and give a mistype another go.
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
    let _ = std::io::Write::flush(&mut std::io::stdout());
    for left in (0..TRIES).rev() {
        match master::prompt_master_attempt(&prompt).map_err(Error::new)? {
            Ok(phrase) => return Ok(phrase),
            Err(typo) if left > 0 => {
                step("master", &typo.message());
                step("", &format!("nothing written — try again ({left} left)"));
            }
            Err(typo) => {
                return Err(Error::new(format!(
                    "{} — {TRIES} attempts, and nothing was written: no key, no roster, no \
                     roster id spent. Re-run `{SHIP} provision` and it resumes exactly \
                     here.",
                    typo.message()
                )));
            }
        }
    }
    unreachable!("the loop returns on every arm")
}

/// A roster pair that verified under the committed paper master and parsed.
#[cfg(unix)]
#[derive(Debug)]
struct Candidate {
    bytes: Vec<u8>,
    sig: Vec<u8>,
    roster: Roster,
}

/// Verify-then-parse.
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

/// A local pair admitted under the release's paper-master anchors.
#[cfg(unix)]
fn read_local_candidate(roster_path: &Path) -> Result<Option<Candidate>> {
    read_local_candidate_with(roster_path, pins::PAPER_MASTER_PUBKEYS)
}

/// Read a local pair under the supplied anchors, without changing either half.
#[cfg(unix)]
fn read_local_candidate_with(
    roster_path: &Path,
    master_pubkeys: &[&str],
) -> Result<Option<Candidate>> {
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
    let c = admit_candidate(master_pubkeys, doc.bytes, doc.signature).map_err(|e| {
        Error::new(format!(
            "the roster pair at {} is unusable: {e}. Refusing to overwrite local \
                 roster state this tool cannot prove — if it was a hand-copy, re-copy \
                 BOTH files from the source machine; remove both files to accept the \
                 channel release's pair instead",
            roster_path.display(),
        ))
    })?;
    Ok(Some(c))
}

/// Audit every supplied local pair, keeping the newest admitted generation. A damaged
/// pair or two different documents at one generation refuses instead of falling back.
#[cfg(unix)]
fn newest_local_candidate(paths: &[&Path], master_pubkeys: &[&str]) -> Result<Option<Candidate>> {
    let mut selected: Option<(&Path, Candidate)> = None;
    for &path in paths {
        atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(path)?)
            .map_err(Error::new)?;
        let Some(candidate) = read_local_candidate_with(path, master_pubkeys)? else {
            continue;
        };
        if let Some((previous_path, previous)) = &selected {
            if previous.roster.roster_seq == candidate.roster.roster_seq
                && previous.bytes != candidate.bytes
            {
                return Err(Error::new(format!(
                    "LINEAGE FORK: local roster pairs {} and {} both carry roster_seq {} \
                     but their bytes differ — resolve the conflicting master-signed \
                     documents before releasing",
                    previous_path.display(),
                    path.display(),
                    candidate.roster.roster_seq,
                )));
            }
            if previous.roster.roster_seq >= candidate.roster.roster_seq {
                continue;
            }
        }
        selected = Some((path, candidate));
    }
    Ok(selected.map(|(_, candidate)| candidate))
}

/// The latest channel release's pair, fetched anonymously and admitted.
#[cfg(unix)]
fn fetch_channel_candidate(slug: &str) -> std::result::Result<Candidate, String> {
    let bytes =
        curl_fetch(&release_asset_url(slug, roster::ROSTER_ASSET), 65_536).map_err(|e| {
            // Only the body's clean 404 means no roster is published yet; any other
            // failure (429/403, a timeout, a private repo) is "cannot tell", and the mint
            // gate refuses on it.
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

/// Marker prefix when the channel roster body answered a clean 404.
#[cfg(unix)]
const NO_CHANNEL_ROSTER: &str = "no roster on the channel";

/// `https://github.com/<slug>/releases/latest/download/<asset>`.
#[cfg(unix)]
fn release_asset_url(slug: &str, asset: &str) -> String {
    format!("https://github.com/{slug}/releases/latest/download/{asset}")
}

/// One bounded anonymous download.
#[cfg(unix)]
fn curl_fetch(url: &str, cap: usize) -> std::result::Result<Vec<u8>, String> {
    let cap_s = cap.to_string();
    let out = Command::new(aterm_uds::os_path!("curl"))
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
    if out.stdout.len() > cap {
        return Err(format!("{url}: exceeded the {cap}-byte cap"));
    }
    if out.stdout.is_empty() {
        return Err(format!("{url}: zero bytes"));
    }
    Ok(out.stdout)
}

/// Newest generation wins between `dist/` and the channel.
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

/// Take the roster pair's writer lock.
#[cfg(unix)]
fn lock_roster_pair(roster_path: &Path) -> Result<atpkg_keys::provision::RosterLock> {
    atpkg_keys::provision::lock_roster(&utf8_path(roster_path)?).map_err(Error::new)
}

/// The pair a write is about to replace, read under its lock.
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

/// The write itself, over bytes a caller has already proven.
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

/// Publish a pair the caller has already proven, holding the writer lock for this one write.
#[cfg(unix)]
pub(crate) fn publish_proven_pair(roster_path: &Path, bytes: &[u8], sig: &[u8]) -> Result<()> {
    let lock = lock_roster_pair(roster_path)?;
    write_pair(&lock, roster_path, bytes, sig)
}

/// `atpkg-keys` is POSIX-only.
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

/// Durable copy of the roster generation under `~/.aterm/release-ring` beside the key.
#[cfg(unix)]
fn kept_roster_path(home: &str) -> PathBuf {
    Path::new(home).join(atpkg_keys::provision::RELEASE_ROSTER_REL)
}

/// Copy the pair `authorize_cut` just accepted into `~/.aterm/release-ring/roster`.
#[cfg(unix)]
fn keep_roster_pair(roster_path: &Path, kept_path: &Path) -> Result<bool> {
    let doc = machines::RosterDocument::read(roster_path)?;
    let kept_lock = lock_roster_pair(kept_path)?;
    if let Ok(kept) = machines::RosterDocument::read(kept_path) {
        if kept.bytes == doc.bytes && kept.signature == doc.signature {
            return Ok(false);
        }
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

/// Restore `dist/` from the kept copy.
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

/// One audit line: proven, missing-with-remedy, or impossible on this host.
#[cfg(unix)]
#[derive(Debug)]
enum Check {
    Pass(String),
    Fail {
        what: String,
        fix: String,
    },
    /// Waiting on the operator (the certificate request is at Apple).
    Todo {
        what: String,
        next: String,
    },
    /// A gap only the owner can close.
    Owner {
        what: String,
        fix: String,
    },
    /// The check could not be asked (Apple's service did not answer at all).
    CouldNotRun(String),
    Skip(String),
}

#[cfg(unix)]
/// One marker vocabulary, matching the words the summary counts in (`gaps()`: "gap", "waiting").
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
        Check::Owner { what, fix } => {
            step(label, &format!("GAP — {what} (owner)"));
            step("", &format!("fix: {fix}"));
        }
        Check::CouldNotRun(what) => {
            step(label, &format!("COULD NOT RUN — {what}"));
            step("", "next: run it again once it answers");
        }
    }
}

/// The Trust stage2 toolchain, proven by the probe a cut runs.
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
        Err(e) => (
            Check::Fail {
                what: e.to_string(),
                fix: format!(
                    "aterm pkg install trust, then re-run this verb\n\
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
        Check::Fail {
            what: format!("{} missing from {}", missing.join(" + "), bin.display()),
            fix: format!(
                "aterm pkg install trust   (then `aterm pkg doctor` to confirm the store)\n\
                 or, from source: set `tools` in the Trust checkout's bootstrap.toml to \
                 include every driver above, then rebuild {}",
                gates::TRUST_FROM_SOURCE
            ),
        }
    }
}

#[cfg(unix)]
/// The remedy for "the driver is there, its directory is not on PATH", shared by
/// [`doc_tool_check`]'s two arms.
fn path_persist_fix(dir: &Path) -> String {
    format!(
        "echo 'export PATH=\"{d}:$PATH\"' >> ~/.zshrc && exec zsh\n\
         it has to persist: a cargo run whose toolchain carries no trustdoc \
         resolves it from PATH, in whatever shell it runs in",
        d = dir.display()
    )
}

/// Check rustup proxies ([`crate::buildplan::release_rustup_shim_dir`]).
#[cfg(unix)]
fn rust_shim_check() -> Check {
    rust_shim_verdict(
        aterm_uds::host::tool("rustup").locate(),
        aterm_uds::host::tooling_dir().as_deref(),
    )
}

/// [`rust_shim_check`] on the `rustup` row's answer `rustup`, the row's copy looked for in
/// `tooling`. A directory that fails is fixed through the row's provider, never by hand: the
/// row's own copy is removed and placed again, and any other rustup (one on `$PATH`, such as
/// Homebrew's link) is outranked by the copy `verify provision` places.
#[cfg(unix)]
fn rust_shim_verdict(
    rustup: std::result::Result<PathBuf, aterm_uds::host::Absent>,
    tooling: Option<&Path>,
) -> Check {
    let rustup = match rustup {
        Ok(rustup) => rustup,
        Err(absent) => {
            return Check::Fail {
                what: absent.why,
                fix: absent.fix,
            };
        }
    };
    match crate::buildplan::release_rustup_shim_dir_of(&rustup) {
        Ok(dir) => Check::Pass(format!(
            "cargo and rustup are real files in {}",
            dir.display()
        )),
        Err(what) => {
            let row = aterm_uds::host::tool("rustup");
            let fix = match tooling.and_then(|t| row.pinned_root(t)) {
                Some(root) if rustup.starts_with(&root) => format!(
                    "remove {}, the row's copy, then {}, with cargo and rustc linked beside it",
                    root.display(),
                    row.remedy()
                ),
                _ => format!(
                    "{}, with cargo and rustc linked beside it, which the row answers before {} \
                     on $PATH",
                    row.remedy(),
                    rustup.display()
                ),
            };
            Check::Fail { what, fix }
        }
    }
}

/// The doc-tool farm link, `~/.local/bin/trustdoc` symlinked at the live stage2.
#[cfg(unix)]
fn doc_tool_check(home: &str, check_only: bool) -> Check {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    if let Some(found) = resolve_on_path("trustdoc", &path_var) {
        return Check::Pass(format!("trustdoc resolves on PATH ({})", found.display()));
    }
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
        return Check::Fail {
            what: format!("{} exists but is not executable", target.display()),
            fix: format!(
                "an incomplete stage2 — reinstall it (`aterm pkg install trust`, then \
                 `aterm pkg doctor`) or, from source, rebuild it ({})",
                gates::TRUST_FROM_SOURCE
            ),
        };
    }
    let fix = format!(
        "ln -sf {} {} — the farm link `[build] rustdoc = \"trustdoc\"` \
         (.cargo/config.toml) resolves; a full `{SHIP} provision` run repairs it",
        target.display(),
        farm.display(),
    );
    if check_only {
        return Check::Fail {
            what: "no `trustdoc` resolves on PATH — a cargo run whose toolchain carries \
                   no trustdoc dies at exec"
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

/// First executable `name` on `path`.
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

/// Create `farm` → `target`, repairing only what is safely mechanical.
#[cfg(unix)]
fn link_farm(farm: &Path, target: &Path) -> std::result::Result<(), String> {
    match std::fs::symlink_metadata(farm) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "cannot inspect {}: {e} — refusing to write what it cannot see",
                farm.display()
            ));
        }
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
        let _ = std::fs::remove_file(&staged);
        format!("rename {} into place: {e}", staged.display())
    })
}

/// The front door of every host lane.
#[cfg(unix)]
fn front_door_check(bin: &Path) -> Check {
    front_door_verdict(bin, |tool| {
        let out = Command::new(tool)
            .arg("--version")
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    })
}

/// The front-door verdict, pure over the `--version` probe so both outcomes are tests.
#[cfg(unix)]
fn front_door_verdict(
    bin: &Path,
    version: impl Fn(&Path) -> std::result::Result<String, String>,
) -> Check {
    let fix = format!(
        "aterm pkg install trust   (then `aterm pkg which targo` names the store's targo, \
         `aterm pkg doctor` the prefix)\n  or, from source: {}\n  or point TRUST_STAGE2_BIN \
         at a stage2 bin dir carrying both — this audit resolved {}",
        gates::TRUST_FROM_SOURCE,
        bin.display()
    );
    let mut targo_version = String::new();
    for tool in ["targo", "trustc"] {
        let path = bin.join(tool);
        if !path.is_file() {
            return Check::Fail {
                what: format!("{tool} is missing from {}", bin.display()),
                fix,
            };
        }
        match version(&path) {
            Ok(answer) => {
                if tool == "targo" {
                    targo_version = answer.lines().next().unwrap_or("").to_string();
                }
            }
            Err(why) => {
                return Check::Fail {
                    what: format!("{} exists but failed --version: {why}", path.display()),
                    fix,
                };
            }
        }
    }
    Check::Pass(format!(
        "targo + trustc answer --version in {} ({targo_version}) — `targo --unverified \
         ship …` needs no rustup",
        bin.display()
    ))
}

/// The x86_64 compat slice of the universal binary rides upstream stable.
/// STOCK EXCEPTION (Trust lacks an x86_64-apple-darwin std: `targo --unverified check
/// --target x86_64-apple-darwin -p aterm` fails E0463, can't find crate for `std`); this
/// row audits that stock lane, nothing else.
#[cfg(unix)]
fn x86_slice_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("universal DMG builds run on macOS".into());
    }
    match gates::x86_target_probe() {
        Ok(()) => match gates::rosetta_runs(&mut |command| command.output()) {
            Ok(()) => Check::Pass(
                "stable x86_64-apple-darwin target installed and Rosetta runs it (universal \
                 slice)"
                    .into(),
            ),
            Err(_) => Check::Fail {
                what: "Rosetta does not run x86_64 programs here — a universal cut runs its \
                       x86_64 slice under it before it ships"
                    .into(),
                fix: "softwareupdate --install-rosetta --agree-to-license".into(),
            },
        },
        Err(e) => Check::Fail {
            what: e.to_string(),
            fix: gates::X86_SLICE_REMEDIES.to_string(),
        },
    }
}

/// Apple's bundle/sign/package command-line tools.
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

/// This machine's own Developer ID identity, acquired rather than copied ([`crate::apple`]).
#[cfg(unix)]
fn apple_identity_check(
    id: &str,
    may_change: bool,
    leave: crate::release_keychain::Leave,
    cert_dir: Option<&Path>,
) -> (Check, Option<String>) {
    match crate::apple::acquire(id, may_change, leave, cert_dir) {
        crate::apple::Outcome::Ready { ids, note } => {
            let mut msg = match ids.len() {
                0 => {
                    return (
                        Check::Fail {
                            what: "no Developer ID Application identity after acquisition".into(),
                            fix: format!("re-run `{SHIP} provision --id {id}`"),
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

/// The label of the release keychain's line.
#[cfg(unix)]
const KEYCHAIN_LABEL: &str = "keychain";

/// What a run may do to the release keychain: `ship provision --check` and a dry
/// `ship release` write nothing; provision and a real release's audit may re-assert its
/// partition list, so their test signature runs.
#[cfg(unix)]
fn leave(look_only: bool) -> crate::release_keychain::Leave {
    if look_only {
        crate::release_keychain::Leave::Look
    } else {
        crate::release_keychain::Leave::Write
    }
}

/// aterm's own release keychain ([`crate::release_keychain`]): missing, locked, holding the
/// identity, its partition list set (a test signature passes; a look only opens it), and on
/// the search list. Each gap's fix runs provision, which needs no login password.
#[cfg(unix)]
fn keychain_check(id: &str, leave: crate::release_keychain::Leave) -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Check::CouldNotRun("HOME is not set".into());
    };
    keychain_check_in(Path::new(&home), id, &crate::dev_identity::Bounded, leave)
}

/// [`keychain_check`] for the user whose home is `home`, every tool call through `run`.
#[cfg(unix)]
fn keychain_check_in(
    home: &Path,
    id: &str,
    run: &dyn crate::dev_identity::Run,
    leave: crate::release_keychain::Leave,
) -> Check {
    let kc = crate::release_keychain::Keychains::new(home, run);
    match kc.row(id, aterm_update_core::pins::APPLE_TEAM_ID, &|sha1| {
        crate::apple::prove_can_sign(&kc, sha1, leave)
    }) {
        crate::release_keychain::Row::Ready(line) => Check::Pass(line),
        crate::release_keychain::Row::Todo { what, next } => Check::Todo { what, next },
        crate::release_keychain::Row::Blocked { what, fix } => Check::Fail { what, fix },
    }
}

/// Fail and Todo both block a cut.
#[cfg(unix)]
fn blockers(checks: &[(&'static str, Check)]) -> usize {
    let (fails, waiting) = tally(checks);
    fails + waiting
}

/// Faults and things merely waiting, counted apart.
#[cfg(unix)]
fn tally(checks: &[(&'static str, Check)]) -> (usize, usize) {
    let count = |f: fn(&Check) -> bool| checks.iter().filter(|(_, c)| f(c)).count();
    (
        count(|c| {
            matches!(
                c,
                Check::Fail { .. } | Check::Owner { .. } | Check::CouldNotRun(_)
            )
        }),
        count(|c| matches!(c, Check::Todo { .. })),
    )
}

/// "1 gap" · "2 gaps, 1 waiting" · "1 waiting".
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

/// `crate::apple::APPLE_LABEL`.
const APPLE_LABEL: &str = "apple id";

/// One provisioning finding, as `ship release` reads it ([`audit`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audited {
    /// Proven.
    Pass,
    /// Missing, with its fix.
    Gap {
        what: String,
        fix: String,
        owner: bool,
    },
    /// The check could not be asked.
    CouldNotRun(String),
}

/// One labelled line of [`audit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The line's label, as `provision --check` prints it (`notary`, `x86 slice`…).
    pub label: &'static str,
    pub state: Audited,
}

/// The machine audit `ship release` runs, using its explicitly selected profile when given.
#[cfg(unix)]
#[must_use]
pub fn audit(repo: &Path, id: &str, profile: Option<&Path>, dry: bool) -> Vec<Finding> {
    let mut found: Vec<(&'static str, Check)> = Vec::new();
    let Ok(home) = std::env::var("HOME") else {
        return vec![Finding {
            label: "roster",
            state: Audited::CouldNotRun("HOME is not set".into()),
        }];
    };
    if id.is_empty() {
        found.push((
            "roster",
            Check::Owner {
                what: "this machine names no roster id (no ~/.aterm/release-ring/machine.toml, \
                       no machine_id in its credentials profile) — the cut signs only as a \
                       rostered machine"
                    .into(),
                fix: format!("{SHIP} provision --id <its roster id> (docs/RELEASE-KEYS.md)"),
            },
        ));
    }
    let (stack, stage2_bin) = toolchain_check(repo);
    found.push(("toolchain", stack));
    if let Some(bin) = &stage2_bin {
        found.push(("verifiers", verifiers_check(bin)));
        found.push(("front door", front_door_check(bin)));
    }
    found.push(("doc tool", doc_tool_check(&home, true)));
    found.push(("rust shims", rust_shim_check()));
    found.push(("x86 slice", x86_slice_check()));
    found.push(("apple sdk", apple_clt_check()));
    let (apple, apple_sha1) = apple_identity_check(id, false, leave(dry), None);
    found.push((crate::apple::APPLE_LABEL, apple));
    found.push((KEYCHAIN_LABEL, keychain_check(id, leave(dry))));
    found.push(("notary", notary_check()));
    let slug = std::fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read {}/Cargo.toml: {e}", repo.display())))
        .and_then(|manifest| channel::update_channel_slug(&manifest))
        .and_then(|slug| slug.ok_or_else(|| Error::new("no update channel is committed")));
    match &slug {
        Ok(slug) => found.push(("channel", channel_token_check(slug, false))),
        Err(e) => found.push(("channel", Check::CouldNotRun(e.to_string()))),
    }
    let resolved = sign::ReleaseCredentials::resolve(profile, repo)
        .ok()
        .flatten();
    let derived = resolved.as_ref().map(|c| c.pubkey().to_string());
    let profile_path = profile.map_or_else(
        || Path::new(&home).join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL),
        Path::to_path_buf,
    );
    found.push((
        "profile",
        profile_check_at(
            &profile_path,
            &home,
            id,
            apple_sha1.as_deref(),
            derived.as_deref(),
            &crate::dev_identity::Bounded,
            leave(dry),
        ),
    ));
    let roster = slug.and_then(|slug| {
        let roster_path = repo.join("dist").join(roster::ROSTER_ASSET);
        let kept_path = kept_roster_path(&home);
        let mut paths = vec![roster_path.as_path(), kept_path.as_path()];
        if let Some(named) = resolved
            .as_ref()
            .and_then(sign::ReleaseCredentials::machine_roster)
        {
            paths.push(named);
        }
        let local = newest_local_candidate(&paths, pins::PAPER_MASTER_PUBKEYS)?;
        let fetched = fetch_channel_candidate(&slug);
        let (chosen, _, _) = choose_candidate(&roster_path, &slug, local, fetched)?;
        let pubkey = derived.clone().ok_or_else(|| {
            Error::new("this machine holds no release key (~/.aterm/release-ring/machine.key)")
        })?;
        authorize(chosen.bytes, &chosen.sig, &pubkey)
    });
    found.push((
        "roster",
        match roster {
            Ok(a) => Check::Pass(format!(
                "authorize_cut passes: '{}' signs under roster seq {}",
                a.machine_id, a.roster_seq
            )),
            Err(e) => Check::Owner {
                what: e.to_string(),
                fix: format!("{SHIP} provision --id {id}"),
            },
        },
    ));
    found
        .into_iter()
        .map(|(label, check)| Finding {
            label,
            state: audited(check),
        })
        .collect()
}

/// [`audit`] off unix.
#[cfg(not(unix))]
#[must_use]
pub fn audit(
    _repo: &std::path::Path,
    _id: &str,
    _profile: Option<&std::path::Path>,
    _dry: bool,
) -> Vec<Finding> {
    vec![Finding {
        label: "toolchain",
        state: Audited::Gap {
            what: "a release runs on macOS".into(),
            fix: "release from a provisioned Mac".into(),
            owner: false,
        },
    }]
}

/// Convert [`Check`] to [`Audited`] by its cause: an owner gap or a [`Check::Todo`] is the
/// owner's, a [`Check::Fail`] the machine's, an unanswered check [`Audited::CouldNotRun`].
#[cfg(unix)]
fn audited(check: Check) -> Audited {
    match check {
        Check::Pass(_) => Audited::Pass,
        Check::Fail { what, fix } => Audited::Gap {
            what,
            fix,
            owner: false,
        },
        Check::Owner { what, fix } => Audited::Gap {
            what,
            fix,
            owner: true,
        },
        Check::Todo { what, next } => Audited::Gap {
            what,
            fix: next,
            owner: true,
        },
        Check::Skip(why) => Audited::Gap {
            what: format!("impossible here — {why}"),
            fix: "release from a provisioned Mac".into(),
            owner: false,
        },
        Check::CouldNotRun(what) => Audited::CouldNotRun(what),
    }
}

/// The words an owner gap is asked with (`aterm ctl notice ask`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskWords {
    pub key: String,
    pub title: String,
    pub reason: String,
    pub step: String,
}

/// Where the release channel's token lives.
pub const CHANNEL_TOKEN_HOME: &str = "~/.secrets/gh_access_token_alabsystems";

/// [`AskWords`] for the gap labelled `label` whose words are `what`.
#[must_use]
pub fn ask_words(label: &str, what: &str) -> AskWords {
    let team = aterm_update_core::pins::APPLE_TEAM_ID;
    let (key, title, reason, step) = match label {
        "notary" if what.contains("agreement") => (
            "notary",
            "Apple agreement blocks the release".to_string(),
            format!(
                "Apple's developer agreement for team {team} lapsed, so notarization answers 403"
            ),
            "Accept the current agreements at developer.apple.com/account".to_string(),
        ),
        "notary" => (
            "notary",
            "Notary login blocks the release".to_string(),
            "Apple refuses the notary credential this Mac stores".to_string(),
            "Store a new app-specific password with xcrun notarytool store-credentials".to_string(),
        ),
        APPLE_LABEL => (
            "apple-id",
            "Signing certificate blocks the release".to_string(),
            "this Mac holds no Developer ID certificate that signs".to_string(),
            "Finish the certificate errand ship provision starts on this Mac".to_string(),
        ),
        "channel" => (
            "channel",
            "Channel token blocks the release".to_string(),
            "only the owner mints the release channel's token, and this Mac holds none".to_string(),
            format!("Save a new fine-grained channel token to {CHANNEL_TOKEN_HOME}"),
        ),
        _ => (
            "key",
            "Release key blocks the release".to_string(),
            "this Mac's release key is not authorized to sign a release".to_string(),
            "Run targo --unverified ship provision --id <machine-id> on this Mac".to_string(),
        ),
    };
    AskWords {
        key: format!("release.{key}"),
        title,
        reason,
        step,
    }
}

/// The cut's own admission gate.
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

/// One numbered phase header, and the only place the plan is stated.
#[cfg(unix)]
fn phase(n: usize, name: &str, what: &str) {
    println!();
    println!("  [{n}/5] {name} — {what}");
}

/// Store the notarytool credential if it is absent, then live-check it either way.
#[cfg(unix)]
fn notary_acquire(may_change: bool) -> Check {
    match notary_check() {
        live @ (Check::Pass(_) | Check::Skip(_) | Check::Owner { .. } | Check::CouldNotRun(_)) => {
            live
        }
        live if !may_change => live,
        _ => match crate::apple::ensure_notary(may_change) {
            crate::apple::Outcome::Ready { .. } => notary_check(),
            crate::apple::Outcome::Waiting { what, next }
            | crate::apple::Outcome::Todo { what, next } => Check::Todo { what, next },
            crate::apple::Outcome::Blocked { what, fix } => Check::Fail { what, fix },
            crate::apple::Outcome::Skipped(why) => Check::Skip(why),
        },
    }
}

/// Live-tested, not presence-tested.
#[cfg(unix)]
fn notary_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE); no keychain on this host".into());
    }
    let profile = crate::apple::NOTARY_PROFILE;
    match Command::new(aterm_uds::os_path!("xcrun"))
        .args(["notarytool", "history", "--keychain-profile", profile])
        .output()
    {
        Ok(out) => notary_verdict(
            profile,
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(e) => Check::Fail {
            what: format!("could not run xcrun notarytool: {e}"),
            fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
        },
    }
}

/// What `notarytool history` answered.
#[cfg(unix)]
fn notary_verdict(profile: &str, success: bool, stdout: &str, stderr: &str) -> Check {
    if success {
        return Check::Pass(format!(
            "notarytool profile '{profile}' answers (live-checked)"
        ));
    }
    let lines: Vec<&str> = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let lower: Vec<String> = lines.iter().map(|l| l.to_ascii_lowercase()).collect();
    let said = |needle: &str| lower.iter().position(|l| l.contains(needle));
    let first = lines.first().copied().unwrap_or("(no error line)");
    if let Some(at) = said("status code: 403").or_else(|| said("http 403"))
        && lower.iter().any(|l| l.contains("agreement"))
    {
        return Check::Owner {
            what: format!(
                "Apple's developer agreement for team {} is missing or expired — notarytool \
                 answers: {}",
                pins::APPLE_TEAM_ID,
                lines[at]
            ),
            fix: format!(
                "the team's Account Holder accepts the current agreements at \
                 https://developer.apple.com/account (no credential fixes this: \
                 store-credentials leaves the 403 as it is), then `{SHIP} release` goes on"
            ),
        };
    }
    let transport = [
        "offline",
        "timed out",
        "timeout",
        "could not connect",
        "network connection",
        "nsurlerrordomain",
        "could not be found",
    ];
    if said("status code").is_none()
        && lower
            .iter()
            .any(|l| transport.iter().any(|t| l.contains(t)))
    {
        return Check::CouldNotRun(format!(
            "Apple's notary service did not answer notarytool profile '{profile}': {first}"
        ));
    }
    Check::Owner {
        what: format!("notarytool profile '{profile}' did not authenticate: {first}"),
        fix: format!(
            "xcrun notarytool store-credentials {profile} --apple-id <your-apple-id> \
             --team-id {} (password: an app-specific password — mint at https://account.apple.com → Sign-In and Security → App-Specific Passwords)",
            pins::APPLE_TEAM_ID
        ),
    }
}

/// The credentials profile a Tier APPLE cut names on its command line.
#[cfg(unix)]
fn profile_check(
    home: &str,
    id: &str,
    apple_sha1: Option<&str>,
    machine_pubkey: Option<&str>,
    run: &dyn crate::dev_identity::Run,
    leave: crate::release_keychain::Leave,
) -> Check {
    let path = Path::new(home).join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL);
    profile_check_at(&path, home, id, apple_sha1, machine_pubkey, run, leave)
}

/// Audit exactly the profile the cut will load, including its permissions and identity.
#[cfg(unix)]
fn profile_check_at(
    path: &Path,
    home: &str,
    id: &str,
    apple_sha1: Option<&str>,
    machine_pubkey: Option<&str>,
    run: &dyn crate::dev_identity::Run,
    leave: crate::release_keychain::Leave,
) -> Check {
    if !cfg!(target_os = "macos") {
        return Check::Skip("cuts run on macOS (Tier APPLE)".into());
    }
    let rewrite = format!(
        "move {} aside and re-run `{SHIP} provision --id {id}` — provision writes it",
        path.display()
    );
    if !path.exists() {
        // Written only when `apple_sha1.is_some()`, so with no certificate a re-run writes
        // nothing: report the dependency, counted as waiting (it still blocks the mint).
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
            fix: format!("re-run `{SHIP} provision --id {id}` — provision writes it"),
        };
    }
    let creds = match sign::ReleaseCredentials::load(path, Some(Path::new(home))) {
        Ok(c) => c,
        Err(e) => {
            return Check::Fail {
                what: e,
                fix: rewrite,
            };
        }
    };
    if creds.notary().is_none() {
        return Check::Fail {
            what: format!("{} names no notarytool credential", path.display()),
            fix: rewrite,
        };
    }
    if let Some(declared) = creds.machine_id().filter(|d| *d != id) {
        return Check::Fail {
            what: format!(
                "{} declares machine_id = \"{declared}\", not '{id}'",
                path.display()
            ),
            fix: rewrite,
        };
    }
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
        (Some(pinned), Some(proved)) if !pinned.eq_ignore_ascii_case(proved) => {
            let kc = crate::release_keychain::Keychains::new(Path::new(home), run);
            match crate::apple::pinned_identity_proves(&kc, pinned, leave) {
                Ok(crate::release_keychain::Proof::Signed) => Check::Pass(format!(
                    "{} pins signing_identity_sha1 {pinned} — installed, valid and proved \
                     to sign (this run's first-listed identity was {proved}; both are \
                     usable)",
                    path.display()
                )),
                Ok(crate::release_keychain::Proof::Opened) => Check::Pass(format!(
                    "{} pins signing_identity_sha1 {pinned} — installed and valid in the \
                     release keychain, which opens; {} (this run's first-listed identity was \
                     {proved}; both are usable)",
                    path.display(),
                    crate::release_keychain::OPENED_NOTE
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
        _ => Check::Pass(format!(
            "{} loads under the cut's own rules",
            path.display()
        )),
    }
}

/// The channel token is the cut's GitHub identity.
#[cfg(unix)]
fn channel_token_check(slug: &str, may_change: bool) -> Check {
    use std::os::unix::fs::PermissionsExt as _;
    let where_it_lives = publish::channel_token_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.secrets/gh_access_token_alabsystems".into());
    let present = crate::github::has_token();
    let path = publish::channel_token_path();
    let open = path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| m.permissions().mode() & 0o077 != 0);
    let tightened = present
        && open
        && may_change
        && path.is_some_and(|p| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).is_ok()
        });
    channel_token_verdict(
        &where_it_lives,
        slug,
        present,
        open && !tightened,
        tightened,
    )
}

/// The channel token's verdict: a missing token is the owner's to mint ([`Check::Owner`]); a
/// file open to group or others is this machine's to `chmod` ([`Check::Fail`]).
#[cfg(unix)]
fn channel_token_verdict(
    where_it_lives: &str,
    slug: &str,
    present: bool,
    open: bool,
    tightened: bool,
) -> Check {
    // "mint", not "copy": a per-machine token revokes per-machine, like the signing key.
    if !present {
        return Check::Owner {
            what: format!("no channel token at {where_it_lives}"),
            fix: format!(
                "1. https://github.com/settings/personal-access-tokens/new  (FINE-GRAINED, not \
                 the classic-token page)\n\
                 2. Contents: read/write on {slug}, short expiry\n\
                 3. (umask 077; cat > {where_it_lives})   (then paste it and press ^D)"
            ),
        };
    }
    if tightened {
        return Check::Pass(format!("{where_it_lives}  tightened to 0600"));
    }
    if open {
        return Check::Fail {
            what: format!("{where_it_lives} is group/other-accessible"),
            fix: format!("chmod 600 {where_it_lives}"),
        };
    }
    Check::Pass(format!("{where_it_lives}  0600, owner-only"))
}

/// Developer ID Application identities for `team_id` in a `find-identity` listing.
#[cfg(unix)]
pub(crate) fn devid_identities(listing: &str, team_id: &str) -> Vec<String> {
    let team_tag = format!("({team_id})");
    listing
        .lines()
        .filter(|l| l.contains("Developer ID Application:") && l.contains(&team_tag))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|t| aterm_codec::hex::is_hex(t, 40))
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

/// Unix seconds, failing rather than guessing.
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

    /// `--check` and a dry release promise no writes, so they only look at the release
    /// keychain; a provision run and a real release's audit may re-assert its partition list.
    #[cfg(unix)]
    #[test]
    fn a_check_only_looks_at_the_release_keychain() {
        use crate::release_keychain::Leave;
        assert_eq!(leave(true), Leave::Look);
        assert_eq!(leave(false), Leave::Write);
    }

    /// The keychain line follows its `leave`: a look (the line's, and the pinned identity's
    /// proof) opens a held identity's keychain and writes nothing else, and runs no codesign
    /// even with its key's partition list gone; a write re-asserts the partition list and
    /// test-signs.
    #[cfg(unix)]
    #[test]
    fn the_keychain_line_looks_or_test_signs_as_told() {
        use crate::release_keychain::fake::{Fake, World};
        use crate::release_keychain::{Keychains, Leave, OPENED_NOTE, login_keychain_path};
        let sha1 = "D".repeat(40);
        let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
        let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
        let mut world = World::new(&sha1);
        world.search_list = vec![login_keychain_path(&home)];
        let fake = Fake::new(world);
        let kc = Keychains::new(&home, &fake);
        kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect("install");
        let release = kc.release();
        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&release)
            .unwrap()
            .partition = false;
        fake.0.borrow_mut().log.clear();

        let Check::Pass(line) = keychain_check_in(&home, "m9", &fake, Leave::Look) else {
            panic!("a held identity is ready to a look");
        };
        assert!(line.contains(OPENED_NOTE), "{line}");
        assert_eq!(
            crate::apple::pinned_identity_proves(&kc, &sha1, Leave::Look),
            Ok(crate::release_keychain::Proof::Opened)
        );
        let changed: Vec<_> = fake
            .writes()
            .into_iter()
            .filter(|c| c[1] != "unlock-keychain")
            .collect();
        assert!(changed.is_empty(), "a look only unlocks: {changed:?}");
        assert!(fake.calls("codesign", "--force").is_empty());

        let Check::Pass(line) = keychain_check_in(&home, "m9", &fake, Leave::Write) else {
            panic!("a write re-asserts and signs");
        };
        assert!(line.contains("a test signature passed"), "{line}");
        assert!(fake.0.borrow().keychains[&release].partition);
    }

    /// The audit's shim line is the build's own refusal, over the `rustup` row's answer: a
    /// symlinked cargo beside it (how one machine's rustup linked it) fails with the build's
    /// words, the same proxy as a hard link (how the row's copy lays it) passes, and no
    /// rustup at all fails naming the row's provider. A failing copy's fix removes and
    /// re-places it; a failing rustup on `$PATH` (Homebrew's link) is fixed by placing the
    /// copy that outranks it, never by editing that directory.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_cargo_proxy_fails_the_audit_as_it_fails_the_build() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = aterm_tempfile::Builder::new()
            .prefix("provision-shims-")
            .tempdir()
            .unwrap();
        let bin = home.path().join("tooling.noindex/rustup-1.29.2/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(bin.join("rustup"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(bin.join("rustup"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::os::unix::fs::symlink("rustup", bin.join("cargo")).unwrap();
        let tooling = home.path().join("tooling.noindex");
        let verdict = || rust_shim_verdict(Ok(bin.join("rustup")), Some(&tooling));
        match verdict() {
            Check::Fail { what, fix } => {
                assert!(what.contains("not a real regular file"), "{what}");
                assert!(
                    fix.starts_with(&format!(
                        "remove {}, the row's copy",
                        tooling.join("rustup-1.29.2").display()
                    )),
                    "{fix}"
                );
                assert_eq!(fix.matches("verify provision").count(), 1, "{fix}");
            }
            other => panic!("a symlinked cargo passed the audit: {other:?}"),
        }
        std::fs::remove_file(bin.join("cargo")).unwrap();
        std::fs::hard_link(bin.join("rustup"), bin.join("cargo")).unwrap();
        assert!(matches!(verdict(), Check::Pass(_)), "{:?}", verdict());
        let row = aterm_uds::host::tool("rustup");
        let on_path = home.path().join("opt/homebrew/bin");
        std::fs::create_dir_all(&on_path).unwrap();
        std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::copy(bin.join("rustup"), on_path.join("rustup")).unwrap();
        std::os::unix::fs::symlink("rustup", on_path.join("cargo")).unwrap();
        match rust_shim_verdict(Ok(on_path.join("rustup")), Some(&tooling)) {
            Check::Fail { what, fix } => {
                assert!(what.contains("not a real regular file"), "{what}");
                assert!(fix.starts_with(&row.remedy()), "the row's provider: {fix}");
                assert!(
                    !fix.contains("ln -f") && !fix.contains("remove"),
                    "nothing is changed in another's directory: {fix}"
                );
            }
            other => panic!("a symlinked cargo on $PATH passed the audit: {other:?}"),
        }
        let absent = row
            .locate_in(&aterm_uds::host::Lookup {
                named: None,
                tooling: Some(home.path().join("empty")),
                path: None,
            })
            .unwrap_err();
        match rust_shim_verdict(Err(absent), Some(&tooling)) {
            Check::Fail { what, fix } => {
                assert!(what.starts_with("no rustup (looked in"), "{what}");
                assert_eq!(fix, row.remedy());
            }
            other => panic!("no rustup passed the audit: {other:?}"),
        }
    }

    #[test]
    fn notary_answers_are_classified_by_every_line_they_carry() {
        let agreement = "Error: HTTP status code: 403. A required agreement is missing or has \
                         expired. This request requires an in-effect agreement that has not been \
                         signed or has expired.";
        match notary_verdict(
            "aterm-notary",
            false,
            "",
            &format!("Fetching history\n{agreement}\n"),
        ) {
            Check::Owner { fix, .. } => {
                assert!(fix.contains("developer.apple.com"), "{fix}");
            }
            other => panic!("{other:?}"),
        }
        for credential in [
            "Error: HTTP status code: 401. Invalid credentials. Username or password is incorrect.",
            "Error: No Keychain password item found for profile: aterm-notary",
        ] {
            match notary_verdict("aterm-notary", false, "", credential) {
                Check::Owner { fix, .. } => assert!(fix.contains("store-credentials"), "{fix}"),
                other => panic!("{credential}: {other:?}"),
            }
        }
        match notary_verdict(
            "aterm-notary",
            false,
            "",
            "Error: The Internet connection appears to be offline.",
        ) {
            Check::CouldNotRun(_) => {}
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            notary_verdict("aterm-notary", true, "No submission history.", ""),
            Check::Pass(_)
        ));
        assert_eq!(
            audited(Check::Owner {
                what: "w".into(),
                fix: "f".into()
            }),
            Audited::Gap {
                what: "w".into(),
                fix: "f".into(),
                owner: true
            }
        );
        assert_eq!(
            audited(Check::Fail {
                what: "w".into(),
                fix: "f".into()
            }),
            Audited::Gap {
                what: "w".into(),
                fix: "f".into(),
                owner: false
            }
        );
    }

    #[test]
    fn a_gap_is_the_owners_by_its_cause_not_its_label() {
        let at = "~/.secrets/gh_access_token_alabsystems";
        assert!(matches!(
            channel_token_verdict(at, "alabsystems/aterm", false, false, false),
            Check::Owner { .. }
        ));
        assert!(
            matches!(
                channel_token_verdict(at, "alabsystems/aterm", true, true, false),
                Check::Fail { .. }
            ),
            "a file mode is this machine's"
        );
        assert!(matches!(
            channel_token_verdict(at, "alabsystems/aterm", true, false, true),
            Check::Pass(_)
        ));
    }

    #[test]
    fn every_owner_ask_is_one_the_wire_stands() {
        for (label, what) in [
            (
                "notary",
                "Apple's developer agreement for team X is missing or expired",
            ),
            (
                "notary",
                "notarytool profile 'aterm-notary' did not authenticate",
            ),
            (APPLE_LABEL, "no Developer ID Application identity"),
            ("channel", "no channel token"),
            ("profile", "no credentials profile"),
            ("roster", "the roster does not authorize this key"),
        ] {
            let w = ask_words(label, what);
            assert!(w.key.starts_with("release."), "{w:?}");
            assert!(
                w.reason.chars().count() <= aterm_messages::need::REASON_CAP,
                "{w:?}"
            );
            assert!(
                w.step.chars().count() <= aterm_messages::need::STEP_CAP,
                "{w:?}"
            );
            let line = format!(
                "ask {} tag=release {} -- {} -- {}",
                w.key, w.title, w.reason, w.step
            );
            if let Err(e) = aterm_messages::wire::NoticeRequest::parse(&line) {
                panic!("{line}: {e}");
            }
        }
    }
    use aterm_update_core::roster::{Machine, SUPPORTED_SCHEMA};

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

    /// The selected profile's valid pair is considered alongside the other local copies.
    /// Newer removal, a fork, a damaged signature and a half-pair all refuse admission.
    #[test]
    fn profile_roster_participates_without_hiding_newer_or_invalid_local_state() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dist = scratch.path().join("dist/aterm-machines.toml");
        let kept = scratch.path().join("kept/aterm-machines.toml");
        let named = scratch.path().join("profile/aterm-machines.toml");
        let master = seed().pubkey_b64().unwrap();
        let write = |path: &Path, seq, members, revoked| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = roster_with(seq, members, revoked)
                .to_toml()
                .unwrap()
                .into_bytes();
            let sig = seed().sign(&bytes).unwrap();
            std::fs::write(path, bytes).unwrap();
            std::fs::write(machines::RosterDocument::signature_path(path), sig).unwrap();
        };
        let choose = || newest_local_candidate(&[&dist, &kept, &named], &[&master]);
        let authorize = |candidate: Candidate| {
            machines::authorize_cut(
                &[&master],
                candidate.bytes,
                &candidate.sig,
                &pubkey_of(0x23),
                1_793_000_000,
            )
        };

        write(&dist, 3, vec![machine("m3", 0x42)], vec![]);
        write(&named, 5, vec![machine("m23", 0x23)], vec![]);
        let chosen = choose().unwrap().unwrap();
        assert_eq!(chosen.roster.roster_seq, 5);
        assert_eq!(authorize(chosen).unwrap().machine_id, "m23");

        write(&kept, 7, vec![machine("m3", 0x42)], vec!["m23".into()]);
        let chosen = choose().unwrap().unwrap();
        assert_eq!(chosen.roster.roster_seq, 7);
        assert!(
            authorize(chosen).is_err(),
            "the older profile cannot restore membership"
        );

        write(&named, 7, vec![machine("m23", 0x23)], vec![]);
        assert!(
            choose().is_err(),
            "different signed documents at one generation refuse"
        );

        write(&named, 5, vec![machine("m23", 0x23)], vec![]);
        let signature = machines::RosterDocument::signature_path(&named);
        let mut damaged = std::fs::read(&signature).unwrap();
        damaged[0] ^= 1;
        std::fs::write(&signature, damaged).unwrap();
        assert!(choose().is_err(), "a bad named signature is never ignored");
        std::fs::remove_file(&signature).unwrap();
        assert!(choose().is_err(), "a half named pair is never ignored");
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

        let (_, b, s) = signed_candidate(3);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (chosen, install, _) = choose_candidate(path, "o/r", Some(local), Ok(newer)).unwrap();
        assert_eq!(chosen.roster.roster_seq, 5);
        assert!(install);

        let (_, b, s) = signed_candidate(9);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (chosen, install, _) = choose_candidate(path, "o/r", Some(local), Ok(older)).unwrap();
        assert_eq!(chosen.roster.roster_seq, 9);
        assert!(!install);

        let (_, b, s) = signed_candidate(4);
        let local = admit_candidate(&[&master], b.clone(), s.clone()).unwrap();
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, _) = choose_candidate(path, "o/r", Some(local), Ok(fetched)).unwrap();
        assert!(!install);

        let (_, b, s) = signed_candidate(2);
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, _) = choose_candidate(path, "o/r", None, Ok(fetched)).unwrap();
        assert!(install);

        let (_, b, s) = signed_candidate(2);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (_, install, _) =
            choose_candidate(path, "o/r", Some(local), Err("offline".into())).unwrap();
        assert!(!install);

        assert!(choose_candidate(path, "owner/repo", None, Err("offline".into())).is_err());
    }

    #[test]
    fn equal_generation_with_different_bytes_is_a_lineage_fork_and_a_hard_stop() {
        let (master, b, s) = signed_candidate_with(4, vec![machine("m3", 0x42)]);
        let local = admit_candidate(&[&master], b, s).unwrap();
        let (_, b, s) = signed_candidate_with(4, vec![machine("m3", 0x42), machine("mx", 0x43)]);
        let fetched = admit_candidate(&[&master], b, s).unwrap();
        assert!(
            choose_candidate(
                Path::new("dist/aterm-machines.toml"),
                "o/r",
                Some(local),
                Ok(fetched),
            )
            .is_err()
        );
    }

    #[test]
    fn local_roster_state_that_cannot_be_proven_is_a_hard_stop_not_a_reseed() {
        let dir = std::env::temp_dir().join(format!("provision-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm-machines.toml");

        std::fs::write(&path, b"schema = 1\n").unwrap();
        assert!(read_local_candidate(&path).is_err());

        std::fs::remove_file(&path).unwrap();
        std::fs::write(machines::RosterDocument::signature_path(&path), [0u8; 64]).unwrap();
        assert!(read_local_candidate(&path).is_err());

        std::fs::write(&path, b"schema = 1\n").unwrap();
        assert!(read_local_candidate(&path).is_err());

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

    /// A fresh Ed25519 keypair as base64 PKCS#8 (the shape `signing_key` carries) and the
    /// public key it derives.
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

    /// An alternate profile is audited in place; neither an absent nor an invalid default
    /// changes which credentials the cut selected. Its own permissions still apply.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_explicit_profile_is_audited_without_replacing_the_default() {
        use crate::release_keychain::Leave;
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path();
        let home_s = home.to_str().unwrap();
        let explicit = home.join("selected.toml");
        let default = home.join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL);
        let (key, pubkey) = signing_key_b64();
        let sha1 = "0".repeat(40);
        let good = format!(
            "signing_key = \"{key}\"\nmachine_id = \"m9\"\nnotary_profile = \"notary\"\n\
             signing_identity_sha1 = \"{sha1}\"\n"
        );
        std::fs::write(&explicit, &good).unwrap();
        std::fs::set_permissions(&explicit, std::fs::Permissions::from_mode(0o600)).unwrap();
        let fake = crate::release_keychain::fake::Fake::new(
            crate::release_keychain::fake::World::new(&"F".repeat(40)),
        );
        let check = || {
            profile_check_at(
                &explicit,
                home_s,
                "m9",
                Some(&sha1),
                Some(&pubkey),
                &fake,
                Leave::Look,
            )
        };
        assert!(!default.exists());
        assert!(matches!(check(), Check::Pass(_)));
        assert!(
            !default.exists(),
            "the audit does not materialize a default"
        );

        std::fs::create_dir_all(default.parent().unwrap()).unwrap();
        std::fs::write(&default, "invalid default").unwrap();
        assert!(matches!(check(), Check::Pass(_)));
        assert_eq!(std::fs::read(&explicit).unwrap(), good.as_bytes());
        assert_eq!(std::fs::read(&default).unwrap(), b"invalid default");

        std::fs::write(&default, &good).unwrap();
        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&explicit, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(check(), Check::Fail { .. }));
        assert_eq!(std::fs::read(&explicit).unwrap(), good.as_bytes());
    }

    /// The audit refuses exactly what the cut refuses: each profile below is one the cut
    /// rejects on its first line.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_profile_audit_refuses_what_the_cut_refuses() {
        use crate::release_keychain::Leave;
        use std::os::unix::fs::PermissionsExt as _;
        let home = std::env::temp_dir().join(format!("provision-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(atpkg_keys::provision::RELEASE_RING_REL)).unwrap();
        let path = home.join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL);
        let home_s = home.to_str().unwrap().to_string();
        let sha1 = "0".repeat(40);

        let write = |body: &str, mode: u32| {
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let refusal = |c: Check| match c {
            Check::Fail { .. } => {}
            Check::Pass(msg) => panic!("expected a refusal, got PASS: {msg}"),
            _ => panic!("expected a refusal"),
        };

        let (key, pubkey) = signing_key_b64();
        let mine = Some(pubkey.as_str());
        // Keychains modelled in the scratch HOME, holding no identity: no real one is read.
        let fake = crate::release_keychain::fake::Fake::new(
            crate::release_keychain::fake::World::new(&"F".repeat(40)),
        );

        refusal(profile_check(
            &home_s,
            "m9",
            Some(&sha1),
            mine,
            &fake,
            Leave::Look,
        ));

        write(
            &format!("notary_profile = \"notary\"\nsigning_identity_sha1 = \"{sha1}\"\n"),
            0o600,
        );
        refusal(profile_check(
            &home_s,
            "m9",
            Some(&sha1),
            mine,
            &fake,
            Leave::Look,
        ));

        let good = format!(
            "signing_key = \"{key}\"\nmachine_id = \"m9\"\nnotary_profile = \"notary\"\n\
             signing_identity_sha1 = \"{sha1}\"\n"
        );
        write(&good, 0o644);
        refusal(profile_check(
            &home_s,
            "m9",
            Some(&sha1),
            mine,
            &fake,
            Leave::Look,
        ));

        write(&good, 0o600);
        refusal(profile_check(
            &home_s,
            "m9",
            Some(&"1".repeat(40)),
            mine,
            &fake,
            Leave::Look,
        ));
        assert!(
            !fake.calls("security", "find-identity").is_empty(),
            "the pinned identity is looked up in the modelled keychains"
        );

        refusal(profile_check(
            &home_s,
            "m8",
            Some(&sha1),
            mine,
            &fake,
            Leave::Look,
        ));

        let (_, other) = signing_key_b64();
        refusal(profile_check(
            &home_s,
            "m9",
            Some(&sha1),
            Some(&other),
            &fake,
            Leave::Look,
        ));

        write(
            &format!(
                "{good}machine_roster = \"{}\"\n",
                home.join("gone/roster.toml").display()
            ),
            0o600,
        );
        refusal(profile_check(
            &home_s,
            "m9",
            Some(&sha1),
            mine,
            &fake,
            Leave::Look,
        ));

        write(
            &format!("{good}machine_roster = \"~/fleet/roster.toml\"\n"),
            0o600,
        );
        refusal(profile_check(
            &home_s,
            "m9",
            Some(&sha1),
            mine,
            &fake,
            Leave::Look,
        ));
        std::fs::create_dir_all(home.join("fleet")).unwrap();
        std::fs::write(home.join("fleet/roster.toml"), "").unwrap();
        assert!(matches!(
            profile_check(&home_s, "m9", Some(&sha1), mine, &fake, Leave::Look),
            Check::Pass(_)
        ));

        write(&good, 0o600);
        assert!(matches!(
            profile_check(&home_s, "m9", Some(&sha1), mine, &fake, Leave::Look),
            Check::Pass(_)
        ));
        write(
            &format!("signing_key = \"{key}\"\nnotary_profile = \"notary\"\n"),
            0o600,
        );
        assert!(matches!(
            profile_check(&home_s, "m9", Some(&sha1), mine, &fake, Leave::Look),
            Check::Pass(_)
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

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
        assert_eq!(blockers(&checks), 2);
    }

    #[test]
    fn the_authorized_generation_is_kept_outside_the_sweepable_checkout() {
        let dir = std::env::temp_dir().join(format!("provision-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dist = dir.join("dist").join("aterm-machines.toml");
        let home = dir.join("home");
        let kept = kept_roster_path(home.to_str().unwrap());
        let lock = lock_roster_pair(&dist).unwrap();

        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());

        let (master, bytes, sig) = signed_candidate(6);
        let c = admit_candidate(&[&master], bytes, sig).unwrap();
        install_pair(&lock, &dist, &c).unwrap();

        assert!(keep_roster_pair(&dist, &kept).unwrap());
        let (a, b) = (
            machines::RosterDocument::read(&dist).unwrap(),
            machines::RosterDocument::read(&kept).unwrap(),
        );
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(a.signature, b.signature);
        assert!(!keep_roster_pair(&dist, &kept).unwrap());

        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());

        let kept_sig = machines::RosterDocument::signature_path(&kept);
        let sig_bytes = std::fs::read(&kept_sig).unwrap();
        std::fs::remove_file(&kept_sig).unwrap();
        keep_roster_pair(&dist, &kept).unwrap_err();
        assert!(!kept_sig.exists(), "a refused copy wrote nothing");
        std::fs::write(&kept_sig, &sig_bytes).unwrap();
        assert!(
            !keep_roster_pair(&dist, &kept).unwrap(),
            "and it is whole again"
        );

        std::fs::remove_file(&dist).unwrap();
        std::fs::remove_file(machines::RosterDocument::signature_path(&dist)).unwrap();
        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());
        assert!(!dist.exists());

        std::fs::remove_file(machines::RosterDocument::signature_path(&kept)).unwrap();
        assert!(!restore_kept_pair(&lock, &kept, &dist).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_front_door_is_targo_and_trustc_answering_version_with_no_rustup() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("provision-front-door-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("store/trust/9192/bin");
        std::fs::create_dir_all(&bin).unwrap();
        for tool in ["targo", "trustc"] {
            std::fs::write(bin.join(tool), b"#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(bin.join(tool), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let answers = |tool: &Path| -> std::result::Result<String, String> {
            Ok(format!(
                "{} 1.99.0-dev (321aaeda7 2026-09-17)\nrelease: 1.99.0-dev",
                tool.file_name().unwrap().to_string_lossy()
            ))
        };

        match front_door_verdict(&bin, answers) {
            Check::Pass(_) => {}
            other => panic!("a store bin whose pair answers is the golden path: {other:?}"),
        }

        let mute = |tool: &Path| -> std::result::Result<String, String> {
            if tool.ends_with("trustc") {
                Err("dyld: Library not loaded".into())
            } else {
                answers(tool)
            }
        };
        match front_door_verdict(&bin, mute) {
            Check::Fail { .. } => {}
            other => panic!("a mute compiler is a GAP: {other:?}"),
        }

        std::fs::remove_file(bin.join("targo")).unwrap();
        match front_door_verdict(&bin, answers) {
            Check::Fail { .. } => {}
            other => panic!("a missing driver is a GAP: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stage2_missing_its_verifiers_is_rebuilt_in_a_trust_checkout() {
        let bin = std::env::temp_dir().join(format!("provision-verifiers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        match verifiers_check(&bin) {
            Check::Fail { what, fix } => {
                assert!(
                    what.starts_with("targo + tippy + ty + trustdoc missing"),
                    "{what}"
                );
                assert!(!fix.contains('~'), "a home folder: {fix}");
                assert!(fix.contains("the Trust checkout's bootstrap.toml"), "{fix}");
                assert!(fix.contains(gates::TRUST_FROM_SOURCE), "{fix}");
            }
            other => panic!("an empty bin is missing every verifier: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&bin);
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

        assert_eq!(resolve_on_path("trustdoc", &joined), None);

        std::fs::write(b.join("trustdoc"), b"").unwrap();
        assert_eq!(resolve_on_path("trustdoc", &joined), None);

        std::fs::set_permissions(b.join("trustdoc"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert_eq!(
            resolve_on_path("trustdoc", &joined),
            Some(b.join("trustdoc"))
        );

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

        std::fs::remove_file(&farm).unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), &farm).unwrap();
        link_farm(&farm, &target).expect("repair dangling");
        assert_eq!(std::fs::read_link(&farm).unwrap(), target);

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
            refused.unwrap_err();
        }

        std::fs::remove_file(&farm).unwrap();
        std::os::unix::fs::symlink(&target, &farm).unwrap();
        let other = dir.join("other-trustdoc");
        std::fs::write(&other, b"#!/bin/sh\n").unwrap();
        link_farm(&farm, &other).unwrap_err();
        assert_eq!(std::fs::read_link(&farm).unwrap(), target);

        std::fs::remove_file(&farm).unwrap();
        std::fs::write(&farm, b"not a link").unwrap();
        link_farm(&farm, &target).unwrap_err();
        assert_eq!(std::fs::read_to_string(&farm).unwrap(), "not a link");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_death_between_the_phase_one_pair_renames_recovers_forward() {
        let dir =
            std::env::temp_dir().join(format!("provision-phase1-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dist = dir.join("dist").join("aterm-machines.toml");
        let sig_path = machines::RosterDocument::signature_path(&dist);

        let (master, six, six_sig) = signed_candidate(6);
        let chosen = admit_candidate(&[&master], six.clone(), six_sig.clone()).unwrap();
        let lock = lock_roster_pair(&dist).expect("the phase's writer lock");
        install_pair(&lock, &dist, &chosen).expect("phase 1 installs the chosen pair");
        assert_eq!(std::fs::read(&dist).unwrap(), six);
        assert_eq!(std::fs::read(&sig_path).unwrap(), six_sig);

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

        let (_, eight, eight_sig) = signed_candidate(8);
        write_pair(&lock, &dist, &eight, &eight_sig).expect("phase 1 writes on");
        assert_eq!(std::fs::read(&dist).unwrap(), eight);
        assert_eq!(std::fs::read(&sig_path).unwrap(), eight_sig);
        assert!(!std::path::Path::new(&transaction).exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
