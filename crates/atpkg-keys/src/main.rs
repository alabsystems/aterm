// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-keys`: owner-only Ed25519 CLI for manifest signing and roster ceremonies.

#[cfg(unix)]
use atpkg_keys::fsio::{concat, read_bytes, write_bytes};
#[cfg(unix)]
use atpkg_keys::{Argv, MAX_ARGS, flag, positional, vet_args};
#[cfg(unix)]
use std::process::ExitCode;

#[cfg(windows)]
fn main() {
    eprintln!("atpkg-keys: unsupported on Windows (POSIX-only owner signing tool)");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() -> ExitCode {
    let mut it = std::env::args().skip(1);
    let verb = it.next();
    let mut argv: Argv = Default::default();
    for slot in argv.iter_mut() {
        *slot = it.next();
    }
    let overflowed = it.next().is_some();

    let r = if overflowed {
        Err(concat(&[
            "too many arguments — `atpkg-keys` reads at most ",
            &MAX_ARGS.to_string(),
            " after the verb",
        ]))
    } else {
        match verb.as_deref() {
            Some("pubkey") => {
                vet_args("pubkey", &argv, &[], 1).and_then(|()| pubkey(positional(&argv, 0)))
            }
            Some("sign") => vet_args("sign", &argv, &[], 3).and_then(|()| {
                sign(
                    positional(&argv, 0),
                    positional(&argv, 1),
                    positional(&argv, 2),
                )
            }),
            Some("master-check") => {
                vet_args("master-check", &argv, &[], 0).and_then(|()| master_check())
            }
            Some("verify-signed") => {
                vet_args("verify-signed", &argv, &[], 5).and_then(|()| verify_signed(&argv))
            }
            Some("setup") => vet_args("setup", &argv, PROVISION_FLAGS, 0)
                .and_then(|()| provision(atpkg_keys::provision::Verb::Setup, &argv)),
            Some("join") => vet_args("join", &argv, PROVISION_FLAGS, 0)
                .and_then(|()| provision(atpkg_keys::provision::Verb::Join, &argv)),
            Some("machine-revoke") => vet_args("machine-revoke", &argv, &["id", "roster"], 0)
                .and_then(|()| machine_revoke(&argv)),
            Some(other) => Err(concat(&[
                "unknown verb '",
                other,
                "' (try: setup, join, machine-revoke, master-check, pubkey, sign, verify-signed)",
            ])),
            None => Err(
                "usage: atpkg-keys <setup|join|machine-revoke|master-check|pubkey|sign|verify-signed> …"
                    .to_string(),
            ),
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprint_line(&concat(&["atpkg-keys: ", &e]));
            ExitCode::from(1)
        }
    }
}

/// `println!` without the fmt machinery.
#[cfg(unix)]
fn print_line(line: &str) {
    use std::io::Write as _;
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    if std::io::stdout().write_all(buf.as_bytes()).is_err() {
        std::panic::panic_any("failed printing to stdout");
    }
}

/// `eprintln!` without the fmt machinery.
#[cfg(unix)]
fn eprint_line(line: &str) {
    use std::io::Write as _;
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    if std::io::stderr().write_all(buf.as_bytes()).is_err() {
        std::panic::panic_any("failed printing to stderr");
    }
}

/// Read the secret pkcs8 key file.
#[cfg(unix)]
fn read_key(path: &str) -> Result<Vec<u8>, String> {
    read_bytes(path).map_err(|e| concat(&["read key ", path, ": ", &e.to_string()]))
}

#[cfg(unix)]
fn pubkey(key: Option<&str>) -> Result<(), String> {
    atpkg_keys::master::forbid_core_dumps();
    let key = key.ok_or("usage: atpkg-keys pubkey <key-file>")?;
    let mut pkcs8 = read_key(key)?;
    let encoded = atpkg_keys::pubkey_b64(&pkcs8);
    atpkg_keys::master::scrub(&mut pkcs8);
    print_line(&encoded?);
    Ok(())
}

/// `verify-signed`: check a file's signature against a roster that verifies under the master.
#[cfg(unix)]
fn verify_signed(argv: &Argv) -> Result<(), String> {
    const USAGE: &str = "usage: atpkg-keys verify-signed <master-pubkey-b64> <file> <file.sig> \
         <aterm-machines.toml> <aterm-machines.toml.sig>";
    let (Some(master), Some(file), Some(sig), Some(roster), Some(roster_sig)) = (
        positional(argv, 0),
        positional(argv, 1),
        positional(argv, 2),
        positional(argv, 3),
        positional(argv, 4),
    ) else {
        return Err(USAGE.to_string());
    };
    let read = |p: &str| read_bytes(p).map_err(|e| concat(&["read ", p, ": ", &e.to_string()]));
    let file_bytes =
        atpkg_keys::fsio::read_bytes_up_to(file, atpkg_keys::fsio::SIGNED_FILE_READ_CAP)
            .map_err(|e| concat(&["read ", file, ": ", &e.to_string()]))?;
    let now =
        i64::try_from(now_unix()?).map_err(|_| "the system clock is out of range".to_string())?;
    let who = atpkg_keys::verify_signed(
        &[master],
        read(roster)?,
        &read(roster_sig)?,
        &file_bytes,
        &read(sig)?,
        now,
    )
    .map_err(|_| {
        "FAIL: no live machine of a roster that verifies under that master signed this file"
            .to_string()
    })?;
    print_line(&concat(&[
        "OK: signed by machine ",
        &who.machine_id,
        " under roster seq ",
        &who.roster_seq.to_string(),
    ]));
    Ok(())
}

#[cfg(unix)]
fn sign(key: Option<&str>, msg: Option<&str>, sig_out: Option<&str>) -> Result<(), String> {
    atpkg_keys::master::forbid_core_dumps();
    let key = key.ok_or("usage: atpkg-keys sign <key-file> <file> [<sig-out>]")?;
    let msg = msg.ok_or("usage: atpkg-keys sign <key-file> <file> [<sig-out>]")?;
    let bytes = atpkg_keys::fsio::read_bytes_up_to(msg, atpkg_keys::fsio::SIGNED_FILE_READ_CAP)
        .map_err(|e| concat(&["read ", msg, ": ", &e.to_string()]))?;
    let mut pkcs8 = read_key(key)?;
    let sig = atpkg_keys::sign(&pkcs8, &bytes);
    atpkg_keys::master::scrub(&mut pkcs8);
    let sig = sig?;
    let path = match sig_out {
        Some(path) => path.to_string(),
        None => concat(&[msg, ".sig"]),
    };
    write_bytes(&path, &sig).map_err(|e| concat(&["write ", &path, ": ", &e.to_string()]))?;
    let shown = match std::env::var("HOME") {
        Ok(home)
            if !home.is_empty()
                && path
                    .strip_prefix(&home)
                    .is_some_and(|rest| rest.starts_with('/')) =>
        {
            concat(&["~", &path[home.len()..]])
        }
        _ => path,
    };
    eprint_line(&concat(&[
        "atpkg-keys: wrote ",
        &shown,
        " (",
        &sig.len().to_string(),
        " bytes)",
    ]));
    Ok(())
}

#[cfg(unix)]
use atpkg_keys::provision::{DEFAULT_ROSTER, MACHINE_KEY_REL, MACHINE_PUB_REL, home_path};

/// Current unix seconds, or an error.
#[cfg(unix)]
fn now_unix() -> Result<u64, String> {
    aterm_time::unix_secs().ok_or_else(|| "the system clock reads before the unix epoch".into())
}

/// Print the master's public identity.
#[cfg(unix)]
fn announce_master(seed: &atpkg_keys::master::MasterSeed) -> Result<(), String> {
    print_line(&concat(&["master fingerprint: ", &seed.fingerprint()?]));
    print_line(&concat(&["master public key:  ", &seed.pubkey_b64()?]));
    Ok(())
}

/// This Mac's vault paths, where the master phrase is kept.
#[cfg(unix)]
fn vault() -> Result<aterm_secrets::client::Paths, String> {
    aterm_secrets::client::Paths::locate().map_err(|e| e.to_string())
}

/// `master-check`: the vault's phrase derives a master this build pins.
#[cfg(unix)]
fn master_check() -> Result<(), String> {
    atpkg_keys::master::forbid_core_dumps();
    let vault = vault()?;
    let seed = atpkg_keys::master::from_vault(&vault)?.seed();
    announce_master(&seed)?;
    if aterm_update_core::pins::PAPER_MASTER_PUBKEYS.contains(&seed.pubkey_b64()?.as_str()) {
        Ok(())
    } else {
        Err(aterm_secrets::paper_master::refused(
            &vault,
            "it derives no master this build pins (aterm_update_core::pins::PAPER_MASTER_PUBKEYS)",
        ))
    }
}

/// The flags `setup` and `join` read.
#[cfg(unix)]
const PROVISION_FLAGS: &[&str] = &["id", "pins", "roster", "key"];

/// What `setup` says when it fails before the vault took a phrase: no master exists.
#[cfg(unix)]
const SETUP_ARMED_NOTHING: &str = "atpkg-keys: no file was written and the vault holds no \
     new phrase: no master exists as a result of this run. Fix the problem above and run \
     `atpkg-keys setup` again.";

/// What `setup` says when the vault took the phrase but this run did not write the anchor; the
/// recovery, [`atpkg_keys::provision::SETUP_START_OVER`], follows it.
#[cfg(unix)]
const SETUP_VAULT_ARMS_NOTHING: &str = "atpkg-keys: the vault's new release-paper-master arms \
     nothing: the anchor file was not written. ";

/// `setup` mints the paper master into the vault and `join` checks the vault's; both mint this
/// machine's signing key and add it to the roster.
#[cfg(unix)]
fn provision(verb: atpkg_keys::provision::Verb, argv: &Argv) -> Result<(), String> {
    use atpkg_keys::provision as prov;

    atpkg_keys::master::forbid_core_dumps();

    let id = flag(argv, "id").ok_or_else(|| {
        concat(&[
            "usage: atpkg-keys ",
            verb.name(),
            " --id <machine-id> [--pins <path>] [--roster <path>] [--key <path>]",
        ])
    })?;
    let now = now_unix()?;
    let (pins, pins_explicit) = match flag(argv, "pins") {
        Some(p) => {
            let explicit = match (
                std::fs::canonicalize(p),
                prov::discover_pins_path().map(std::fs::canonicalize),
            ) {
                (Ok(given), Ok(Ok(discovered))) => given != discovered,
                _ => true,
            };
            (p.to_string(), explicit)
        }
        None => (prov::discover_pins_path()?, false),
    };
    let paths = prov::Paths {
        pins,
        pins_explicit,
        roster: flag(argv, "roster").unwrap_or(DEFAULT_ROSTER).to_string(),
        key: match flag(argv, "key") {
            Some(p) => p.to_string(),
            None => home_path(MACHINE_KEY_REL)?,
        },
        machine_pub: home_path(MACHINE_PUB_REL)?,
    };

    let pre = prov::preflight(verb, id, &paths)?;

    let report = match verb {
        prov::Verb::Setup => {
            let unarmed = |e: String| concat(&[&e, "\n", SETUP_ARMED_NOTHING]);
            let vault = vault().map_err(unarmed)?;
            let phrase = atpkg_keys::master::generate_master().map_err(unarmed)?;
            let seed = phrase.seed();
            let planned = prov::plan(pre, &seed, now).map_err(unarmed)?;
            phrase.store_in_vault(&vault).map_err(unarmed)?;

            prov::write_pins(&planned).map_err(|e| {
                if e.landed {
                    concat(&[
                        &e.message,
                        "\natpkg-keys: the anchor file may now name the vault's new master: \
                         `git checkout -- ",
                        &paths.pins,
                        "` first.\n",
                        SETUP_VAULT_ARMS_NOTHING,
                        prov::SETUP_START_OVER,
                    ])
                } else {
                    concat(&[
                        &e.message,
                        "\n",
                        SETUP_VAULT_ARMS_NOTHING,
                        prov::SETUP_START_OVER,
                    ])
                }
            })?;
            announce_master(&seed)?;
            prov::write_rest(planned)?
        }
        prov::Verb::Join => {
            let vault = vault()?;
            let seed = atpkg_keys::master::from_vault(&vault)?.seed();
            announce_master(&seed)?;
            prov::verify_master(&pre, &seed).map_err(|e| {
                concat(&[
                    &e,
                    "\n",
                    &aterm_secrets::paper_master::refused(&vault, "it derives another master"),
                ])
            })?;
            let planned = prov::plan(pre, &seed, now)?;
            prov::write_pins(&planned)?;
            prov::write_rest(planned)?
        }
    };

    for line in prov::render_report(&report) {
        print_line(&line);
    }
    Ok(())
}

/// Read the roster at `path`, which must exist, and verify it under `master_pubkey`.
#[cfg(unix)]
fn load_roster(
    path: &str,
    master_pubkey: &str,
    now: u64,
) -> Result<
    (
        aterm_update_core::roster::Roster,
        Option<atpkg_keys::provision::RosterSnapshot>,
    ),
    String,
> {
    atpkg_keys::provision::load_roster(
        path,
        master_pubkey,
        now,
        atpkg_keys::provision::RosterExpectation::MustExist,
    )
}

/// Sign and publish the roster and `.sig` via [`atpkg_keys::provision::publish_roster_locked`].
#[cfg(unix)]
fn publish_roster(
    lock: &atpkg_keys::provision::RosterLock,
    snapshot: Option<&atpkg_keys::provision::RosterSnapshot>,
    path: &str,
    roster: &aterm_update_core::roster::Roster,
    seed: &atpkg_keys::master::MasterSeed,
) -> Result<(), String> {
    let text = roster.to_toml().map_err(|e| {
        concat(&[
            "refusing to emit an invalid roster (",
            &atpkg_keys::provision::reject_name(&e),
            ")",
        ])
    })?;
    let bytes = text.into_bytes();
    let sig = seed.sign(&bytes)?;
    atpkg_keys::provision::publish_roster_locked(lock, path, snapshot, &bytes, &sig)?;
    print_line(&concat(&["wrote ", path, " and ", path, ".sig"]));
    Ok(())
}

/// Print the roster's public contents.
#[cfg(unix)]
fn show_roster(r: &aterm_update_core::roster::Roster) {
    print_line(&concat(&["roster_seq:  ", &r.roster_seq.to_string()]));
    print_line(&concat(&["valid_until: ", &r.valid_until]));
    for m in &r.machines {
        print_line(&concat(&[
            "  machine ",
            &m.id,
            "  ",
            &m.pubkey,
            "  added ",
            &m.added_at,
        ]));
    }
    for id in &r.revoked {
        print_line(&concat(&["  REVOKED ", id]));
    }
}

/// `machine-revoke --id <id> [--roster <path>]`.
#[cfg(unix)]
fn machine_revoke(argv: &Argv) -> Result<(), String> {
    atpkg_keys::master::forbid_core_dumps();
    let id = flag(argv, "id")
        .ok_or("usage: atpkg-keys machine-revoke --id <machine-id> [--roster <path>]")?;
    let roster_path = flag(argv, "roster").unwrap_or(DEFAULT_ROSTER);
    let now = now_unix()?;
    if matches!(std::fs::symlink_metadata(roster_path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        && atpkg_keys::provision::refuse_pending_roster_transaction(roster_path).is_ok()
    {
        return Err(concat(&[
            "no roster at ",
            roster_path,
            " — copy aterm-machines.toml and its .sig there from a machine that has them \
             or from the latest release, or pass --roster <path>",
        ]));
    }

    let seed = atpkg_keys::master::from_vault(&vault()?)?.seed();
    announce_master(&seed)?;
    let roster_lock = atpkg_keys::provision::lock_roster(roster_path)?;
    let (roster, snapshot) = load_roster(roster_path, &seed.pubkey_b64()?, now)?;
    let roster = atpkg_keys::roster_ops::revoke(roster, id, now)?;
    publish_roster(&roster_lock, snapshot.as_ref(), roster_path, &roster, &seed)?;
    show_roster(&roster);
    eprint_line(&concat(&[
        "atpkg-keys: '",
        id,
        "' is revoked. Publish the roster and its .sig with the next release; a running \
         aterm refuses that machine from its next update check after that. A fresh install \
         can still be served an older master-signed roster that lists it — if that \
         matters, re-key under a new master.",
    ]));
    Ok(())
}
