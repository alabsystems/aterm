// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-keys` — the owner-only Ed25519 CLI: atpkg manifest signing (§12), and the
//! **paper master / machine roster** the release channel runs on.
//!
//! ## Signing plumbing (used by the publish scripts, not by hand)
//!
//!   atpkg-keys pubkey <key-file>            print the base64 pubkey of an existing key
//!   atpkg-keys sign   <key-file> <file> [<sig-out>]   detached-sign <file>'s exact bytes
//!
//! Both operate on the machine key `setup`/`join` minted (`~/.aterm/machine.key`); the
//! detached signature is exactly what the verify-only client (`atpkg::sig`) checks.
//! There is deliberately NO standalone key generator any more: every signing identity is
//! a machine key born in provisioning, so a key that exists is a key the roster can
//! account for. (`keygen` was that generator; it made keys the roster had never heard
//! of, which is exactly the kind of stray authority this tool exists to end.)
//!
//! ## Arming the channel — the two verbs the owner actually runs
//!
//!   atpkg-keys setup --id <id> [--head-id <id>]   FIRST machine, once ever
//!   atpkg-keys join  --id <id>                    EVERY later machine
//!
//! `setup` generates the paper master, shows the 52 characters ONCE **on the terminal**
//! (`/dev/tty`, never stdout — see below), and then does every remaining step itself: it
//! writes the master's public key into `pins::PAPER_MASTER_PUBKEYS`, mints this machine's
//! keypair to a `0600` file in `$HOME`, and creates the master-signed roster. That roster
//! names the INCUMBENT keyset head first (`--head-id`, default `incumbent-head`) and this
//! machine second, because arming the master changes how a cut is authorized and a roster
//! without the incumbent would leave the one machine every PRE-ROSTER client can verify
//! unable to cut. `join` does the same remaining work on a later machine, after PROVING
//! the phrase you type against the committed anchor and the existing roster — which must
//! be copied to that machine first, since the roster is a release asset and not part of
//! the repository.
//!
//! **Neither verb touches `pins::UPDATE_CHANNEL_PUBKEYS`.** A minted machine is authorized
//! by the roster, which is revocable; a keyset entry would be a grant no revocation can
//! reach, made to clients this tool cannot influence anyway. See `provision`'s module doc.
//!
//! **The only thing a human does is write the phrase on paper.** There is no step that
//! asks anyone to copy a key into a source file, because that is the transcription this
//! pair of verbs exists to delete.
//!
//! Neither verb commits and neither pushes. Arming a trust anchor is a reviewed act, so
//! they edit the working tree, verify what they wrote by reading it back, and print the
//! diff to review plus exactly what is — and is not — true afterwards.
//!
//! ## The remaining ceremony verbs
//!
//!   atpkg-keys master-check                 type the phrase back; print the fingerprint again
//!   atpkg-keys machine-revoke --id <id> [--roster <p>]
//!
//! That is the WHOLE surface: provision with `setup`/`join`, revoke with
//! `machine-revoke`, check a transcription with `master-check`, and let the publish
//! scripts drive `pubkey`/`sign`. The one-per-act rule is deliberate — this tool used to
//! carry a second generator (`master-new`), a second minting path (`machine-mint`) and a
//! bare roster checker (`roster-verify`), each a way to do half a provisioning by hand
//! and get the other half wrong. All three are deleted, not hidden: `setup`/`join` are
//! the halves welded together, and `atpkg verify-index` already proves a roster as part
//! of proving the thing the roster exists for.
//!
//! **The master is never an argument.** It is read only from `/dev/tty`, with echo off —
//! never from argv (world-readable via `ps`), never from an environment variable, never
//! from stdin, never from a file. There is no flag that takes it and there must never be
//! one; `--master` and the rest of that family are refused BY NAME, with a warning that a
//! phrase typed on a command line must now be treated as compromised, because accepting
//! and ignoring one is the response that leaves the operator believing nothing happened.
//!
//! **And it is never written anywhere except that same terminal.** The one deliberate
//! emission — `setup` showing a freshly generated phrase — goes to the
//! `/dev/tty` handle, not to stdout. `atpkg-keys setup > setup.log` therefore cannot put a
//! master into a file, and `atpkg-keys setup > /dev/null` cannot destroy one while
//! reporting success. Every unread argument is refused for the same reason: a flag this
//! tool silently drops is one whose value is replaced by a default naming a trust-anchor
//! file or a secret key. See `master.rs` for the full leak-vector inventory.
//!
//! `join` and `machine-revoke` VERIFY the existing roster under the master you
//! just typed before they will edit it. That is the automatic transcription check: a
//! mistyped phrase derives a different master, fails to verify a roster its real master
//! signed, and is refused before anything is written — so a typo cannot quietly produce a
//! roster no client will accept.

#[cfg(unix)]
use atpkg_keys::fsio::{concat, read_bytes, write_bytes};
#[cfg(unix)]
use std::process::ExitCode;

// POSIX-only owner tool (see lib.rs); the Windows binary honestly refuses.
#[cfg(windows)]
fn main() {
    eprintln!("atpkg-keys: unsupported on Windows (POSIX-only owner signing tool)");
    std::process::exit(2);
}

/// The most arguments any verb takes, counting flags and their values. Bounded so the
/// argument pull stays a fixed number of `next()` calls: `collect()` on an arbitrary-length
/// iterator is an unbounded allocation Trust cannot bound (`count-not-derivable`).
/// (`std::env::args` itself is a hardened compat_observable boundary either way; see the
/// artifact notes in [`atpkg_keys::fsio`].) The widest verb is `setup`, which takes six
/// flags — twelve tokens — so this leaves room and [`vet_args`] refuses anything past it
/// rather than silently dropping it.
#[cfg(unix)]
const MAX_ARGS: usize = 14;

/// The bounded argument vector. `None` from the first unset slot onwards.
#[cfg(unix)]
type Argv = [Option<String>; MAX_ARGS];

/// The value following `--<name>`, or `None`.
///
/// Deliberately the ONLY way this CLI reads a named option, and deliberately used for
/// nothing secret: everything reachable through here is visible in `ps`. It sees only the
/// two-token `--name value` form, which is why [`vet_args`] must run first — an unrecognised
/// spelling that this returns `None` for is a flag the operator believes they passed.
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

/// Flag names that would carry the master, refused by name with a warning rather than
/// ignored.
///
/// None of these exists and none may ever exist (`master.rs`, leak vector 1). They are
/// listed HERE because an operator working from a stale note, or guessing, will type one —
/// and the old parser accepted the tokens, ignored them, and prompted anyway. That is the
/// worst possible response: the run succeeds, so nothing tells the operator that their
/// master is now in `ps` output, in their shell history, and must be treated as burned.
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

/// REFUSE EVERY ARGUMENT SHAPE THIS CLI DOES NOT ACTUALLY READ.
///
/// # Why an unknown flag has to be an error here specifically
///
/// [`flag`] recognises only `--name value`. Everything else — `--name=value`, a misspelling,
/// a flag belonging to another verb — used to be skipped in silence, and the value the
/// operator supplied was replaced by a DEFAULT they never saw. For this tool those defaults
/// are: the `pins.rs` discovered by walking up from the current directory, `dist/` relative
/// to the current directory, and `$HOME/.aterm/machine.key`. So `setup --pins=/path/to/other`
/// armed a trust anchor in whichever checkout the operator happened to be standing in,
/// reporting success, and `join --roster=<the real roster>` extended a different file
/// entirely. Editing the wrong copy of `pins.rs` is the failure this whole component exists
/// to prevent; being handed the right path and ignoring it is the worst way to do it.
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
                    "' — `atpkg-keys ",
                    verb,
                    "` takes no more than ",
                    &max_positional.to_string(),
                    " positional argument(s). Refusing rather than ignoring it: an \
                     argument this tool silently drops is one the operator believes it \
                     read.",
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
                "' uses the `--name=value` form, which this tool does not read. Write it \
                 as two tokens: `--",
                name.get(..eq).unwrap_or(""),
                " <value>`. It is refused rather than ignored because ignoring it \
                 substitutes a default path for the one you supplied, and the defaults \
                 here are a trust-anchor file and a machine key.",
            ]));
        }
        if MASTER_FLAGS.contains(&name) {
            return Err(concat(&[
                "'",
                tok,
                "' does not exist and never will. The master phrase is read from /dev/tty \
                 and from nowhere else — never argv (world-readable via `ps`), never an \
                 environment variable, never stdin, never a file. IF YOU JUST TYPED YOUR \
                 REAL PHRASE ON THIS COMMAND LINE, TREAT THAT MASTER AS COMPROMISED: it is \
                 in your shell history and was visible to every process on this machine \
                 while the command ran. Destroy the paper and generate a new master on a \
                 tree whose anchor is not yet armed.",
            ]));
        }
        if !allowed.contains(&name) {
            let mut msg = concat(&[
                "unknown flag '",
                tok,
                "' for `atpkg-keys ",
                verb,
                "`. It accepts: ",
            ]);
            for (n, a) in allowed.iter().enumerate() {
                if n > 0 {
                    msg.push_str(", ");
                }
                msg.push_str("--");
                msg.push_str(a);
            }
            msg.push_str(
                ". Refusing rather than ignoring it: a flag this tool drops silently is one \
                 whose value gets replaced by a default the operator never chose.",
            );
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
                    "' as its value, which is another flag. Refusing to treat a flag as a \
                     path or an id.",
                ]));
            }
            Some(_) => {}
        }
        i += 2;
    }
    Ok(())
}

/// The `i`th POSITIONAL argument (skipping `--flag value` pairs), 0-based after the verb.
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
    // Anything past the bounded vector would have been dropped without a word. For a tool
    // whose flags name a trust anchor and a secret key, "some of what you typed was
    // discarded" must be an error and not a shrug.
    let overflowed = it.next().is_some();

    /// The flags each verb reads, and how many bare arguments it takes. Written out per
    /// verb rather than pooled, so a flag added to one verb cannot be silently accepted by
    /// another that ignores it.
    #[cfg(unix)]
    fn vetted(verb: &str, argv: &Argv, allowed: &[&str], positionals: usize) -> Result<(), String> {
        vet_args(verb, argv, allowed, positionals)
    }

    let r = if overflowed {
        Err(concat(&[
            "too many arguments — `atpkg-keys` reads at most ",
            &MAX_ARGS.to_string(),
            " after the verb, and refuses rather than silently ignoring the rest",
        ]))
    } else {
        match verb.as_deref() {
            Some("pubkey") => {
                vetted("pubkey", &argv, &[], 1).and_then(|()| pubkey(positional(&argv, 0)))
            }
            Some("sign") => vetted("sign", &argv, &[], 3).and_then(|()| {
                sign(
                    positional(&argv, 0),
                    positional(&argv, 1),
                    positional(&argv, 2),
                )
            }),
            Some("master-check") => {
                vetted("master-check", &argv, &[], 0).and_then(|()| master_check())
            }
            Some("setup") => vetted("setup", &argv, PROVISION_FLAGS, 0)
                .and_then(|()| provision(atpkg_keys::provision::Verb::Setup, &argv)),
            Some("join") => vetted("join", &argv, PROVISION_FLAGS, 0)
                .and_then(|()| provision(atpkg_keys::provision::Verb::Join, &argv)),
            Some("machine-revoke") => vetted("machine-revoke", &argv, &["id", "roster"], 0)
                .and_then(|()| machine_revoke(&argv)),
            Some(other) => Err(concat(&[
                "unknown verb '",
                other,
                "' (try: setup, join, machine-revoke, master-check, pubkey, sign)",
            ])),
            None => Err(
                "usage: atpkg-keys <setup|join|machine-revoke|master-check|pubkey|sign> …"
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

/// `println!`-equivalent without the fmt machinery: one buffered write of `line` + '\n',
/// panicking on failure exactly like `println!`.
///
/// Known Trust L0 artifact: `panic_any` is unsupported MIR for the full verifier
/// (`Call::std::panic::panic_any`), and hoisting it into a cold local leaf does NOT
/// help — the caller then carries an equally-unsupported `Call::<leaf>` obligation
/// (unsupported lowering propagates through local callees), so the direct call is the
/// minimal shape. An inline `panic!` is no better: it is a charged assert edge.
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
    // `~`-collapse before printing: `sign` runs inside publish pipelines whose
    // transcripts can end up beside public releases, and an absolute path names the
    // account on this machine. The signature path is not a secret; the home layout
    // is nobody's business. (This was the one remaining absolute-path print — the
    // producer_to_client suite asserts there is no other.)
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

// ---------------------------------------------------------------------------
// The paper master and the machine roster.
//
// Everything below obeys one rule: the master is read from /dev/tty, used, and
// scrubbed. It is never a parameter, never written, and never printed after the
// single moment at generation when the owner has to read it in order to write it
// down. See `master.rs` for the leak-vector inventory this implements.
// ---------------------------------------------------------------------------

/// Where the roster and its master signature live by default — the release staging dir,
/// so a cut picks them up as assets alongside the appcast. Defined once, in the library,
/// so `setup`/`join` and the publish scripts cannot drift about where a roster lives.
#[cfg(unix)]
use atpkg_keys::provision::{DEFAULT_ROSTER, MACHINE_KEY_REL, MACHINE_PUB_REL};

/// `$HOME/<rel>`, or an error naming what was missing.
#[cfg(unix)]
use atpkg_keys::provision::home_path;

/// Current unix seconds. Fails rather than guessing: every timestamp this tool writes is
/// load-bearing (`valid_until` decides whether clients accept the roster at all), so a
/// broken clock must stop the mint, not produce a window nobody chose.
#[cfg(unix)]
fn now_unix() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| "the system clock is before the unix epoch; refusing to stamp a roster".into())
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
/// screen. The 2026-08-14 ceremony failure was an operator who never realized the phrase
/// was the deliverable: it was one unmarked line, and a report buried it.
#[cfg(unix)]
const PHRASE_RULE: &str = "----------------------------------------------------------------";

/// SHOW THE PHRASE — on the terminal, and only on the terminal.
///
/// Every line of this exchange goes to the same `/dev/tty` handle: the heading, the 52
/// characters, and the fingerprint that pairs with them on the paper. Splitting them across
/// stdout and stderr is how the phrase used to end up in `setup.log` while the warning went
/// to the screen — and how a `> /dev/null` could swallow the master entirely while the tool
/// reported success. `write_phrase` checks its flush, so a caller that is about to arm a
/// trust anchor on the strength of "the owner has seen this" is relying on a claim that was
/// actually tested.
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

/// THE TRANSCRIPTION GATE — nothing is armed until the phrase comes back FROM THE PAPER.
///
/// Added after the 2026-08-14 ceremony failure: the phrase was shown, never copied, and
/// scrolled away — a master nobody transcribed armed a tier nobody could ever extend or
/// revoke. `setup` therefore no longer trusts "the owner has seen this"; it requires "the
/// owner's paper re-derives this". A mismatch re-shows the phrase and asks again — the
/// paper is fixable exactly as long as this loop is alive. Abort (Ctrl-C/EOF) arms nothing.
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
            // EOF presents as an empty attempt: the terminal is gone, abort — a retry
            // loop against a closed tty would spin forever.
            Err(atpkg_keys::master::PhraseError::Length { got: 0 }) => {
                return Err("nothing was typed at the transcription gate".to_string());
            }
            // A malformed typo is not an abort: one missed keystroke must not cost the
            // whole ceremony. Say what was wrong and ask again.
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

/// `master-check` — type the phrase back and see the fingerprint again.
///
/// The step that turns 64 hand-copied characters into something trustworthy. Run it while
/// the terminal still shows the original, so a mismatch is fixable; once the paper is the
/// only copy, a bad transcription is unrecoverable.
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

/// The flags `setup` and `join` read. `--head-id` is only meaningful to `setup` (it names
/// the incumbent on the first roster) but is accepted by both, because refusing it on
/// `join` would be a refusal the operator has to look up rather than a fact they can act
/// on; `join` documents that it ignores it in the closing report's roster line.
#[cfg(unix)]
const PROVISION_FLAGS: &[&str] = &["id", "pins", "roster", "key", "head-id"];

/// What `setup` must say if it cannot arm the anchor. Nothing was shown and nothing was
/// written, so the correct action is simply to try again.
#[cfg(unix)]
const SETUP_ARMED_NOTHING: &str = "atpkg-keys: nothing was written and no phrase was \
     shown — no master exists as a result of this run. Fix the problem above and run \
     `atpkg-keys setup` again.";

/// What `setup` must say when the phrase reached the terminal but the anchor did not reach
/// the file. The paper is real and arms NOTHING, which is recoverable — but only if the
/// operator is told to destroy it rather than filing it.
#[cfg(unix)]
const SETUP_PAPER_ARMS_NOTHING: &str = "atpkg-keys: THE PHRASE ABOVE ARMS NOTHING. The \
     anchor file was not written, so the master you just saw is not referenced by anything \
     and never will be. DESTROY that paper, fix the problem above, and run `atpkg-keys \
     setup` again — it will generate a different master.";

/// What `setup` must say when the anchor is armed but the run did not finish.
///
/// Nothing is committed and no other machine has seen this master, so the clean recovery is
/// to start over rather than to patch the half-state. `join` cannot finish it: `join`
/// requires the master-signed roster this run failed to produce.
#[cfg(unix)]
fn setup_armed_but_unfinished(pins: &str) -> String {
    concat(&[
        "atpkg-keys: THE ANCHOR IS ARMED but this run did not finish, so do NOT commit it \
         in this state. Nothing is committed yet and no other machine has seen this \
         master, so start over cleanly: `git checkout -- ",
        pins,
        "` to discard the armed anchor, destroy the paper you just wrote, fix the problem \
         above, and run `atpkg-keys setup` again.",
    ])
}

/// `setup` / `join` — the two verbs whose only human step is writing the phrase on paper.
///
/// # The order here is the entire safety argument, so it is spelled out
///
/// `setup` does four things that can fail, and their order decides what a failure costs:
///
/// 1. **Open the terminal**, before a master exists. A master with nowhere to be delivered
///    must never be generated — see below.
/// 2. **Generate and PLAN.** Everything that can fail on the state of the tree fails here,
///    in memory, with nothing shown and nothing written ([`SETUP_ARMED_NOTHING`]).
/// 3. **Show the phrase**, and check the flush.
/// 4. **Arm the anchor.**
///
/// Steps 3 and 4 are the pair that used to be the other way round, on the argument that an
/// anchor naming a master nobody holds is unrecoverable while a phrase that arms nothing is
/// merely wasteful. That argument was correct and its premise was not: "printed" meant
/// `write_all` to fd 1, which succeeds just as happily into `/dev/null` as onto a screen,
/// so `setup > log` armed the anchor and then destroyed the master while reporting success.
/// With the phrase going to `/dev/tty` — which cannot be redirected, which must exist, and
/// whose flush is checked — delivery is something this code can actually establish, so the
/// order is now the one whose failure mode is recoverable: paper first, anchor second, and
/// a failure between them says DESTROY THE PAPER ([`SETUP_PAPER_ARMS_NOTHING`]) rather than
/// leaving the operator with a phrase that arms nothing and no way to know it.
///
/// For `join` there is no such tension: the master already exists on paper. The phrase is
/// read, PROVED twice — against the committed anchor and against the existing roster the
/// real master signed — and only then is anything written.
///
/// # The phrase reaches this function from `/dev/tty`, and leaves through it too
///
/// There is no `--master` flag and there must never be one; `master.rs` holds the full
/// leak-vector inventory, and [`vet_args`] now refuses the whole family of spellings by
/// name rather than ignoring them. `atpkg-keys join < phrase.txt` fails loudly in
/// `prompt_for_master`, which opens `/dev/tty` explicitly rather than reading fd 0, and
/// `atpkg-keys setup > log` fails just as loudly rather than writing the master into `log`.
#[cfg(unix)]
fn provision(verb: atpkg_keys::provision::Verb, argv: &Argv) -> Result<(), String> {
    use atpkg_keys::provision as prov;

    // Before anything sensitive exists in this process (leak vector 7).
    atpkg_keys::master::forbid_core_dumps();

    let id = flag(argv, "id")
        .ok_or_else(|| concat(&["usage: atpkg-keys ", verb.name(), " --id <machine-id>"]))?;
    let head_id = flag(argv, "head-id").unwrap_or(prov::DEFAULT_HEAD_ID);
    let now = now_unix()?;
    let paths = prov::Paths {
        // Discovered, not configured: the operator's job is to type one phrase, and a
        // `--pins` path typed wrong is another transcription risk in a different costume.
        // (An UNREAD `--pins` is worse still, which is why `--pins=x` is now refused rather
        // than falling through to this discovery.)
        pins: match flag(argv, "pins") {
            Some(p) => p.to_string(),
            None => prov::discover_pins_path()?,
        },
        // Explicit names another tree; discovery names THIS one — the distinction the
        // compiled-anchor refusal in `preflight` turns on. An explicit path that
        // CANONICALIZES to the discovered tree's own anchor is discovery wearing a
        // flag (or a symlink to it) and gets no bypass — external audit 2026-08-15.
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
    let pre = prov::preflight(verb, id, head_id, &paths)?;

    let report = match verb {
        prov::Verb::Setup => {
            // (1) SOMEWHERE TO DELIVER IT, before there is anything to deliver.
            let mut tty = atpkg_keys::master::Tty::open()
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;
            // (2) Generate and compute everything. Still nothing shown, nothing written.
            let phrase = atpkg_keys::master::generate_master()
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;
            let seed = phrase.seed();
            let planned = prov::plan(pre, &seed, now)
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;

            // (3) THE ONE PLACE IN THIS TREE A SECRET IS DELIBERATELY EMITTED. It is
            // unavoidable: the owner cannot write down what they cannot see. It goes to the
            // terminal, and the write is checked.
            show_phrase(&mut tty, &phrase, &seed)
                .map_err(|e| concat(&[&e, "\n", SETUP_ARMED_NOTHING]))?;

            // (3b) PROVE THE PAPER. The phrase must be retyped from the paper before
            // anything is armed — a shown-but-never-copied master arms nothing.
            confirm_transcription(&mut tty, &phrase, &seed)
                .map_err(|e| concat(&[&e, "\n", SETUP_PAPER_ARMS_NOTHING]))?;

            // (4) Arm the anchor. From here the paper is load-bearing.
            prov::write_pins(&planned)
                .map_err(|e| concat(&[&e, "\n", SETUP_PAPER_ARMS_NOTHING]))?;
            announce_master(&seed)?;

            prov::write_rest(planned)
                .map_err(|e| concat(&[&e, "\n", &setup_armed_but_unfinished(&paths.pins)]))?
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

/// Read the roster and its signature, verify under `master_pubkey`, and parse — the CLI
/// skin over [`atpkg_keys::provision::load_roster`], which holds the rule that matters:
/// there is deliberately NO unverified parse path in this tool. Reading a roster means
/// verifying it first, which is also the automatic transcription check.
///
/// All this layer adds is the line that tells the operator a fresh roster was started,
/// because the library half is used by code paths that report through a structured
/// summary instead.
///
/// The exact byte SNAPSHOT comes back with the roster, because guarded publication needs
/// it twice over: as the compare-and-swap premise checked immediately before the write,
/// and as the predecessor a crash-recovery replay is entitled to overwrite.
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
    // `MayCreateFresh` is this wrapper's callers' contract (`setup`, the first
    // mint on a box with no roster); `join`'s stricter `MustExist` is applied inside
    // `provision::plan`, not here.
    let (roster, snapshot) = atpkg_keys::provision::load_roster(
        path,
        master_pubkey,
        now,
        atpkg_keys::provision::RosterExpectation::MayCreateFresh,
    )?;
    let was_fresh = snapshot.is_none();
    if was_fresh {
        eprint_line(&concat(&[
            "atpkg-keys: no roster at ",
            path,
            " — starting a new one at roster_seq 0",
        ]));
    }
    Ok((roster, snapshot))
}

/// A stable, printable name for a rejection (see
/// [`atpkg_keys::provision::reject_name`], where the mapping lives so the CLI and the
/// provisioning engine cannot describe the same refusal two different ways).
#[cfg(unix)]
fn reject_name(r: &aterm_update_core::roster::RosterReject) -> String {
    atpkg_keys::provision::reject_name(r)
}

/// Emit the roster and its detached master signature. The bytes are signed BEFORE either
/// file is written, and [`atpkg_keys::provision::publish_roster_locked`] commits a
/// durable redo directory before either canonical file moves — so the crash window
/// between the two renames, which no error path can close, is completed forward by the
/// next run that takes the roster lock.
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
            &reject_name(&e),
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

/// `machine-revoke --id <id>` — withdraw a machine's authority.
///
/// This is the operation the retired one-key design could not perform, and the reason the
/// tier is worth its cost: a thief holds a MACHINE key, which signs artifacts. Only the
/// paper master signs rosters, so only the owner can deny.
///
/// It can be run from ANY surviving machine — the master is the authority, not the
/// hardware. What it does NOT do is un-install: revocation stops future acceptance, and
/// anything already staged from a malicious-but-validly-signed release is the operator
/// yank's problem (`cargo ship yank` / `min_build`), not this command's.
#[cfg(unix)]
fn machine_revoke(argv: &Argv) -> Result<(), String> {
    atpkg_keys::master::forbid_core_dumps();
    let id = flag(argv, "id").ok_or("usage: atpkg-keys machine-revoke --id <machine-id>")?;
    let roster_path = flag(argv, "roster").unwrap_or(DEFAULT_ROSTER);
    let now = now_unix()?;

    let phrase = atpkg_keys::master::prompt_for_master(
        "master phrase (52 characters, echo off; spaces, case, and o/i/l are forgiven): ",
    )?;
    let seed = phrase.seed();
    announce_master(&seed)?;
    // ONE lock across read, edit and publish — the same one `setup`/`join` take, so a
    // revoke and a join cannot each read sequence N and each publish an N+1 that
    // de-authorizes the other's machine. The kernel releases it if this process dies.
    let roster_lock = atpkg_keys::provision::lock_roster(roster_path)?;
    let (roster, snapshot) = load_roster(roster_path, &seed.pubkey_b64()?, now)?;
    let roster = atpkg_keys::roster_ops::revoke(roster, id, now)?;
    publish_roster(&roster_lock, snapshot.as_ref(), roster_path, &roster, &seed)?;
    show_roster(&roster);
    eprint_line(&concat(&[
        "atpkg-keys: '",
        id,
        "' is revoked. Publish the roster and its .sig on the next release — running \
         clients pick it up on their next check (75s authenticated, 15min anonymous) and \
         refuse that machine before any signature check. A FRESH install (no roster_seq \
         floor yet) can still be served an older, still-master-signed roster that lists \
         the revoked machine — rosters never lapse by design, so re-key entirely if that \
         residual matters for the theft at hand.",
    ]));
    Ok(())
}
