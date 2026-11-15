// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Subprocess CLI tests for `atpkg-keys` provisioning: the master phrase comes from the vault
//! alone, and a Mac without it files `aterm secrets need paper-master`.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use aterm_update_core::roster::{Roster, verify_roster};
use atpkg_keys::master::parse_master;
use atpkg_keys::pins_edit::{MASTER_ANCHOR, read_anchor};
use atpkg_keys::roster_ops::{add, empty};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// An obviously synthetic master.
const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

/// A realistic machine key for a roster entry this test writes by hand.
const HEAD_KEY: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";

/// 2026-08-04T00:00:00Z.
const NOW: u64 = 1_785_801_600;

/// A `pins.rs` master anchor.
fn pins_fixture(master_entry: &str) -> String {
    let mut s = String::from(
        "// Copyright 2026 Andrew Yates\n\
         // SPDX-License-Identifier: Apache-2.0\n\
         \n\
         /// The paper master. Empty here means INERT.\n",
    );
    if master_entry.is_empty() {
        s.push_str("pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n");
    } else {
        s.push_str("pub const PAPER_MASTER_PUBKEYS: &[&str] = &[\n    \"");
        s.push_str(master_entry);
        s.push_str("\",\n];\n");
    }
    s
}

/// A private scratch tree for one test, removed when the test ends however it ends.
fn scratch(name: &str) -> aterm_tempfile::TempDir {
    let end = name.floor_char_boundary(24);
    let prefix = format!("atpkg-keys-cli-{}-", &name[..end]);
    aterm_tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".noindex")
        .tempdir()
        .expect("scratch dir")
}

fn p(dir: &Path, name: &str) -> String {
    dir.join(name).to_str().expect("utf-8 path").to_string()
}

/// The environment every child gets ([`settle_before_fork`]).
fn base_command(dir: &Path, args: &[&str]) -> Command {
    settle_before_fork();
    // A fresh build's first-exec check is waited out here, outside the terminal runs' hang
    // detectors (an unknown verb ends it at once).
    let exe = aterm_tempfile::bin_exe("atpkg-keys");
    aterm_spec::wait::first_exec(&exe, &["--help"]);
    let mut cmd = Command::new(&exe);
    cmd.args(args)
        .env("HOME", dir)
        .env("ATPKG_MASTER", PAPER)
        .env("ATERM_MASTER", PAPER)
        .env("MASTER", PAPER)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Settle libSystem's libnotify state before this process's first fork.
fn settle_before_fork() {
    #[cfg(target_vendor = "apple")]
    {
        static SETTLED: std::sync::Once = std::sync::Once::new();
        SETTLED.call_once(|| {
            unsafe extern "C" {
                /// `notify.h`: whether `token` is registered.
                fn notify_is_valid_token(token: std::ffi::c_int) -> bool;
            }
            // SAFETY: takes an int and reads libnotify's own table; no pointer crosses.
            let _ = unsafe { notify_is_valid_token(0) };
        });
    }
}

/// Spawn the real binary with no controlling terminal and `stdin` taken from `stdin_path`.
fn run(dir: &Path, stdin_path: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = base_command(dir, args);
    match stdin_path {
        Some(path) => {
            let f = std::fs::File::open(path).expect("stdin fixture");
            cmd.stdin(Stdio::from(f));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    // SAFETY: `setsid` is async-signal-safe and touches no state shared with the parent; it
    // runs post-fork, pre-exec. Its result is ignored: failing because the child already
    // leads a session is harmless, and the assertions do not depend on how `/dev/tty` became
    // unavailable.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.output().expect("the atpkg-keys binary runs")
}

fn stderr_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Write an armed `pins.rs` plus a roster the synthetic master signed.
fn armed_tree(dir: &Path) -> String {
    let seed = parse_master(PAPER).expect("synthetic phrase").seed();
    let master_pub = seed.pubkey_b64().expect("master identity");
    std::fs::write(p(dir, "pins.rs"), pins_fixture(&master_pub)).expect("pins fixture");

    let (_, m3_pub) = atpkg_keys::generate().expect("a machine key");
    let roster = add(empty(NOW), "m3", &m3_pub, NOW).expect("m3 joins");
    let bytes = roster.to_toml().expect("valid roster").into_bytes();
    let sig = seed.sign(&bytes).expect("the master signs");
    std::fs::write(p(dir, "roster.toml"), &bytes).expect("roster");
    std::fs::write(p(dir, "roster.toml.sig"), &sig).expect("roster sig");
    master_pub
}

/// The first phrase-shaped line of `text`: 52 symbols, spaces allowed.
fn phrase_on(text: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    text.lines().map(str::trim).find_map(|l| {
        (!l.is_empty()
            && l.bytes().all(|b| b == b' ' || ALPHABET.contains(&b))
            && l.bytes().filter(|b| *b != b' ').count() == 52)
            .then(|| l.to_string())
    })
}

/// This home's vault paths.
fn vault_paths(dir: &Path) -> aterm_secrets::client::Paths {
    aterm_secrets::client::Paths::locate_for_home(dir)
}

/// This home's vault holding `phrase` in its store (replacing what it held), read in place as a
/// member's sync leaves it: the vault is made here, with this Mac its admin, where the home has
/// none yet.
fn registry_holding(dir: &Path, phrase: &str) {
    use aterm_secrets::{client, text::Audience};
    let paths = vault_paths(dir);
    if client::config(&paths).unwrap().is_none() {
        admin_vault(dir);
    }
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        aterm_secrets::paper_master::ENTRY,
        phrase.as_bytes(),
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
}

/// A vault at a local source with this home's Mac as its admin.
fn admin_vault(dir: &Path) -> aterm_secrets::client::Paths {
    let paths = vault_paths(dir);
    let source = dir.join("test.vault");
    aterm_secrets::client::create(&paths, source.to_str().unwrap(), None, Some("admin"))
        .expect("a scratch vault");
    paths
}

/// Whether this home's need ledger filed the paper master.
fn filed_paper_master(dir: &Path) -> bool {
    aterm_secrets::need::list(&vault_paths(dir)).is_ok_and(|needs| {
        needs
            .iter()
            .any(|need| need.want == aterm_secrets::need::Want::PaperMaster)
    })
}

const NEED: &str = "`aterm secrets need paper-master`";

/// Every file under `dir`, scanned for `needle` and for any 16-character run of it.
fn assert_nothing_on_disk_holds(dir: &Path, needle: &str) -> usize {
    let mut checked = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("readable scratch tree") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            checked += 1;
            let raw = std::fs::read(&path).expect("readable file");
            let text = String::from_utf8_lossy(&raw);
            assert!(!text.contains(needle), "{path:?} holds the phrase");
            for start in 0..needle.len().saturating_sub(16) {
                assert!(
                    !text.contains(&needle[start..start + 16]),
                    "{path:?} holds a 16-character run of the phrase"
                );
            }
        }
    }
    checked
}

/// The phrase is never taken from the environment, stdin or a file: with none in the vault,
/// `join` files `paper-master`, names that request and writes nothing.
#[test]
fn join_without_the_phrase_in_the_vault_files_paper_master_and_writes_nothing() {
    let tmp = scratch("join-leak-vectors");
    let dir = tmp.path().to_path_buf();
    armed_tree(&dir);
    let phrase_file = p(&dir, "phrase.txt");
    std::fs::write(&phrase_file, PAPER).expect("the file the operator must not be able to use");

    let before_pins = std::fs::read(p(&dir, "pins.rs")).unwrap();
    let before_roster = std::fs::read(p(&dir, "roster.toml")).unwrap();

    let out = run(
        &dir,
        Some(&phrase_file),
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );

    assert!(!out.status.success(), "join must refuse: {out:?}");
    let err = stderr_of(&out);
    assert!(
        err.contains(NEED),
        "the refusal names the request it filed: {err}"
    );
    assert!(filed_paper_master(&dir), "the need ledger holds it");
    assert!(
        !stdout_of(&out).contains(PAPER) && !err.contains(PAPER),
        "a refusal must never echo the phrase back"
    );
    assert!(
        !Path::new(&p(&dir, "m11.key")).exists(),
        "a refused join must mint no key"
    );
    assert_eq!(std::fs::read(p(&dir, "pins.rs")).unwrap(), before_pins);
    assert_eq!(
        std::fs::read(p(&dir, "roster.toml")).unwrap(),
        before_roster
    );
}

#[test]
fn a_flag_that_would_carry_the_master_is_refused_and_calls_it_compromised() {
    let tmp = scratch("master-flag");
    let dir = tmp.path().to_path_buf();
    armed_tree(&dir);
    let before = std::fs::read(p(&dir, "pins.rs")).unwrap();

    for flag in ["--master", "--master-file", "--phrase", "--seed"] {
        let out = run(
            &dir,
            None,
            &[
                "join",
                "--id",
                "m11",
                "--pins",
                &p(&dir, "pins.rs"),
                "--roster",
                &p(&dir, "roster.toml"),
                flag,
                PAPER,
            ],
        );
        assert!(!out.status.success(), "{flag} must be refused");
        let err = stderr_of(&out);
        assert!(err.contains(flag), "the refusal names the flag: {err}");
        assert!(
            err.contains("COMPROMISED"),
            "the refusal must say what the operator has to do about it: {err}"
        );
        assert!(err.contains("the vault"), "{err}");
        assert!(
            !err.contains(PAPER),
            "the refusal must not echo the value: {err}"
        );
    }
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before,
        "a refused run writes nothing"
    );
    assert!(!filed_paper_master(&dir), "a refused flag reads no vault");

    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
        ],
    );
    assert!(
        !stderr_of(&out).contains("COMPROMISED"),
        "{}",
        stderr_of(&out)
    );
    assert!(stderr_of(&out).contains(NEED), "{}", stderr_of(&out));
}

/// A Mac that cannot publish to a vault as its admin mints no master: nothing is written and no
/// phrase appears anywhere.
#[test]
fn setup_on_a_mac_that_cannot_write_the_vault_writes_nothing() {
    let tmp = scratch("setup-no-vault");
    let dir = tmp.path().to_path_buf();
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).expect("unarmed fixture");
    let before = std::fs::read(p(&dir, "pins.rs")).unwrap();

    let out = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );

    assert!(!out.status.success(), "setup must refuse without a vault");
    let err = stderr_of(&out);
    assert!(err.contains("the vault did not take"), "{err}");
    assert!(
        err.contains("no master exists as a result of this run"),
        "the refusal must state that no master exists: {err}"
    );
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before,
        "the anchor is untouched"
    );
    assert!(!Path::new(&p(&dir, "m3.key")).exists());
    assert!(!Path::new(&p(&dir, "roster.toml")).exists());
    for text in [stdout_of(&out), err] {
        assert!(phrase_on(&text).is_none(), "no phrase-shaped line: {text}");
    }
}

#[test]
fn an_unread_flag_spelling_is_refused_rather_than_silently_replaced_by_a_default() {
    let tmp = scratch("flag-equals");
    let dir = tmp.path().to_path_buf();
    let checkout = dir.join("checkout/crates/aterm-update-core/src");
    std::fs::create_dir_all(&checkout).expect("fake checkout");
    let discovered = checkout.join("pins.rs");
    std::fs::write(&discovered, pins_fixture("")).expect("the file the tool would find");
    let named = p(&dir, "decoy-pins.rs");
    std::fs::write(&named, pins_fixture("")).expect("the file the operator named");

    let mut pins_eq = String::from("--pins=");
    pins_eq.push_str(&named);
    let mut cmd = base_command(&dir, &["setup", "--id", "oops", &pins_eq]);
    cmd.current_dir(dir.join("checkout")).stdin(Stdio::null());
    // SAFETY: see `run`.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let out = cmd.output().expect("the binary runs");

    assert!(!out.status.success(), "the unread spelling must be refused");
    let err = stderr_of(&out);
    assert!(err.contains("--name=value"), "{err}");
    assert!(err.contains("two tokens"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&discovered).unwrap(),
        pins_fixture(""),
        "THE DISCOVERED CHECKOUT MUST NOT HAVE BEEN ARMED"
    );
    assert_eq!(std::fs::read_to_string(&named).unwrap(), pins_fixture(""));

    let out = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "oops",
            "--pins",
            &named,
            "--roster",
            &p(&dir, "r.toml"),
        ],
    );
    assert!(
        !stderr_of(&out).contains("--name=value"),
        "{}",
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).contains("no master exists as a result of this run"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn unknown_and_malformed_arguments_are_refused_by_name() {
    let tmp = scratch("bad-args");
    let dir = tmp.path().to_path_buf();
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).unwrap();

    let out = run(
        &dir,
        None,
        &["setup", "--id", "m3", "--pinz", &p(&dir, "pins.rs")],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("unknown flag '--pinz'"), "{err}");
    assert!(
        err.contains("--pins"),
        "the message lists what IS accepted: {err}"
    );

    let out = run(
        &dir,
        None,
        &["setup", "--id", "--pins", &p(&dir, "pins.rs")],
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("as its value, which is another flag"),
        "{}",
        stderr_of(&out)
    );

    let out = run(&dir, None, &["setup", "stray", "--id", "m3"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("unexpected argument 'stray'"),
        "{}",
        stderr_of(&out)
    );

    let out = run(&dir, None, &["setup", "--id", "m3", "--id", "m11"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("'--id' was given more than once"),
        "{}",
        stderr_of(&out)
    );

    let out = run(
        &dir,
        None,
        &["setup", "--id", "m3", "--pins", &p(&dir, "pins.rs")],
    );
    let err = stderr_of(&out);
    assert!(!err.contains("unknown flag"), "{err}");
    assert!(!err.contains("unexpected argument"), "{err}");
}

/// `sign` takes every file `verify-signed` takes: a payload past the 1 MiB key-file cap signs and
/// verifies, and one past the shared signed-file cap is refused by both verbs before anything is
/// written. NEGATIVE CONTROL: the 1 MiB + 17 payload is one a `sign` reading at the key-file cap
/// refuses.
#[test]
fn sign_and_verify_signed_take_the_same_files() {
    use atpkg_keys::fsio::{READ_CAP, SIGNED_FILE_READ_CAP};

    let tmp = scratch("sign-cap");
    let dir = tmp.path().to_path_buf();
    let seed = parse_master(PAPER).expect("synthetic phrase").seed();
    let master_pub = seed.pubkey_b64().expect("master identity");
    let (machine_key, machine_pub) = atpkg_keys::generate().expect("machine keypair");
    let key = p(&dir, "m3.key");
    std::fs::write(&key, &machine_key).unwrap();
    let roster = add(empty(NOW), "m3", &machine_pub, NOW).expect("m3 joins");
    let roster_bytes = roster.to_toml().expect("valid roster").into_bytes();
    std::fs::write(p(&dir, "roster.toml"), &roster_bytes).unwrap();
    std::fs::write(
        p(&dir, "roster.toml.sig"),
        seed.sign(&roster_bytes).expect("master signs"),
    )
    .unwrap();
    let verify = |file: &str, sig: &str| {
        run(
            &dir,
            None,
            &[
                "verify-signed",
                &master_pub,
                file,
                sig,
                &p(&dir, "roster.toml"),
                &p(&dir, "roster.toml.sig"),
            ],
        )
    };

    let payload = p(&dir, "SHA256SUMS");
    let len = usize::try_from(READ_CAP).expect("fits usize") + 17;
    std::fs::write(&payload, vec![b'a'; len]).unwrap();
    let signed = run(&dir, None, &["sign", &key, &payload]);
    assert!(signed.status.success(), "{}", stderr_of(&signed));
    let sig = p(&dir, "SHA256SUMS.sig");
    let verified = verify(&payload, &sig);
    assert!(verified.status.success(), "{}", stderr_of(&verified));
    assert!(
        stdout_of(&verified).contains("OK: signed by machine m3"),
        "{}",
        stdout_of(&verified)
    );

    let over = p(&dir, "OVER");
    std::fs::File::create(&over)
        .and_then(|f| f.set_len(SIGNED_FILE_READ_CAP + 1))
        .expect("a file one byte past the cap");
    let refused = run(&dir, None, &["sign", &key, &over]);
    assert!(!refused.status.success());
    assert!(
        stderr_of(&refused).contains("exceeds the 64 MiB read cap"),
        "{}",
        stderr_of(&refused)
    );
    assert!(!Path::new(&p(&dir, "OVER.sig")).exists(), "no signature");
    let refused = verify(&over, &sig);
    assert!(!refused.status.success());
    assert!(
        stderr_of(&refused).contains("exceeds the 64 MiB read cap"),
        "{}",
        stderr_of(&refused)
    );
}

#[test]
fn machine_revoke_with_no_roster_refuses_before_reading_the_vault() {
    let tmp = scratch("revoke-no-roster");
    let dir = tmp.path().to_path_buf();
    let roster = p(&dir, "dist/aterm-machines.toml");
    let out = run(
        &dir,
        None,
        &["machine-revoke", "--id", "m3", "--roster", &roster],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no roster at "), "{err}");
    assert!(
        !err.contains("paper-master") && !filed_paper_master(&dir),
        "refused before the vault is read: {err}"
    );
    assert!(
        !dir.join("dist").exists(),
        "no roster lock or directory is created: {err}"
    );

    armed_tree(&dir);
    let out = run(
        &dir,
        None,
        &[
            "machine-revoke",
            "--id",
            "m3",
            "--roster",
            &p(&dir, "roster.toml"),
        ],
    );
    let err = stderr_of(&out);
    assert!(!err.contains("no roster at "), "{err}");
    assert!(err.contains(NEED), "{err}");
}

/// `machine-revoke` signs with the vault's phrase.
#[test]
fn machine_revoke_signs_with_the_vaults_phrase() {
    let tmp = scratch("revoke-vault");
    let dir = tmp.path().to_path_buf();
    let master_pub = armed_tree(&dir);
    registry_holding(&dir, PAPER);
    let out = run(
        &dir,
        None,
        &[
            "machine-revoke",
            "--id",
            "m3",
            "--roster",
            &p(&dir, "roster.toml"),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let bytes = std::fs::read(p(&dir, "roster.toml")).unwrap();
    let sig = std::fs::read(p(&dir, "roster.toml.sig")).unwrap();
    let roster = Roster::parse(&verify_roster(&[&master_pub], bytes, &sig).expect("verifies"))
        .expect("parses");
    assert_eq!(roster.revoked, ["m3"]);
    assert!(!stdout_of(&out).contains(PAPER) && !stderr_of(&out).contains(PAPER));
}

#[test]
fn join_without_a_roster_refuses_instead_of_forking_one() {
    let tmp = scratch("join-no-roster");
    let dir = tmp.path().to_path_buf();
    armed_tree(&dir);
    std::fs::remove_file(p(&dir, "roster.toml")).unwrap();
    std::fs::remove_file(p(&dir, "roster.toml.sig")).unwrap();

    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no roster at"), "{err}");
    assert!(
        err.contains("COPY"),
        "the refusal must say how to fix it: {err}"
    );
    assert!(
        err.contains("will not start a second roster"),
        "and why it will not do it for you: {err}"
    );
    assert!(!err.contains("paper-master"), "{err}");
    assert!(
        !Path::new(&p(&dir, "roster.toml")).exists(),
        "no roster was created"
    );
    assert!(!Path::new(&p(&dir, "m11.key")).exists());

    let seed = parse_master(PAPER).unwrap().seed();
    let roster = add(empty(NOW), "m3", HEAD_KEY, NOW).unwrap();
    let bytes = roster.to_toml().unwrap().into_bytes();
    std::fs::write(p(&dir, "roster.toml"), &bytes).unwrap();
    std::fs::write(p(&dir, "roster.toml.sig"), seed.sign(&bytes).unwrap()).unwrap();
    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    let err = stderr_of(&out);
    assert!(!err.contains("no roster at"), "{err}");
    assert!(err.contains(NEED), "it got as far as the vault: {err}");
}

/// `setup` mints the master into the vault, the phrase's one store, and arms the anchor with
/// it; the phrase is on no output and in no file but the vault's own.
#[test]
fn setup_mints_the_master_into_the_vault_and_shows_it_nowhere() {
    let tmp = scratch("setup-vault");
    let dir = tmp.path().to_path_buf();
    let vault = admin_vault(&dir);
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).expect("unarmed fixture");

    let out = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );
    assert!(
        out.status.success(),
        "setup must succeed on the vault's admin Mac.\nstderr: {}",
        stderr_of(&out)
    );

    let stored = aterm_secrets::paper_master::read(&vault).expect("the vault holds the phrase");
    let phrase = std::str::from_utf8(stored.as_slice()).unwrap().to_owned();
    assert!(aterm_secrets::paper_master::well_formed(phrase.as_bytes()));
    for text in [stdout_of(&out), stderr_of(&out)] {
        assert!(!text.contains(&phrase), "{text}");
        for start in 0..36 {
            assert!(!text.contains(&phrase[start..start + 16]), "{text}");
        }
        assert!(phrase_on(&text).is_none(), "{text}");
    }

    let src = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
    let master_pub = parse_master(&phrase)
        .expect("the stored phrase parses")
        .seed()
        .pubkey_b64()
        .unwrap();
    assert_eq!(
        read_anchor(&src, MASTER_ANCHOR).unwrap().members,
        vec![master_pub.clone()],
        "the tool wrote the anchor of the vault's master"
    );

    let bytes = std::fs::read(p(&dir, "roster.toml")).unwrap();
    let sig = std::fs::read(p(&dir, "roster.toml.sig")).unwrap();
    let roster = Roster::parse(&verify_roster(&[&master_pub], bytes, &sig).expect("verifies"))
        .expect("parses");
    assert_eq!(roster.machines.len(), 1);
    assert_eq!(roster.machines[0].id, "m3");
    let machine_pub = roster.machines[0].pubkey.clone();
    assert!(
        !src.contains(&machine_pub),
        "the minted key must appear nowhere in the anchor file: {src}"
    );

    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(p(&dir, "m3.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let record = std::fs::read_to_string(dir.join(".aterm/release-ring/machine.toml")).unwrap();
    assert!(record.contains("id = \"m3\""), "{record}");
    assert!(
        stdout_of(&out).contains("the ONLY roster this master signs"),
        "{}",
        stdout_of(&out)
    );

    let kept = dir
        .join(".secrets")
        .join(aterm_secrets::paper_master::ENTRY);
    let saved = std::fs::read(&kept).unwrap();
    std::fs::remove_file(&kept).unwrap();
    let checked = assert_nothing_on_disk_holds(&dir, &phrase);
    std::fs::write(&kept, saved).unwrap();
    assert!(
        checked >= 5,
        "the run must have written five files; saw {checked}"
    );

    let again = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    assert!(!again.status.success());
    assert!(
        stderr_of(&again).contains("ALREADY committed"),
        "{}",
        stderr_of(&again)
    );
    assert_eq!(
        std::fs::read_to_string(p(&dir, "pins.rs")).unwrap(),
        src,
        "the refused re-run left the anchor exactly as it was"
    );
}

/// A vault that already holds a phrase keeps it: `setup` on an unarmed tree mints nothing.
#[test]
fn setup_never_replaces_the_vaults_phrase() {
    let tmp = scratch("setup-vault-held");
    let dir = tmp.path().to_path_buf();
    let vault = admin_vault(&dir);
    aterm_secrets::paper_master::store(&vault, PAPER.as_bytes()).unwrap();
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).expect("unarmed fixture");

    let out = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("already holds"), "{err}");
    assert!(
        err.contains("no master exists as a result of this run"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(p(&dir, "pins.rs")).unwrap(),
        pins_fixture("")
    );
    assert!(!Path::new(&p(&dir, "m3.key")).exists());
    assert_eq!(
        aterm_secrets::paper_master::read(&vault)
            .unwrap()
            .as_slice(),
        PAPER.as_bytes()
    );
}

/// `join` takes the vault's phrase: a phrase that derives another master writes nothing, and the
/// committed one extends the roster.
#[test]
fn join_reads_the_phrase_from_the_vault_and_extends_the_roster() {
    let tmp = scratch("join-vault");
    let dir = tmp.path().to_path_buf();
    let master_pub = armed_tree(&dir);
    let before_pins = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();

    let mut wrong = String::from(PAPER);
    wrong.replace_range(0..1, "f");
    registry_holding(&dir, &wrong);
    let bad = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    assert!(!bad.status.success(), "another master must be refused");
    assert!(
        stderr_of(&bad).contains("is NOT the master committed"),
        "stderr: {}",
        stderr_of(&bad)
    );
    assert_eq!(
        std::fs::read_to_string(p(&dir, "pins.rs")).unwrap(),
        before_pins,
        "a refused join writes nothing"
    );
    assert!(!Path::new(&p(&dir, "m11.key")).exists());
    assert!(!stderr_of(&bad).contains(&wrong) && !stdout_of(&bad).contains(&wrong));

    registry_holding(&dir, PAPER);
    let good = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );
    assert!(good.status.success(), "stderr: {}", stderr_of(&good));
    assert!(!stdout_of(&good).contains(PAPER) && !stderr_of(&good).contains(PAPER));

    let src = std::fs::read_to_string(p(&dir, "pins.rs")).unwrap();
    assert_eq!(
        read_anchor(&src, MASTER_ANCHOR).unwrap().members,
        vec![master_pub.clone()]
    );

    let bytes = std::fs::read(p(&dir, "roster.toml")).unwrap();
    let sig = std::fs::read(p(&dir, "roster.toml.sig")).unwrap();
    let roster = Roster::parse(&verify_roster(&[&master_pub], bytes, &sig).expect("verifies"))
        .expect("parses");
    let ids: Vec<&str> = roster.machines.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m3", "m11"], "the existing machine survives");
    assert_eq!(
        roster.roster_seq, 2,
        "the sequence advanced rather than restarting"
    );
    assert!(
        !stdout_of(&good).contains("the ONLY roster this master signs"),
        "join extended a roster; it must not claim to have created one: {}",
        stdout_of(&good)
    );
}

/// `master-check` derives the vault's master and refuses one this build does not pin; without a
/// phrase it files `paper-master`.
#[test]
fn master_check_reads_the_vault_and_holds_it_to_the_compiled_anchor() {
    let tmp = scratch("master-check");
    let dir = tmp.path().to_path_buf();
    let out = run(&dir, None, &["master-check"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains(NEED), "{}", stderr_of(&out));

    registry_holding(&dir, PAPER);
    let out = run(&dir, None, &["master-check"]);
    assert!(
        !out.status.success(),
        "a synthetic master is pinned by no build"
    );
    assert!(
        stdout_of(&out).contains("master fingerprint: "),
        "{}",
        stdout_of(&out)
    );
    assert!(stderr_of(&out).contains("derives no master this build pins"));
    assert!(!stdout_of(&out).contains(PAPER) && !stderr_of(&out).contains(PAPER));
}

/// The vault took the phrase but the anchor could not be written: nothing else is written, and
/// the refusal says how to clear the vault's orphan before a new run.
#[test]
fn a_setup_that_cannot_write_the_anchor_leaves_it_unarmed_and_says_so() {
    let tmp = scratch("setup-unwritable");
    let dir = tmp.path().to_path_buf();
    admin_vault(&dir);
    let anchor_dir = dir.join("anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    let pins = anchor_dir.join("pins.rs").to_str().unwrap().to_string();
    std::fs::write(&pins, pins_fixture("")).unwrap();

    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&anchor_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let out = run(
        &dir,
        None,
        &[
            "setup",
            "--id",
            "m3",
            "--pins",
            &pins,
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );
    std::fs::set_permissions(&anchor_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(!out.status.success(), "the run must fail");
    let err = stderr_of(&out);
    assert!(err.contains("arms nothing"), "stderr: {err}");
    assert!(
        err.contains("aterm secrets unset release-paper-master"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(&pins).unwrap(),
        pins_fixture(""),
        "THE ANCHOR IS UNARMED"
    );
    assert!(!Path::new(&p(&dir, "roster.toml")).exists());
    assert!(!Path::new(&p(&dir, "m3.key")).exists());
}

#[test]
fn join_on_an_unarmed_tree_refuses_before_the_vault() {
    let tmp = scratch("join-unarmed");
    let dir = tmp.path().to_path_buf();
    std::fs::write(p(&dir, "pins.rs"), pins_fixture("")).unwrap();
    let out = run(
        &dir,
        None,
        &[
            "join",
            "--id",
            "m3",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m3.key"),
        ],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no paper master is committed"), "{err}");
    assert!(err.contains("setup"), "{err}");
    assert!(
        !err.contains("paper-master") && !filed_paper_master(&dir),
        "{err}"
    );
}

#[test]
fn setup_refuses_on_an_armed_tree() {
    let tmp = scratch("setup-armed-refusal");
    let dir = tmp.path().to_path_buf();
    armed_tree(&dir);
    let phrase_file = p(&dir, "phrase.txt");
    std::fs::write(&phrase_file, PAPER).unwrap();
    let before_pins = std::fs::read(p(&dir, "pins.rs")).unwrap();

    let out = run(
        &dir,
        Some(&phrase_file),
        &[
            "setup",
            "--id",
            "m11",
            "--pins",
            &p(&dir, "pins.rs"),
            "--roster",
            &p(&dir, "roster.toml"),
            "--key",
            &p(&dir, "m11.key"),
        ],
    );

    assert!(!out.status.success(), "setup must refuse an armed tree");
    let err = stderr_of(&out);
    assert!(err.contains("ALREADY committed"), "{err}");
    assert!(err.contains("join"), "{err}");
    assert!(
        !err.contains(PAPER),
        "the refusal must not echo a phrase: {err}"
    );
    assert_eq!(
        std::fs::read(p(&dir, "pins.rs")).unwrap(),
        before_pins,
        "a refused setup writes nothing"
    );
    assert!(!Path::new(&p(&dir, "m11.key")).exists());
}
