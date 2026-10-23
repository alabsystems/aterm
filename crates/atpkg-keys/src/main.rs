// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-keys`: owner-only Ed25519 CLI for manifest signing and roster ceremonies.

#[cfg(unix)]
use atpkg_keys::fsio::{concat, read_bytes, write_bytes};
#[cfg(unix)]
use std::process::ExitCode;

#[cfg(windows)]
fn main() {
    eprintln!("atpkg-keys: unsupported on Windows (POSIX-only owner signing tool)");
    std::process::exit(2);
}

/// Maximum tokens after the verb (`setup`/`join` take up to 8; bounded so argument parsing
/// uses a fixed array rather than `collect()`).
#[cfg(unix)]
const MAX_ARGS: usize = 14;

/// Bounded argument vector (`None` from the first unset slot onwards).
#[cfg(unix)]
type Argv = [Option<String>; MAX_ARGS];

/// Value following `--<name>`, or `None` (two-token `--name value` form; [`vet_args`] runs first).
#[cfg(unix)]
fn flag<'a>(argv: &'a Argv, name: &str) -> Option<&'a str> {
    let mut want = String::from("--");
    want.push_str(name);
    for i in 0..MAX_ARGS {
        if argv[i].as_deref() == Some(want.as_str()) && i + 1 < MAX_ARGS {
            return argv[i + 1].as_deref();
        }
    }
    None
}

/// Flag names that would carry the master key on argv, refused with a compromise warning.
#[cfg(unix)]
const MASTER_FLAGS: &[&str] = &[
    "master",
    "master-file",
    "phrase",
    "phrase-file",
    "seed",
    "paper",
    "secret",
];

/// Refuse any argument shape outside `allowed` flags and `max_positional` bare arguments.
#[cfg(unix)]
fn vet_args(
    verb: &str,
    argv: &Argv,
    allowed: &[&str],
    max_positional: usize,
) -> Result<(), String> {
    let mut positional = 0usize;
    let mut i = 0usize;
    while i < MAX_ARGS {
        let Some(tok) = argv[i].as_deref() else {
            return Ok(());
        };
        if !tok.starts_with("--") {
            positional += 1;
            if positional > max_positional {
                return Err(concat(&[
                    "unexpected argument '",
                    tok,
                    "' for `atpkg-keys ",
                    verb,
                    "`",
                ]));
            }
            i += 1;
            continue;
        }
        let name = tok.get(2..).unwrap_or("");
        if let Some(eq) = name.find('=') {
            return Err(concat(&[
                "'",
                tok,
                "' uses the `--name=value` form; write it as two tokens: `--",
                name.get(..eq).unwrap_or(""),
                " <value>`",
            ]));
        }
        if MASTER_FLAGS.contains(&name) {
            return Err(concat(&[
                "'",
                tok,
                "' does not exist: the master phrase is read only from /dev/tty. If you \
                 typed your real phrase on this command line, TREAT THAT MASTER AS \
                 COMPROMISED (it is in shell history and was visible via `ps`): destroy \
                 the paper and generate a new master on a tree whose anchor is not armed.",
            ]));
        }
        if !allowed.contains(&name) {
            let mut msg = concat(&["unknown flag '", tok, "' for `atpkg-keys ", verb, "`"]);
            for (n, a) in allowed.iter().enumerate() {
                msg.push_str(if n == 0 { " (it accepts " } else { ", " });
                msg.push_str("--");
                msg.push_str(a);
            }
            if !allowed.is_empty() {
                msg.push(')');
            }
            return Err(msg);
        }
        match argv.get(i + 1).and_then(|v| v.as_deref()) {
            None => {
                return Err(concat(&["'", tok, "' needs a value after it"]));
            }
            Some(v) if v.starts_with("--") => {
                return Err(concat(&[
                    "'",
                    tok,
                    "' was given '",
                    v,
                    "' as its value, which is another flag",
                ]));
            }
            Some(_) => {}
        }
        i += 2;
    }
    Ok(())
}

/// The `i`th positional argument (skipping `--flag value` pairs), 0-based after the verb.
#[cfg(unix)]
fn positional(argv: &Argv, want: usize) -> Option<&str> {
    let mut seen = 0usize;
    let mut i = 0usize;
    while i < MAX_ARGS {
        match argv[i].as_deref() {
            None => return None,
            Some(a) if a.starts_with("--") => i += 2,
            Some(a) => {
                if seen == want {
                    return Some(a);
                }
                seen += 1;
                i += 1;
            }
        }
    }
    None
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
    let mut buf = String::new();
    buf.push_str(line);
    buf.push('\n');
    if std::io::stdout().write_all(buf.as_bytes()).is_err() {
        std::panic::panic_any("failed printing to stdout");
    }
}

/// `eprintln!`-equivalent without the fmt machinery (see `print_line`).
#[cfg(unix)]
fn eprint_line(line: &str) {
    use std::io::Write as _;
    let mut buf = String::new();
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
    let key = key.ok_or("usage: atpkg-keys pubkey <key-file>")?;
    print_line(&atpkg_keys::pubkey_b64(&read_key(key)?)?);
    Ok(())
}

/// `verify-signed`.
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
    let file_bytes = atpkg_keys::fsio::read_bytes_up_to(file, 64 * 1024 * 1024)
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
    let key = key.ok_or("usage: atpkg-keys sign <key-file> <file> [<sig-out>]")?;
    let msg = msg.ok_or("usage: atpkg-keys sign <key-file> <file> [<sig-out>]")?;
    let bytes = read_bytes(msg).map_err(|e| concat(&["read ", msg, ": ", &e.to_string()]))?;
    let sig = atpkg_keys::sign(&read_key(key)?, &bytes)?;
    let path = match sig_out {
        Some(path) => path.to_string(),
        None => concat(&[msg, ".sig"]),
    };
    write_bytes(&path, &sig).map_err(|e| concat(&["write ", &path, ": ", &e.to_string()]))?;
    let shown = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            concat(&["~", &path[home.len()..]])
        }
        _ => path.clone(),
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
    aterm_time::unix_secs().ok_or_else(|| {
        "the system clock is before the unix epoch; refusing to stamp a roster".into()
    })
}

/// Print the master's public identity.
#[cfg(unix)]
fn announce_master(seed: &atpkg_keys::master::MasterSeed) -> Result<(), String> {
    print_line(&concat(&["master fingerprint: ", &seed.fingerprint()?]));
    print_line(&concat(&["master public key:  ", &seed.pubkey_b64()?]));
    Ok(())
}

/// The heading that must land on the same surface as the phrase itself.
#[cfg(unix)]
const PHRASE_HEADING: &str = "atpkg-keys: MASTER PHRASE — write it on paper, twice, in two \
     places. Shown once. Stored nowhere. Unrecoverable.";

/// The rule that isolates the two lines the paper must carry from everything else on the screen.
#[cfg(unix)]
const PHRASE_RULE: &str = "----------------------------------------------------------------";

/// Show the phrase on the terminal only.
#[cfg(unix)]
fn show_phrase(
    tty: &mut atpkg_keys::master::Tty,
    phrase: &atpkg_keys::master::MasterPhrase,
    seed: &atpkg_keys::master::MasterSeed,
) -> Result<(), String> {
    tty.write_line("")?;
    tty.write_line(PHRASE_HEADING)?;
    tty.write_line(PHRASE_RULE)?;
    tty.write_phrase(phrase)?;
    tty.write_line(&concat(&[
        "fingerprint: ",
        &seed.fingerprint()?,
        "  (write it beside the phrase)",
    ]))?;
    tty.write_line(PHRASE_RULE)?;
    Ok(())
}

/// The transcription gate.
#[cfg(unix)]
fn confirm_transcription(
    tty: &mut atpkg_keys::master::Tty,
    phrase: &atpkg_keys::master::MasterPhrase,
    seed: &atpkg_keys::master::MasterSeed,
) -> Result<(), String> {
    let want = seed.pubkey_b64()?;
    loop {
        let typed = match atpkg_keys::master::prompt_master_attempt(
            "retype the phrase FROM YOUR PAPER (echo off; spaces ignored; Ctrl-C aborts, \
             nothing is armed): ",
        )? {
            Ok(typed) => typed,
            Err(atpkg_keys::master::PhraseError::Length { got: 0 }) => {
                return Err("nothing was typed at the transcription gate".to_string());
            }
            Err(e) => {
                tty.write_line(&concat(&["atpkg-keys: ", &e.message(), " — try again."]))?;
                continue;
            }
        };
        if typed.seed().pubkey_b64()? == want {
            return tty.write_line(
                "atpkg-keys: paper proven. Clear this terminal's scrollback when done.",
            );
        }
        tty.write_line(&concat(&[
            "atpkg-keys: NO MATCH — your paper derives fingerprint ",
            &typed.seed().fingerprint()?,
            ", the master is ",
            &seed.fingerprint()?,
            ". Fix the paper from the line below, then retype.",
        ]))?;
        tty.write_line(PHRASE_RULE)?;
        tty.write_phrase(phrase)?;
        tty.write_line(PHRASE_RULE)?;
    }
}

/// `master-check`: type the phrase back and see the fingerprint again.
#[cfg(unix)]
fn master_check() -> Result<(), String> {
    atpkg_keys::master::forbid_core_dumps();
    let phrase = atpkg_keys::master::prompt_for_master(
        "master phrase (52 characters, echo off; spaces, case, and o/i/l are forgiven): ",
    )?;
    announce_master(&phrase.seed())?;
    eprint_line(
        "atpkg-keys: compare that fingerprint with the one you wrote down. If they \
         differ, you transcribed a character wrong — the phrase itself is fine.",
    );
    Ok(())
}

/// The flags `setup` and `join` read.
#[cfg(unix)]
const PROVISION_FLAGS: &[&str] = &["id", "pins", "roster", "key"];

/// What `setup` says when it fails before showing a phrase: no master exists, so try again.
#[cfg(unix)]
const SETUP_ARMED_NOTHING: &str = "atpkg-keys: nothing was written and no phrase was \
     shown — no master exists as a result of this run. Fix the problem above and run \
     `atpkg-keys setup` again.";

/// What `setup` says when the phrase was shown but the anchor not written.
#[cfg(unix)]
const SETUP_PAPER_ARMS_NOTHING: &str = "atpkg-keys: THE PHRASE ABOVE ARMS NOTHING. The \
     anchor file was not written, so the master you just saw is not referenced by anything \
     and never will be. DESTROY that paper, fix the problem above, and run `atpkg-keys \
     setup` again — it will generate a different master.";

/// `setup` / `join`: provision the paper master and a machine's signing key.
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
    let paths = prov::Paths {
        pins: match flag(argv, "pins") {
            Some(p) => p.to_string(),
            None => prov::discover_pins_path()?,
        },
        pins_explicit: match flag(argv, "pins") {
            None => false,
            Some(p) => match (
                std::fs::canonicalize(p),
                prov::discover_pins_path().map(std::fs::canonicalize),
            ) {
                (Ok(given), Ok(Ok(discovered))) => given != discovered,
                _ => true,
            },
        },
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
            let mut tty = atpkg_keys::master::Tty::open()
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;
            let phrase = atpkg_keys::master::generate_master()
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;
            let seed = phrase.seed();
            let planned = prov::plan(pre, &seed, now)
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;

            show_phrase(&mut tty, &phrase, &seed)
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;
            confirm_transcription(&mut tty, &phrase, &seed)
                .map_err(|e| concat(&[&e, "\n", SETUP_PAPER_ARMS_NOTHING]))?;

            prov::write_pins(&planned)
                .map_err(|e| concat(&[&e, "\n", SETUP_PAPER_ARMS_NOTHING]))?;
            announce_master(&seed)?;
            prov::write_rest(planned)?
        }
        prov::Verb::Join => {
            let phrase = atpkg_keys::master::prompt_for_master(
                "master phrase (52 characters, echo off; spaces, case, and o/i/l are forgiven): ",
            )?;
            let seed = phrase.seed();
            announce_master(&seed)?;
            prov::verify_master(&pre, &seed)?;
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

/// Read, verify under `master_pubkey` ([`aterm_update_core::roster::Roster`]).
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
    if matches!(std::fs::metadata(roster_path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        && atpkg_keys::provision::refuse_pending_roster_transaction(roster_path).is_ok()
    {
        return Err(concat(&[
            "no roster at ",
            roster_path,
            " — copy aterm-machines.toml and its .sig there from a machine that has them \
             or from the latest release, or pass --roster <path>",
        ]));
    }

    let phrase = atpkg_keys::master::prompt_for_master(
        "master phrase (52 characters, echo off; spaces, case, and o/i/l are forgiven): ",
    )?;
    let seed = phrase.seed();
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
         aterm refuses that machine at its first update check after that (every 10 \
         minutes on macOS). A fresh install can still be served an older master-signed \
         roster that lists it — if that matters for this theft, re-key entirely.",
    ]));
    Ok(())
}
