// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-keys`: the owner-only Ed25519 CLI for atpkg manifest signing and the paper master /
//! machine roster the release channel runs on.
//!
//! ## Signing plumbing (used by the publish scripts, not by hand)
//!
//!   atpkg-keys pubkey <key-file>            print the base64 pubkey of an existing key
//!   atpkg-keys sign   <key-file> <file> [<sig-out>]   detached-sign <file>'s exact bytes
//!   atpkg-keys verify-signed <master-pubkey-b64> <file> <file.sig> <roster> <roster.sig>
//!                                           did a live machine of that roster sign <file>?
//!
//! `pubkey` and `sign` take the machine key `setup`/`join` minted (`~/.aterm/machine.key`).
//! `verify-signed` checks a file that is not an atpkg document (an ALab release's
//! `SHA256SUMS`) with the client's own roster code. There is no standalone key generator:
//! every signing key is born in provisioning, so the roster accounts for every key.
//!
//! ## Arming the channel
//!
//!   atpkg-keys setup --id <id>   first machine, once ever
//!   atpkg-keys join  --id <id>   every later machine
//!   (both also take [--pins <path>] [--roster <path>] [--key <path>]: the anchor file, the
//!   roster and the machine key they write)
//!
//! `setup` generates the paper master, shows the 52 characters once on `/dev/tty`, writes the
//! master's public key into `pins::PAPER_MASTER_PUBKEYS`, mints this machine's keypair to a
//! `0600` file in `$HOME`, and creates the master-signed roster naming this machine. `join`
//! does the same on a later machine after proving the typed phrase against the committed
//! anchor and the existing roster (a release asset, copied to the machine first). The roster
//! is the only grant, and `machine-revoke` takes it back; no verb edits any anchor but the
//! master's. Neither verb commits or pushes: they edit the working tree, read back what they
//! wrote, and print a `review: git diff -- <pins>` line when the anchor file changed.
//!
//! ## Other ceremony verbs
//!
//!   atpkg-keys master-check                 type the phrase back; print the fingerprint again
//!   atpkg-keys machine-revoke --id <id> [--roster <p>]
//!
//! The master is read only from `/dev/tty` with echo off: never argv, an environment variable,
//! stdin or a file. `--master` and its family are refused by name, with a warning that a
//! phrase typed on a command line is compromised. The phrase `setup` shows goes to the same
//! `/dev/tty` handle, so `setup > log` cannot capture it and `> /dev/null` cannot swallow it.
//! Every unread argument is refused too: a silently dropped flag would leave a default naming
//! a trust-anchor file or a secret key. `join` and `machine-revoke` verify the existing roster
//! under the typed master before editing it, so a mistyped phrase is refused before anything
//! is written. `master.rs` has the leak-vector inventory.

#[cfg(unix)]
use atpkg_keys::fsio::{concat, read_bytes, write_bytes};
#[cfg(unix)]
use std::process::ExitCode;

// POSIX-only owner tool (see lib.rs).
#[cfg(windows)]
fn main() {
    eprintln!("atpkg-keys: unsupported on Windows (POSIX-only owner signing tool)");
    std::process::exit(2);
}

/// The most arguments any verb takes, counting flags and values. A fixed bound keeps the
/// argument pull a fixed number of `next()` calls (`collect()` is an allocation Trust cannot
/// bound); `setup` and `join` take eight tokens, and `main` refuses anything past the bound.
#[cfg(unix)]
const MAX_ARGS: usize = 14;

/// The bounded argument vector. `None` from the first unset slot onwards.
#[cfg(unix)]
type Argv = [Option<String>; MAX_ARGS];

/// The value following `--<name>`, or `None`: the only way this CLI reads a named option,
/// never used for anything secret (argv is visible in `ps`). It sees only the two-token
/// `--name value` form, so [`vet_args`] must run first.
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

/// Flag names that would carry the master (leak vector 1), refused by name with a warning
/// rather than ignored: whoever typed one must learn the phrase is now in `ps` and history.
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

/// Refuse every argument shape this CLI does not read. [`flag`] sees only `--name value`;
/// anything skipped silently would fall back to a default the operator never saw, so
/// `setup --pins=<other>` would arm whichever checkout they stood in.
///
/// `allowed` lists the flags the verb reads; `max_positional` bounds its bare arguments.
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
    // Anything past the bounded vector would be dropped silently: refuse it instead.
    let overflowed = it.next().is_some();

    let r = if overflowed {
        Err(concat(&[
            "too many arguments — `atpkg-keys` reads at most ",
            &MAX_ARGS.to_string(),
            " after the verb",
        ]))
    } else {
        // Per verb, not pooled, so a flag added to one verb cannot be silently accepted by
        // another that ignores it.
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

/// `println!` without the fmt machinery: one write of `line` + '\n', panicking on failure
/// like `println!`.
///
/// `panic_any` is unsupported MIR for the Trust verifier; moving it into a local leaf only
/// moves the obligation to that call, and an inline `panic!` is a charged assert edge, so the
/// direct call is the minimal shape.
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

/// `verify-signed`: did a live machine of a roster that verifies under that master sign this
/// file's exact bytes? Prints `OK: signed by machine <id> under roster seq <n>`. Any
/// verification failure is one opaque `FAIL:` line (no oracle), an unreadable input names its
/// path, and every refusal exits 1.
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
    // A whole source tree's SHA256SUMS (see `fsio::read_bytes_up_to`).
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
        // Default: <file>.sig next to the input.
        None => concat(&[msg, ".sig"]),
    };
    write_bytes(&path, &sig).map_err(|e| concat(&["write ", &path, ": ", &e.to_string()]))?;
    // Collapse `$HOME` to `~`: publish transcripts can sit beside public releases, and an
    // absolute path names this machine's account (atpkg-publish's `producer_to_client` pins it).
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

// The paper master and the machine roster. The master is read from /dev/tty, used, and
// scrubbed: never a parameter, never written, never printed after the one moment at
// generation when the owner must read it (`master.rs`).

#[cfg(unix)]
use atpkg_keys::provision::{DEFAULT_ROSTER, MACHINE_KEY_REL, MACHINE_PUB_REL, home_path};

/// Current unix seconds, or an error: every timestamp this tool writes is load-bearing
/// (`valid_until` decides whether clients accept the roster), so a broken clock stops the mint.
#[cfg(unix)]
fn now_unix() -> Result<u64, String> {
    aterm_time::unix_secs().ok_or_else(|| {
        "the system clock is before the unix epoch; refusing to stamp a roster".into()
    })
}

/// Print the master's public identity: the fingerprint the owner eyeballs against paper,
/// and the base64 public key that goes into `pins::PAPER_MASTER_PUBKEYS`.
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

/// The rule that isolates the two lines the paper must carry from everything else on the
/// screen, so the phrase cannot read as one unmarked line in a report.
#[cfg(unix)]
const PHRASE_RULE: &str = "----------------------------------------------------------------";

/// Show the phrase on the terminal only: heading, 52 characters and fingerprint all go to the
/// one `/dev/tty` handle, where no redirect can capture or swallow them.
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

/// The transcription gate: nothing is armed until the phrase is retyped from the paper, since
/// a shown-but-never-copied master would arm a tier nobody can extend or revoke. A mismatch
/// re-shows the phrase and asks again; an abort (Ctrl-C/EOF) arms nothing.
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
            // EOF presents as an empty attempt: abort, or a closed tty would loop forever.
            Err(atpkg_keys::master::PhraseError::Length { got: 0 }) => {
                return Err("nothing was typed at the transcription gate".to_string());
            }
            // A typo is not an abort: say what was wrong and ask again.
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

/// `master-check`: type the phrase back and see the fingerprint again. Run it while the
/// terminal still shows the original, so a bad transcription is still fixable.
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

/// What `setup` says when the phrase was shown but the anchor not written: the paper arms
/// nothing and must be destroyed, not filed.
#[cfg(unix)]
const SETUP_PAPER_ARMS_NOTHING: &str = "atpkg-keys: THE PHRASE ABOVE ARMS NOTHING. The \
     anchor file was not written, so the master you just saw is not referenced by anything \
     and never will be. DESTROY that paper, fix the problem above, and run `atpkg-keys \
     setup` again — it will generate a different master.";

/// `setup` / `join`: the two verbs whose only human step is writing the phrase on paper.
///
/// For `setup` the order is the safety argument:
///
/// 1. Open the terminal before a master exists: a master with nowhere to go must never be
///    generated.
/// 2. Generate and plan. Everything that can fail on the state of the tree fails here, in
///    memory, with nothing shown or written ([`SETUP_ARMED_NOTHING`]).
/// 3. Show the phrase (the flush is checked), then prove the paper by retyping.
/// 4. Arm the anchor.
///
/// Paper first, anchor second: an anchor naming a master nobody holds is unrecoverable, a
/// phrase that arms nothing is merely wasteful. That holds only because delivery is
/// established (`/dev/tty` cannot be redirected and its flush is checked). A failure between
/// the two says to destroy the paper ([`SETUP_PAPER_ARMS_NOTHING`]).
///
/// `join` reads the phrase, proves it against the committed anchor and against the existing
/// roster the real master signed, and only then writes anything.
#[cfg(unix)]
fn provision(verb: atpkg_keys::provision::Verb, argv: &Argv) -> Result<(), String> {
    use atpkg_keys::provision as prov;

    // Before anything sensitive exists in this process (leak vector 7).
    atpkg_keys::master::forbid_core_dumps();

    let id = flag(argv, "id")
        .ok_or_else(|| concat(&["usage: atpkg-keys ", verb.name(), " --id <machine-id>"]))?;
    let now = now_unix()?;
    let paths = prov::Paths {
        // Discovered unless given: a mistyped `--pins` is another transcription risk.
        pins: match flag(argv, "pins") {
            Some(p) => p.to_string(),
            None => prov::discover_pins_path()?,
        },
        // An explicit path names another tree, discovery names this one: the distinction
        // `preflight`'s compiled-anchor refusal turns on. A path that canonicalizes to the
        // discovered anchor (directly or by symlink) counts as discovery.
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

    // Every refusal that can be made before a secret exists is made here.
    let pre = prov::preflight(verb, id, &paths)?;

    let report = match verb {
        prov::Verb::Setup => {
            // (1) Somewhere to deliver it, before there is anything to deliver.
            let mut tty = atpkg_keys::master::Tty::open()
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;
            // (2) Generate and plan; still nothing shown or written.
            let phrase = atpkg_keys::master::generate_master()
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;
            let seed = phrase.seed();
            let planned = prov::plan(pre, &seed, now)
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;

            // (3) The one place this tree deliberately emits a secret: the owner cannot write
            // down what they cannot see. It goes to the terminal, and the write is checked.
            show_phrase(&mut tty, &phrase, &seed)
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;

            // (3b) Prove the paper: it is retyped before anything is armed.
            confirm_transcription(&mut tty, &phrase, &seed)
                .map_err(|e| concat(&[&e, "\n", SETUP_PAPER_ARMS_NOTHING]))?;

            // (4) Arm the anchor. From here the paper is load-bearing.
            prov::write_pins(&planned)
                .map_err(|e| concat(&[&e, "\n", SETUP_PAPER_ARMS_NOTHING]))?;
            announce_master(&seed)?;

            // Its failures state the recovery: start over when nothing authorizes this
            // machine, keep the key when the roster is recoverable.
            prov::write_rest(planned)?
        }
        prov::Verb::Join => {
            let phrase = atpkg_keys::master::prompt_for_master(
                "master phrase (52 characters, echo off; spaces, case, and o/i/l are forgiven): ",
            )?;
            let seed = phrase.seed();
            announce_master(&seed)?;
            // Proof one: the anchor already committed names this master. Proof two, in
            // `plan`: the existing roster verifies under it. Both precede every write.
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

/// Read, verify under `master_pubkey`, and parse the roster: the CLI skin over
/// [`atpkg_keys::provision::load_roster`]. There is no unverified parse path, and verifying is
/// also the transcription check. The one caller, `machine-revoke`, refuses an absent roster
/// (`MustExist`).
///
/// The byte snapshot comes back too: it is the compare-and-swap premise checked just before
/// the write, and the predecessor a crash-recovery replay may overwrite.
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

/// Emit the roster and its detached master signature. The bytes are signed before either
/// file is written, and [`atpkg_keys::provision::publish_roster_locked`] commits a durable
/// redo directory first, so a crash between the two renames is completed forward by the next
/// run that takes the roster lock.
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
    // The master's whole residency window closes here: sign, and drop it.
    let sig = seed.sign(&bytes)?;
    atpkg_keys::provision::publish_roster_locked(lock, path, snapshot, &bytes, &sig)?;
    print_line(&concat(&["wrote ", path, " and ", path, ".sig"]));
    Ok(())
}

/// Print the roster's public contents — the human-readable half of attribution.
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

/// `machine-revoke --id <id>`: withdraw a machine's authority. Only the paper master signs
/// rosters, so a thief holding a machine key cannot undo it, and it runs from any surviving
/// machine. It does not un-install: what a validly signed malicious release already staged is
/// for `targo --unverified ship yank` / `min_build`.
#[cfg(unix)]
fn machine_revoke(argv: &Argv) -> Result<(), String> {
    atpkg_keys::master::forbid_core_dumps();
    let id = flag(argv, "id").ok_or("usage: atpkg-keys machine-revoke --id <machine-id>")?;
    let roster_path = flag(argv, "roster").unwrap_or(DEFAULT_ROSTER);
    let now = now_unix()?;
    // Nothing to revoke from: say so before the phrase is typed. An interrupted roster write
    // is not nothing (the lock below completes it forward), nor is an unreadable roster (the
    // lock and the load name that error).
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
    // One lock across read, edit and publish, the same one `setup`/`join` take, so a revoke
    // and a join cannot each publish an N+1 that de-authorizes the other's machine. The
    // kernel releases it if this process dies.
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
