// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine provisioning for `targo --unverified ship provision --id <machine-id>`.

#[cfg(unix)]
use std::fmt::Write as _;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use aterm_secrets::need::{GhProfile, Purpose, Want};
#[cfg(unix)]
use aterm_uds::spawnfd::command;
#[cfg(unix)]
use aterm_update_core::pins;
#[cfg(unix)]
use aterm_update_core::roster;

#[cfg(unix)]
use crate::ledger::{Error, Result};
#[cfg(unix)]
use crate::machines::current_roster::{
    Candidate, admit_candidate, read_local_candidate_with, require_public_pair,
    require_public_pair_held, require_public_source, same_candidate, utf8_path,
};
#[cfg(unix)]
use crate::publish::step;
#[cfg(unix)]
use crate::{channel, gates, machines, publish, sign};

#[cfg(unix)]
#[path = "provision/layer.rs"]
mod layer;

/// Check or restore only the authenticated shared notary credential, without provisioning
/// release identities or credential files, packages, site keys, or credential requests.
pub fn run_notary_only(check_only: bool) -> crate::ledger::Result<()> {
    #[cfg(unix)]
    {
        let check = notary_only_with(check_only, notary_probe, || {
            aterm_secrets::client::Paths::live()
                .and_then(|paths| aterm_secrets::client::release::sync_notary(&paths))
                .map_err(|error| error.to_string())
        });
        print_check("notary", &check);
        if matches!(check, Check::Pass(_)) {
            println!(
                "NOTARY READY{}",
                if check_only {
                    " (--check: nothing written)"
                } else {
                    ""
                }
            );
            Ok(())
        } else {
            Err(Error::new("notary-only: credential is not ready"))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = check_only;
        Err(crate::ledger::Error::new("notary-only requires macOS"))
    }
}

#[cfg(unix)]
fn notary_only_with(
    check_only: bool,
    mut probe: impl FnMut() -> NotaryProbe,
    sync: impl FnOnce() -> std::result::Result<Option<String>, String>,
) -> Check {
    let initial = probe();
    if check_only || matches!(initial.check, Check::Pass(_) | Check::Skip(_)) {
        return initial.check;
    }
    match sync() {
        Ok(Some(_)) => probe().check,
        Ok(None) => Check::Todo {
            what: "no authenticated shared notary credential is available".into(),
            next: "inspect the vault membership and registered release-notary entry".into(),
        },
        Err(why) => Check::Fail {
            what: why,
            fix: "retry after the shared credential or notary service recovers".into(),
        },
    }
}

/// Restore only an already-authorized signer's public roster pair.
pub fn run_roster_only(
    repo: &std::path::Path,
    id: &str,
    check: bool,
    input: &std::path::Path,
) -> crate::ledger::Result<()> {
    #[cfg(unix)]
    {
        let home = std::env::var("HOME").map_err(|_| Error::new("HOME is not set"))?;
        let who = roster_only_in(
            repo,
            Path::new(&home),
            id,
            check,
            input,
            pins::PAPER_MASTER_PUBKEYS,
            now_unix()? as i64,
        )?;
        println!(
            "roster-only: {} authorized under roster seq {}{}",
            who.machine_id,
            who.roster_seq,
            if check {
                " (--check: nothing written)"
            } else {
                " (canonical pair verified)"
            }
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (repo, id, check, input);
        Err(crate::ledger::Error::new(
            "roster-only provisioning is POSIX-only",
        ))
    }
}

/// Read only the canonical key and identity; never resolve a credential profile.
#[cfg(unix)]
fn existing_signer(repo: &Path, home: &Path, id: &str) -> Result<machines::MachineIdentity> {
    atpkg_keys::provision::vet_machine_id(id).map_err(Error::new)?;
    if !home.is_absolute() {
        return Err(Error::new("HOME must be absolute"));
    }
    let home_text = utf8_path(home)?;
    let credentials = sign::ReleaseCredentials::resolve_with(None, repo, Some(&home_text))
        .map_err(Error::new)?
        .ok_or_else(|| {
            Error::new("roster-only requires an existing machine key; it never mints")
        })?;
    let identity = machines::MachineIdentity::read(&machines::identity_path_in(home))?
        .ok_or_else(|| Error::new("existing machine key has no canonical identity"))?;
    if identity.id != id || identity.pubkey != credentials.pubkey() {
        return Err(Error::new(
            "existing machine id or derived public key disagrees with canonical identity",
        ));
    }
    Ok(identity)
}

/// Selection, authorization and publication share the kept pair's transaction lock.
/// The check path only reads, including refusing an unfinished pair transaction.
#[cfg(unix)]
fn roster_only_in(
    repo: &Path,
    home: &Path,
    id: &str,
    check: bool,
    input: &Path,
    masters: &[&str],
    now: i64,
) -> Result<roster::Attribution> {
    let identity = existing_signer(repo, home, id)?;
    let kept = home.join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    let lock = if check {
        None
    } else {
        Some(lock_roster_pair(&kept)?)
    };
    let dist = repo.join("dist").join(roster::ROSTER_ASSET);
    // Even an absent explicit input is an error, not permission to fall back.
    atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(input)?)
        .map_err(Error::new)?;
    if read_local_candidate_with(input, masters)?.is_none() {
        return Err(Error::new("--roster public pair is absent"));
    }
    let current = machines::current_roster::resolve(
        home,
        &[&dist, &kept, input],
        masters,
        Ok(None),
        aterm_secrets::client::current_public_roster,
    )?;
    let selected = current.selected.clone();
    let authorize = |candidate: Candidate| {
        let who = machines::authorize_cut(
            masters,
            candidate.bytes,
            &candidate.sig,
            &identity.pubkey,
            now,
        )
        .map_err(|e| Error::new(e.to_string()))?;
        if who.machine_id != identity.id {
            return Err(Error::new(
                "signed roster authorizes the key under a different machine id",
            ));
        }
        Ok(who)
    };
    // Persist verified public denials before asking whether this signer is admitted.
    current.recheck(home, masters)?;
    if let Some(lock) = lock.as_ref() {
        promote_public_pair_held(lock, &kept, &selected, masters)?;
        current.recheck(home, masters)?;
        require_public_pair_held(&kept, &selected, masters)?;
        let effective = read_local_candidate_with(&kept, masters)?
            .ok_or_else(|| Error::new("canonical public pair vanished after publication"))?;
        return authorize(effective);
    }
    authorize(selected)
}

/// POSIX-only, exactly like the engine it drives.
#[cfg(not(unix))]
pub fn run_provision(
    _repo: &std::path::Path,
    _id: &str,
    _check_only: bool,
    _cert_dir: Option<&std::path::Path>,
    _mint_developer_id: bool,
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
    mint_developer_id: bool,
) -> Result<()> {
    atpkg_keys::provision::vet_machine_id(id).map_err(Error::new)?;

    let manifest = crate::durable::read_text(&repo.join("Cargo.toml"))
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
    let paths = aterm_secrets::client::Paths::for_home(Path::new(&home));
    let fetched = fetch_channel_candidate(&slug);
    let channel_unreadable = match &fetched {
        Err(error) if !error.starts_with(NO_CHANNEL_ROSTER) => Some(error.clone()),
        _ => None,
    };
    if let Some(why) = channel_unreadable.as_ref() {
        step(
            "roster",
            &format!("the public channel could not be read ({why}); minting remains refused"),
        );
    }
    let current = machines::current_roster::resolve(
        Path::new(&home),
        &[&roster_path, &kept_path],
        pins::PAPER_MASTER_PUBKEYS,
        fetched.map(Some),
        aterm_secrets::client::current_public_roster,
    )?;
    current.recheck(Path::new(&home), pins::PAPER_MASTER_PUBKEYS)?;
    if !check_only {
        // Public denial evidence must survive this signer's failed authorization.
        promote_public_pair(&kept_path, &current.selected, pins::PAPER_MASTER_PUBKEYS)?;
        promote_public_pair(&roster_path, &current.selected, pins::PAPER_MASTER_PUBKEYS)?;
        current.require_canonical(Path::new(&home), pins::PAPER_MASTER_PUBKEYS)?;
    }
    step(
        "roster",
        &format!(
            "selected newest admitted public roster_seq {}{}",
            current.selected.roster.roster_seq,
            if check_only {
                " — not written (--check)"
            } else {
                ""
            }
        ),
    );
    let machines::current_roster::CurrentRoster {
        selected: mut chosen,
        observed: observed_vault,
        floor: observed_floor,
    } = current;

    let key_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_KEY_REL);
    let identity_path = Path::new(&home).join(atpkg_keys::provision::MACHINE_PUB_REL);

    if !check_only {
        require_public_pair(&kept_path, &chosen, pins::PAPER_MASTER_PUBKEYS)?;
    }
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
                    &aterm_verify::provision::Options {
                        release: true,
                        ..Default::default()
                    },
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
    let source_helper = if check_only {
        Ok(None)
    } else {
        let explicit = std::env::var_os("ATPKG_KEYS");
        let system = aterm_verify::provision::System::new(repo);
        crate::source_signing::prepare_helper(
            repo,
            explicit.as_deref(),
            &aterm_verify::EnvSnapshot::capture(),
            |args, plan| {
                use aterm_verify::provision::Host as _;
                step(
                    "source signer",
                    "building this checkout's atpkg-keys helper",
                );
                let mut set = Vec::new();
                let mut remove: Vec<String> = aterm_verify::CHILD_ENV_REMOVED
                    .iter()
                    .map(|key| (*key).into())
                    .collect();
                for (name, value) in plan.compiler_env().map_err(|e| e.to_string())? {
                    match value {
                        Some(value) => set.push((name.to_owned(), value.to_owned())),
                        None => remove.push(name.to_owned()),
                    }
                }
                system
                    .run(&aterm_verify::provision::Spawn {
                        program: system.targo()?,
                        args: args.to_vec(),
                        cwd: repo.to_path_buf(),
                        set,
                        remove,
                    })
                    .map(String::into_bytes)
            },
        )
        .map(Some)
    };
    let helper_ready = source_helper.is_ok();
    if let Err(why) = &source_helper {
        record(
            "source helper",
            Check::Fail {
                what: why.clone(),
                fix: "restore the helper build or the explicitly selected executable, then retry"
                    .into(),
            },
            &mut checks,
        );
    }
    let host: std::sync::Arc<dyn layer::ReleaseHost> = std::sync::Arc::new(
        layer::System::new(repo, Path::new(&home)).with_source_helper(source_helper),
    );
    let initial = if check_only {
        layer::audit(&host)
    } else {
        layer::audit_before_identity(&host)
    };
    for (label, check) in initial {
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
    let (apple_check, apple_sha1) = apple_identity_check(
        id,
        crate::apple::Change {
            may_change: !check_only,
            leave: leave(check_only),
            mint: mint_developer_id,
        },
        cert_dir,
    );
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
        channel_token_check(!check_only, !check_only),
        &mut checks,
    );

    let blocking = blockers(&checks);
    let host_cannot_cut = checks.iter().any(|(_, c)| matches!(c, Check::Skip(_)));

    let minted_result = if already || check_only || blocking > 0 {
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
        Some(mint(repo, id, &roster_path, || {
            require_public_source(
                &paths,
                observed_vault.as_ref(),
                observed_floor,
                pins::PAPER_MASTER_PUBKEYS,
                aterm_secrets::client::current_public_roster,
            )?;
            require_public_pair(&roster_path, &chosen, pins::PAPER_MASTER_PUBKEYS)?;
            require_public_pair(&kept_path, &chosen, pins::PAPER_MASTER_PUBKEYS)
        })?)
    };
    let (report, minted_pair) = match minted_result {
        Some((report, pair)) => (Some(report), Some(pair)),
        None => (None, None),
    };
    let minted = report.is_some();
    if let Some(pair) = minted_pair {
        let completed =
            admit_candidate(pins::PAPER_MASTER_PUBKEYS, pair.raw, pair.sig).map_err(Error::new)?;
        require_public_pair(&roster_path, &completed, pins::PAPER_MASTER_PUBKEYS)?;
        let report = report.as_ref().expect("a completed mint has its report");
        let who = machines::authorize_cut(
            pins::PAPER_MASTER_PUBKEYS,
            completed.bytes.clone(),
            &completed.sig,
            &report.machine_pubkey,
            now_unix()? as i64,
        )
        .map_err(|error| Error::new(error.to_string()))?;
        if who.roster_seq != report.roster_seq || who.machine_id != report.id {
            return Err(Error::new(
                "minted public roster disagrees with the completed ceremony",
            ));
        }
        chosen = completed;
    }

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
        match keep_roster_pair(&roster_path, &kept_path, pins::PAPER_MASTER_PUBKEYS) {
            Ok(true) => Some(format!(
                "kept at {} — dist/ is sweepable",
                kept_path.display()
            )),
            Ok(false) => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    if !check_only && helper_ready {
        for (label, check) in layer::audit_source_signing(&host) {
            record(label, check, &mut checks);
        }
    }
    require_public_source(
        &paths,
        observed_vault.as_ref(),
        observed_floor,
        pins::PAPER_MASTER_PUBKEYS,
        aterm_secrets::client::current_public_roster,
    )?;
    if key_path.exists() && !check_only {
        require_public_pair(&roster_path, &chosen, pins::PAPER_MASTER_PUBKEYS)?;
        require_public_pair(&kept_path, &chosen, pins::PAPER_MASTER_PUBKEYS)?;
        let identity = machines::MachineIdentity::read(&identity_path)?
            .ok_or_else(|| Error::new("canonical signer identity disappeared"))?;
        let who = machines::authorize_cut(
            pins::PAPER_MASTER_PUBKEYS,
            chosen.bytes.clone(),
            &chosen.sig,
            &identity.pubkey,
            now_unix()? as i64,
        )
        .map_err(|error| Error::new(error.to_string()))?;
        if who.machine_id != identity.id {
            return Err(Error::new(
                "effective public roster changed signer identity",
            ));
        }
    }
    let (fails, waiting) = tally(&checks);

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
        let _ = write!(roster, "  valid until {}", r.roster_valid_until);
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
fn mint(
    repo: &Path,
    id: &str,
    roster_path: &Path,
    validate: impl FnOnce() -> Result<()>,
) -> Result<(
    atpkg_keys::provision::Report,
    atpkg_keys::provision::RosterSnapshot,
)> {
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
    publish_mint_plan(planned, validate)
}

/// Validate after planning and immediately before the first ceremony publication.
#[cfg(unix)]
fn publish_mint_plan(
    planned: atpkg_keys::provision::Planned,
    validate: impl FnOnce() -> Result<()>,
) -> Result<(
    atpkg_keys::provision::Report,
    atpkg_keys::provision::RosterSnapshot,
)> {
    let public_pair = planned.public_roster();
    validate()?;
    atpkg_keys::provision::write_pins(&planned).map_err(Error::new)?;
    let report = atpkg_keys::provision::write_rest(planned).map_err(Error::new)?;
    Ok((report, public_pair))
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

/// Promote verified public evidence before asking whether it authorizes this signer.
#[cfg(unix)]
fn promote_public_pair(path: &Path, selected: &Candidate, masters: &[&str]) -> Result<bool> {
    let lock = lock_roster_pair(path)?;
    promote_public_pair_held(&lock, path, selected, masters)
}

/// Promote under the caller's existing writer custody without acquiring it again.
#[cfg(unix)]
fn promote_public_pair_held(
    lock: &atpkg_keys::provision::RosterLock,
    path: &Path,
    selected: &Candidate,
    masters: &[&str],
) -> Result<bool> {
    let current = read_local_candidate_with(path, masters)?;
    if let Some(current) = current.as_ref() {
        if current.roster.roster_seq > selected.roster.roster_seq {
            return Err(Error::new(
                "a newer public roster appeared during provisioning",
            ));
        }
        if current.roster.roster_seq == selected.roster.roster_seq
            && current.bytes != selected.bytes
        {
            return Err(Error::new(
                "LINEAGE FORK: public roster changed at the selected generation",
            ));
        }
    }
    if same_candidate(current.as_ref(), Some(selected)) {
        return Ok(false);
    }
    write_pair(lock, path, &selected.bytes, &selected.sig)?;
    require_public_pair_held(path, selected, masters)?;
    Ok(true)
}

/// The latest channel release's pair ([`machines::channel_roster_pair`]), admitted.
#[cfg(unix)]
fn fetch_channel_candidate(slug: &str) -> std::result::Result<Candidate, String> {
    let reader = crate::github::reader(machines::CUT_READ_PATIENCE).map_err(|e| e.to_string())?;
    // Only the channel answering that its latest release has no roster means none is published
    // yet; any other failure (a route that failed, a private repository) is "cannot tell", and
    // the mint gate refuses on it.
    let machines::RosterDocument { bytes, signature } =
        machines::channel_roster_pair(&reader, slug)?.ok_or_else(|| {
            format!(
                "{NO_CHANNEL_ROSTER}: {slug}'s latest release carries no {}",
                roster::ROSTER_ASSET
            )
        })?;
    admit_candidate(pins::PAPER_MASTER_PUBKEYS, bytes, signature)
}

/// Marker prefix when the channel's latest release answered that it carries no roster.
#[cfg(unix)]
const NO_CHANNEL_ROSTER: &str = "no roster on the channel";

/// Take the roster pair's writer lock.
#[cfg(unix)]
fn lock_roster_pair(roster_path: &Path) -> Result<atpkg_keys::provision::RosterLock> {
    atpkg_keys::provision::lock_roster(&utf8_path(roster_path)?).map_err(Error::new)
}

/// The pair a write is about to replace, read under its lock.
#[cfg(unix)]
fn pair_premise(roster_path: &Path) -> Result<Option<atpkg_keys::provision::RosterSnapshot>> {
    let sig_path = machines::RosterDocument::signature_path(roster_path);
    match (
        crate::durable::read_regular_bytes(roster_path, 65_536),
        crate::durable::read_regular_bytes(&sig_path, 4_096),
    ) {
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
    crate::durable::write_fresh(roster_path, bytes)
        .and_then(|()| crate::durable::write_fresh(&std::path::PathBuf::from(sig_path), sig))
        .map_err(|e| crate::ledger::Error::new(format!("write {}: {e}", roster_path.display())))
}

/// Durable copy of the roster generation under `~/.aterm/release-ring` beside the key.
#[cfg(unix)]
fn kept_roster_path(home: &str) -> PathBuf {
    Path::new(home).join(atpkg_keys::provision::RELEASE_ROSTER_REL)
}

/// Copy the pair `authorize_cut` just accepted into `~/.aterm/release-ring/roster`.
#[cfg(unix)]
fn keep_roster_pair(roster_path: &Path, kept_path: &Path, masters: &[&str]) -> Result<bool> {
    let candidate = read_local_candidate_with(roster_path, masters)?
        .ok_or_else(|| Error::new("selected public roster pair is absent"))?;
    promote_public_pair(kept_path, &candidate, masters)
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
    /// A gap only the owner can close: Apple's developer agreement, never a credential.
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
        let out = command(tool)
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

/// This machine's Developer ID identity: held, else imported from the vault registry, else
/// filed with the lab as `developer-id:<id>` ([`crate::apple`]). A run that may change things
/// moves the need either way.
#[cfg(unix)]
fn apple_identity_check(
    id: &str,
    change: crate::apple::Change,
    cert_dir: Option<&Path>,
) -> (Check, Option<String>) {
    let want = || Want::DeveloperId(id.to_owned());
    let outcome = crate::apple::acquire(id, change.may_change, change.leave, cert_dir, change.mint);
    if change.may_change && matches!(outcome, crate::apple::Outcome::Ready { .. }) {
        match aterm_secrets::client::Paths::live().and_then(|paths| {
            aterm_secrets::need::satisfied(&paths, &want(), Purpose::ReleaseDeveloperId)
        }) {
            Ok(Some(need)) => step("developer-id need", &need.line),
            Ok(None) => {}
            Err(error) => step(
                "developer-id need",
                &format!("could not record the request: {error}"),
            ),
        }
    }
    match outcome {
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
                let _ = write!(msg, "; {note}");
            }
            (Check::Pass(msg), Some(ids[0].clone()))
        }
        crate::apple::Outcome::Waiting { what, next }
        | crate::apple::Outcome::Todo { what, next } => (Check::Todo { what, next }, None),
        crate::apple::Outcome::Blocked { what, fix } => (Check::Fail { what, fix }, None),
        // Only a run that may change things yields it (a read-only one reports a to-do).
        crate::apple::Outcome::Unheld(what) => (
            filed(
                Check::Fail {
                    what,
                    fix: String::new(),
                },
                aterm_secrets::client::Paths::live(),
                want(),
                Purpose::ReleaseDeveloperId,
            ),
            None,
        ),
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
    let kc = crate::release_keychain::Keychains::waiting(home, run);
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
            Check::Fail {
                what: "this machine names no roster id (no ~/.aterm/release-ring/machine.toml, \
                       no machine_id in its credentials profile) — the cut signs only as a \
                       rostered machine"
                    .into(),
                fix: RELEASE_MACHINE.into(),
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
    let (apple, apple_sha1) = apple_identity_check(
        id,
        crate::apple::Change {
            may_change: false,
            leave: leave(dry),
            mint: false,
        },
        None,
    );
    found.push((crate::apple::APPLE_LABEL, apple));
    found.push((KEYCHAIN_LABEL, keychain_check(id, leave(dry))));
    found.push(("notary", notary_check_recording(!dry)));
    let slug = crate::durable::read_text(&repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read {}/Cargo.toml: {e}", repo.display())))
        .and_then(|manifest| channel::update_channel_slug(&manifest))
        .and_then(|slug| slug.ok_or_else(|| Error::new("no update channel is committed")));
    found.push(("channel", channel_token_check(false, !dry)));
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
        let fetched = fetch_channel_candidate(&slug);
        let pubkey = derived.clone().ok_or_else(|| {
            Error::new("this machine holds no release key (~/.aterm/release-ring/machine.key)")
        })?;
        audit_roster_at(
            Path::new(&home),
            &paths,
            pins::PAPER_MASTER_PUBKEYS,
            fetched,
            &pubkey,
            now_unix()? as i64,
        )
    });
    found.push((
        "roster",
        match roster {
            Ok(a) => Check::Pass(format!(
                "authorize_cut passes: '{}' signs under roster seq {}",
                a.machine_id, a.roster_seq
            )),
            Err(e) => Check::Fail {
                what: e.to_string(),
                fix: RELEASE_MACHINE.into(),
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

/// What `ship release` proves at step 1 of the writes its publish, cut and after-cut steps make
/// from this Mac ([`layer::distribution`]): the site checkout, the deployer's key and Google's
/// grant to it, the site checkout's push, and the routed identities' push to the repositories
/// and the Homebrew tap, each by a read or a dry run. A real run asks the lab's vault for a
/// deployer key missing where the vault delivers it; a dry run (`dry`) writes nothing.
#[cfg(unix)]
#[must_use]
pub fn distribution(repo: &Path, dry: bool) -> Vec<Finding> {
    let Ok(home) = std::env::var("HOME") else {
        return vec![Finding {
            label: "release.site",
            state: Audited::CouldNotRun("HOME is not set".into()),
        }];
    };
    let host: std::sync::Arc<dyn layer::ReleaseHost> =
        std::sync::Arc::new(layer::System::new(repo, Path::new(&home)));
    layer::distribution(&host, dry)
        .into_iter()
        .map(|(label, check)| Finding {
            label,
            state: audited(check),
        })
        .collect()
}

/// [`distribution`] off unix, where [`audit`] already refuses the release.
#[cfg(not(unix))]
#[must_use]
pub fn distribution(_repo: &std::path::Path, _dry: bool) -> Vec<Finding> {
    Vec::new()
}

/// The release audit's roster decision, separated from machine and provider probes.
#[cfg(unix)]
fn audit_roster_at(
    home: &Path,
    paths: &[&Path],
    masters: &[&str],
    fetched: std::result::Result<Candidate, String>,
    pubkey: &str,
    now: i64,
) -> Result<roster::Attribution> {
    let current = machines::current_roster::resolve(
        home,
        paths,
        masters,
        fetched.map(Some),
        aterm_secrets::client::current_public_roster,
    )?;
    let who = machines::authorize_cut(
        masters,
        current.selected.bytes.clone(),
        &current.selected.sig,
        pubkey,
        now,
    )
    .map_err(|error| Error::new(error.to_string()))?;
    current.require_canonical(home, masters)?;
    current.recheck(home, masters)?;
    Ok(who)
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

/// Convert [`Check`] to [`Audited`] by its cause: only a [`Check::Owner`] gap is the owner's
/// (no credential is one: a missing credential is filed with the lab and fails the step), any
/// other gap the machine's, an unanswered check [`Audited::CouldNotRun`].
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
            owner: false,
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

/// What a release gap this Mac cannot close says: a release is the release machine's to cut.
#[cfg(unix)]
const RELEASE_MACHINE: &str = "the release machine cuts releases; `targo --unverified ship \
                               provision --id <its roster id>` rosters this Mac \
                               (docs/RELEASE-KEYS.md)";

/// The notice key a gap labelled `label` is asked, withdrawn or resolved under.
#[must_use]
pub fn ask_key(label: &str) -> String {
    format!("release.{}", label.replace(' ', "-"))
}

/// The audit label of the one gap a person closes: the notary row's 403.
pub const AGREEMENT_LABEL: &str = "notary";

/// [`AskWords`] for Apple's developer agreement, the notary row's 403 and the one owner gap:
/// a person's act at Apple, not a credential.
#[must_use]
pub fn agreement_ask() -> AskWords {
    let team = aterm_update_core::pins::APPLE_TEAM_ID;
    AskWords {
        key: ask_key(AGREEMENT_LABEL),
        title: "Apple agreement blocks the release".to_string(),
        reason: format!(
            "Apple's developer agreement for team {team} lapsed, so notarization answers 403"
        ),
        step: "Accept the current agreements at developer.apple.com/account".to_string(),
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

/// Check or acquire the notary credential and report its live verdict to the need ledger.
#[cfg(unix)]
fn notary_acquire(may_change: bool) -> Check {
    let result = notary_acquire_with(may_change, notary_probe, crate::apple::ensure_notary);
    if may_change {
        report_notary_need(result)
    } else {
        result.check
    }
}

#[cfg(unix)]
fn notary_acquire_with(
    may_change: bool,
    mut probe: impl FnMut() -> NotaryProbe,
    ensure: impl FnOnce(bool) -> crate::apple::Outcome,
) -> NotaryProbe {
    let initial = probe();
    match initial.need {
        NotaryNeed::Raise if may_change => match ensure(may_change) {
            crate::apple::Outcome::Ready { .. } => probe(),
            crate::apple::Outcome::Waiting { what, next }
            | crate::apple::Outcome::Todo { what, next } => NotaryProbe {
                check: Check::Todo { what, next },
                need: initial.need,
            },
            crate::apple::Outcome::Blocked { what, fix } => NotaryProbe {
                check: Check::Fail { what, fix },
                need: initial.need,
            },
            crate::apple::Outcome::Unheld(what) => NotaryProbe {
                check: Check::Fail {
                    what,
                    fix: format!("`aterm secrets need {}`", Want::Notary.key()),
                },
                need: initial.need,
            },
            crate::apple::Outcome::Skipped(why) => NotaryProbe {
                check: Check::Skip(why),
                need: NotaryNeed::Unchanged,
            },
        },
        _ => initial,
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotaryNeed {
    Unchanged,
    Raise,
    Satisfied,
}

#[cfg(unix)]
struct NotaryProbe {
    check: Check,
    need: NotaryNeed,
}

/// Record the consumer's live verdict only on a real provision/release run. A dry audit reads
/// the provider without changing the credential-request ledger.
#[cfg(unix)]
fn notary_check_recording(record: bool) -> Check {
    let result = notary_probe();
    if record {
        report_notary_need(result)
    } else {
        result.check
    }
}

/// Move the notary need as `result` says; a raised need is the check's fix: filed, not asked.
#[cfg(unix)]
fn report_notary_need(result: NotaryProbe) -> Check {
    let paths = aterm_secrets::client::Paths::live();
    match result.need {
        NotaryNeed::Unchanged => result.check,
        NotaryNeed::Raise => filed(result.check, paths, Want::Notary, Purpose::ReleaseNotary),
        NotaryNeed::Satisfied => {
            match paths.and_then(|paths| close_notary_need(&paths)) {
                Ok(Some(line)) => step("notary need", &line),
                Ok(None) => {}
                Err(error) => step(
                    "notary need",
                    &format!("could not record the request: {error}"),
                ),
            }
            result.check
        }
    }
}

/// THE ONE PATH A CREDENTIAL THIS MAC LACKS TAKES: file `want` with the lab's Macs
/// (`aterm secrets need <want>`), and fail `check` naming the want and its filed request. No
/// person is asked for a credential.
#[cfg(unix)]
fn filed(
    check: Check,
    paths: aterm_secrets::Result<aterm_secrets::client::Paths>,
    want: Want,
    purpose: Purpose,
) -> Check {
    let (Check::Fail { what, .. } | Check::Todo { what, .. } | Check::Owner { what, .. }) = check
    else {
        return check;
    };
    let key = want.key();
    let fix = match paths.and_then(|paths| aterm_secrets::need::raise(&paths, want, purpose)) {
        Ok(need) => format!(
            "filed `aterm secrets need {key}` with the lab: {}",
            need.line
        ),
        Err(error) => format!("`aterm secrets need {key}` could not be filed: {error}"),
    };
    Check::Fail { what, fix }
}

/// The live check passed: the notary need, if one is open, is met.
#[cfg(unix)]
fn close_notary_need(
    paths: &aterm_secrets::client::Paths,
) -> aterm_secrets::Result<Option<String>> {
    aterm_secrets::need::satisfied(paths, &Want::Notary, Purpose::ReleaseNotary)
        .map(|need| need.map(|need| need.line))
}

/// Live-tested, not presence-tested.
#[cfg(unix)]
fn notary_probe() -> NotaryProbe {
    if !cfg!(target_os = "macos") {
        return NotaryProbe {
            check: Check::Skip("cuts run on macOS (Tier APPLE); no keychain on this host".into()),
            need: NotaryNeed::Unchanged,
        };
    }
    let profile = crate::apple::NOTARY_PROFILE;
    match command(aterm_uds::os_path!("xcrun"))
        .args(["notarytool", "history", "--keychain-profile", profile])
        .output()
    {
        Ok(out) => notary_probe_verdict(
            profile,
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(e) => NotaryProbe {
            check: Check::Fail {
                what: format!("could not run xcrun notarytool: {e}"),
                fix: "install the Xcode command-line tools (`xcode-select --install`)".into(),
            },
            need: NotaryNeed::Unchanged,
        },
    }
}

/// What `notarytool history` answered.
#[cfg(all(unix, test))]
fn notary_verdict(profile: &str, success: bool, stdout: &str, stderr: &str) -> Check {
    notary_probe_verdict(profile, success, stdout, stderr).check
}

#[cfg(unix)]
fn notary_probe_verdict(profile: &str, success: bool, stdout: &str, stderr: &str) -> NotaryProbe {
    if success {
        return NotaryProbe {
            check: Check::Pass(format!(
                "notarytool profile '{profile}' answers (live-checked)"
            )),
            need: NotaryNeed::Satisfied,
        };
    }
    let lines: Vec<&str> = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let has = |line: &str, needle: &str| {
        line.as_bytes()
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    };
    let said = |needle: &str| lines.iter().position(|l| has(l, needle));
    let first = lines.first().copied().unwrap_or("(no error line)");
    if let Some(at) = said("status code: 403").or_else(|| said("http 403"))
        && lines.iter().any(|l| has(l, "agreement"))
    {
        return NotaryProbe {
            check: Check::Owner {
                what: format!(
                    "Apple's developer agreement for team {} is missing or expired — notarytool \
                 answers: {}",
                    pins::APPLE_TEAM_ID,
                    lines[at]
                ),
                fix: format!(
                    "the team's Account Holder accepts the current agreements at \
                 https://developer.apple.com/account (no credential fixes this), then \
                 `{SHIP} release` goes on"
                ),
            },
            need: NotaryNeed::Unchanged,
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
    if said("status code").is_none() && lines.iter().any(|l| transport.iter().any(|t| has(l, t))) {
        return NotaryProbe {
            check: Check::CouldNotRun(format!(
                "Apple's notary service did not answer notarytool profile '{profile}': {first}"
            )),
            need: NotaryNeed::Unchanged,
        };
    }
    NotaryProbe {
        check: Check::Fail {
            what: format!("notarytool profile '{profile}' did not authenticate: {first}"),
            fix: format!(
                "`aterm secrets need {}` (the vault's {} entry sets profile '{profile}')",
                Want::Notary.key(),
                aterm_secrets::need::catalog::NOTARY_ENTRY
            ),
        },
        need: NotaryNeed::Raise,
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
            let kc = crate::release_keychain::Keychains::waiting(Path::new(home), run);
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

/// The channel token is the cut's GitHub identity: the vault's [`publish::CHANNEL_TOKEN`] entry.
/// `may_change` tightens an open file; `record` files a missing token with the lab.
#[cfg(unix)]
fn channel_token_check(may_change: bool, record: bool) -> Check {
    let path = publish::channel_token_path();
    channel_token_check_at(may_change, path.as_deref(), crate::github::has_token).unwrap_or_else(
        || {
            let missing = channel_token_missing(path.as_deref());
            if record {
                filed(
                    missing,
                    aterm_secrets::client::Paths::live(),
                    Want::Gh(GhProfile::Release),
                    Purpose::AghRelease,
                )
            } else {
                missing
            }
        },
    )
}

/// The verdict on no channel token at `path`: the want that brings it, never a person's errand.
#[cfg(unix)]
fn channel_token_missing(path: Option<&Path>) -> Check {
    Check::Fail {
        what: format!(
            "no channel token at {}",
            path.map_or_else(publish::channel_token_pointer, |p| p.display().to_string())
        ),
        fix: format!(
            "`aterm secrets need {}`",
            Want::Gh(GhProfile::Release).key()
        ),
    }
}

/// The token file at `path` judged, `None` when it holds no token.
#[cfg(unix)]
fn channel_token_check_at(
    may_change: bool,
    path: Option<&Path>,
    present: impl FnOnce() -> bool,
) -> Option<Check> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = path?;
    let where_it_lives = path.display();
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => meta,
        Ok(_) => {
            return Some(Check::Fail {
                what: format!("{where_it_lives} is not a regular token file"),
                fix: format!(
                    "restore this machine's private regular token file at {where_it_lives}"
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            return Some(Check::Fail {
                what: format!("cannot inspect {where_it_lives}: {error}"),
                fix: format!(
                    "make this machine's private regular token file readable at {where_it_lives}"
                ),
            });
        }
    };
    if !present() {
        return None;
    }
    let open = meta.permissions().mode() & 0o077 != 0;
    let tightened = open
        && may_change
        && crate::durable::open_regular_file(path)
            .and_then(|f| f.set_permissions(std::fs::Permissions::from_mode(0o600)))
            .is_ok();
    Some(if tightened {
        Check::Pass(format!("{where_it_lives}  tightened to 0600"))
    } else if open {
        Check::Fail {
            what: format!("{where_it_lives} is group/other-accessible"),
            fix: format!("chmod 600 {where_it_lives}"),
        }
    } else {
        Check::Pass(format!("{where_it_lives}  0600, owner-only"))
    })
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
#[path = "provision_tests.rs"]
mod tests;
