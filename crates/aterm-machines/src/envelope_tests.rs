// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Live, Ran, Stat};

fn envelope(host: &str, serial: u64, issued: u64) -> Envelope {
    Envelope {
        schema: 1,
        host: host.into(),
        host_name: "lab-07".into(),
        serial,
        issued,
        expires: issued + 86_400,
        action: Action::Apply,
        disrupt: Disrupt::WhenIdle,
        idle_secs: 900,
        baseline: Some("schema = 1\nusers = [\"admin\"]\n".into()),
        artifacts: vec![ArtifactRef {
            sha256: "ab".repeat(32),
            name: "app.Emacs".into(),
        }],
        exec: None,
        upgrade: None,
        rekey: None,
    }
}

/// A signature `ssh-keygen -Y sign` wrote (OpenSSH 10.3, a throwaway key).
const SIG: &str = "-----BEGIN SSH SIGNATURE-----\n\
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgSwtZ7v55h6ZC7I/f6ieVoCUIc1\n\
UurXKiMHgI/I9bjJEAAAAOYXRlcm0tbWFjaGluZXMAAAAAAAAABnNoYTUxMgAAAFMAAAAL\n\
c3NoLWVkMjU1MTkAAABAmFAIRmmKIXpRv6CqO3Mn1cCVmjnKSGPwiGfkzT1hhEnzXPFB7F\n\
h+C/7UIZ5kDs26vTm+z6W30uFz3DtGTzPRBw==\n\
-----END SSH SIGNATURE-----\n";

#[test]
fn split_cuts_after_the_signature_end_line() {
    let wire = format!("{SIG}schema = 1\n");
    let (sig, body) = split(wire.as_bytes()).unwrap();
    assert_eq!(sig, SIG.as_bytes());
    assert_eq!(body, b"schema = 1\n");
}

#[test]
fn split_refuses_what_is_not_a_signature_followed_by_a_body() {
    for wire in [
        "schema = 1\n".to_owned(),
        format!(" {SIG}schema = 1\n"),
        SIG.replace(END, ""),
        SIG.to_owned(),
        format!("{BEGIN}not base64 at all!\n{END}schema = 1\n"),
        format!("{BEGIN}{END}schema = 1\n"),
    ] {
        assert!(
            matches!(split(wire.as_bytes()), Err(Refusal::Malformed(_))),
            "{wire:?}"
        );
    }
}

#[test]
fn a_body_round_trips_through_its_toml_and_unknown_keys_are_refused() {
    let e = envelope("U", 3, 1_800_000_000);
    let text = body_text(&e).unwrap();
    assert_eq!(parse_body(text.as_bytes()).unwrap(), e);
    let extra = format!("{text}surprise = true\n");
    assert!(matches!(
        parse_body(extra.as_bytes()),
        Err(Refusal::Malformed(_))
    ));
    let wrong = text.replace("schema = 1", "schema = 2");
    let err = parse_body(wrong.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("schema 2"), "{err}");
}

#[test]
fn allowed_signers_must_be_one_namespaced_key_per_line() {
    let blob = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";
    let good =
        format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n# a comment\n");
    assert_eq!(check_signers(&good).unwrap(), ["signing@ctl"]);
    assert_eq!(
        signers_line("signing@ctl", &format!("ssh-ed25519 {blob} me@ctl")).unwrap(),
        format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}")
    );
    for bad in [
        String::new(),
        format!("signing@ctl ssh-ed25519 {blob}\n"),
        format!("signing@ctl namespaces=\"*\" ssh-ed25519 {blob}\n"),
        format!("signing@ctl cert-authority,namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"),
        format!("sign ing namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"),
        format!("a namespaces=\"aterm-machines\" ssh-dss {blob}\n"),
        format!(
            "a namespaces=\"aterm-machines\" ssh-ed25519 {blob}\nb namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"
        ),
    ] {
        assert!(check_signers(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_key_id_is_the_fingerprint_verify_prints() {
    let out = "Good \"aterm-machines\" signature for signing@ctl with ED25519 key \
               SHA256:n3DGXrPVwmgGR8K+oRoVIIetGQDFJsaq8hxEbDCtxLs\n";
    assert_eq!(
        key_id(out).as_deref(),
        Some("SHA256:n3DGXrPVwmgGR8K+oRoVIIetGQDFJsaq8hxEbDCtxLs")
    );
}

fn private(fake: &Fake, dir: &str) {
    fake.stat_of(
        dir,
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
}

/// The fake's answers for a good signature by `signing@ctl`, measured in the VM.
fn script_good(fake: &Fake, scratch: &str) {
    let pid = std::process::id();
    fake.on(
        &format!("/usr/bin/ssh-keygen -Y find-principals -s {scratch}/aterm-open-{pid}.sig -f /as"),
        Ran::ok("signing@ctl\n"),
    );
    fake.on(
        &format!(
            "/usr/bin/ssh-keygen -Y verify -f /as -I signing@ctl -n aterm-machines -s {scratch}/aterm-open-{pid}.sig"
        ),
        Ran::ok(
            "Good \"aterm-machines\" signature for signing@ctl with ED25519 key SHA256:P6HGW+cv8zgw7CrKPm2sOl5fu7RG4dXNK0SYwxiFmiw\n", // gitleaks:allow
        ),
    );
}

#[test]
fn open_asks_who_signed_then_verifies_as_that_principal_then_checks_the_body() {
    let fake = Fake::root();
    private(&fake, "/s");
    script_good(&fake, "/s");
    let body = body_text(&envelope("U", 5, fake.now)).unwrap();
    let wire = format!("{SIG}{body}");
    let opened = open_in(
        &fake,
        wire.as_bytes(),
        Path::new("/as"),
        "U",
        &|_| 4,
        Path::new("/s"),
    )
    .unwrap();
    assert_eq!(opened.principal, "signing@ctl");
    assert_eq!(
        opened.key_id,
        "SHA256:P6HGW+cv8zgw7CrKPm2sOl5fu7RG4dXNK0SYwxiFmiw"
    );
    assert_eq!(opened.envelope.serial, 5);
    let calls = fake.calls();
    assert!(calls[2].contains("find-principals"), "{calls:?}");
    assert!(calls[3].contains("-Y verify"), "{calls:?}");
    let pid = std::process::id();
    assert!(fake.written(&format!("/s/aterm-open-{pid}.sig")).is_none());
    assert!(fake.written(&format!("/s/aterm-open-{pid}.body")).is_none());
}

#[test]
fn open_refuses_an_unknown_signer_with_what_ssh_keygen_said() {
    let fake = Fake::root();
    private(&fake, "/s");
    let pid = std::process::id();
    fake.on(
        &format!("/usr/bin/ssh-keygen -Y find-principals -s /s/aterm-open-{pid}.sig -f /as"),
        Ran {
            status: Some(255),
            stdout: b"No principal matched.\n".to_vec(),
            stderr: Vec::new(),
        },
    );
    let wire = format!("{SIG}{}", body_text(&envelope("U", 5, fake.now)).unwrap());
    let err = open_in(
        &fake,
        wire.as_bytes(),
        Path::new("/as"),
        "U",
        &|_| 0,
        Path::new("/s"),
    )
    .unwrap_err();
    assert_eq!(err, Refusal::BadSignature("No principal matched.".into()));
    assert!(!fake.calls().iter().any(|c| c.contains("-Y verify")));
}

#[test]
fn open_refuses_a_scratch_directory_others_may_write() {
    let fake = Fake::root();
    fake.stat_of(
        "/tmp",
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o1777,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    let wire = format!("{SIG}{}", body_text(&envelope("U", 5, fake.now)).unwrap());
    let err = open_in(
        &fake,
        wire.as_bytes(),
        Path::new("/as"),
        "U",
        &|_| 0,
        Path::new("/tmp"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("writable by others"), "{err}");
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[test]
fn seal_batch_hands_every_body_to_one_ssh_keygen_run() {
    let fake = Fake::root();
    private(&fake, "/s");
    let pid = std::process::id();
    let bodies = [envelope("U", 1, 10), envelope("V", 2, 10)];
    fake.on(
        &format!(
            "/usr/bin/ssh-keygen -Y sign -f /k/signing -n aterm-machines /s/aterm-envelope-{pid}-0.toml /s/aterm-envelope-{pid}-1.toml"
        ),
        Ran::exit(255, "Load key \"/k/signing\": incorrect passphrase supplied to decrypt private key"),
    );
    let err = seal_batch(
        &fake,
        &bodies,
        Path::new("/k/signing"),
        Signing::Unattended,
        Path::new("/s"),
        None,
    )
    .unwrap_err();
    assert!(err.contains("incorrect passphrase"), "{err}");
    let signs = fake
        .calls()
        .iter()
        .filter(|c| c.contains("-Y sign"))
        .count();
    assert_eq!(signs, 1);
    assert!(
        fake.written(&format!("/s/aterm-envelope-{pid}-0.toml"))
            .is_none()
    );
    assert!(
        fake.written(&format!("/s/aterm-envelope-{pid}-1.toml"))
            .is_none()
    );
}

#[test]
fn seal_refuses_a_key_whose_passphrase_probe_does_not_answer() {
    let fake = Fake::root();
    let err = seal(&fake, &envelope("U", 1, 10), Path::new("/k/signing")).unwrap_err();
    assert!(err.contains("ssh-keygen -y"), "{err}");
    assert!(
        !fake.calls().iter().any(|c| c.contains("-Y sign")),
        "{:?}",
        fake.calls()
    );
}

#[test]
fn an_agent_held_key_is_named_by_its_public_half_and_signed_without_a_terminal() {
    let fake = Fake::root();
    private(&fake, "/s");
    let pid = std::process::id();
    fake.on(
        &format!(
            "SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-keygen -Y sign -U -f /k/signing.pub -n aterm-machines /s/aterm-envelope-{pid}-0.toml"
        ),
        Ran::exit(1, ""),
    );
    let handed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&handed);
    fake.simulate(move |_, cmd| {
        seen.lock().unwrap().push(cmd.env.clone());
        None
    });
    let err = seal_batch(
        &fake,
        &[envelope("U", 1, 10)],
        Path::new("/k/signing"),
        Signing::Agent,
        Path::new("/s"),
        Some(OsStr::new("/private/walk-agent.sock")),
    )
    .unwrap_err();
    assert!(err.contains("did not sign: exit 1"), "{err}");
    assert_eq!(
        *handed.lock().unwrap(),
        [vec![(
            "SSH_AUTH_SOCK".to_owned(),
            "/private/walk-agent.sock".to_owned()
        )]]
    );
    assert!(!fake.calls().iter().any(|c| c.starts_with("tty ")));
}

/// An agent run with no socket named is refused before `ssh-keygen` runs, whatever this
/// process's own `SSH_AUTH_SOCK` says. NEGATIVE CONTROL: the run handed a socket, above,
/// reaches `ssh-keygen` with exactly it.
#[test]
fn an_agent_run_with_no_socket_named_is_refused_before_signing() {
    for none in [None, Some(OsStr::new(""))] {
        let fake = Fake::root();
        private(&fake, "/s");
        let err = seal_batch(
            &fake,
            &[envelope("U", 1, 10)],
            Path::new("/k/signing"),
            Signing::Agent,
            Path::new("/s"),
            none,
        )
        .unwrap_err();
        assert!(err.contains("no ssh-agent"), "{none:?}: {err}");
        assert!(
            !fake.calls().iter().any(|c| c.contains("ssh-keygen")),
            "{:?}",
            fake.calls()
        );
    }
}

/// A throwaway controller: signing and recovery keys (recovery with a passphrase), an
/// outsider's key, and the `allowed_signers` naming the first two.
struct Keys {
    dir: aterm_tempfile::TempDir,
}

impl Keys {
    /// Where there is no `/usr/bin/ssh-keygen` at all the test ends COULD NOT RUN; any
    /// other failure to set up is a test failure, never a silent pass.
    fn new() -> Self {
        if !Path::new(SSH_KEYGEN).exists() {
            aterm_uds::host::could_not_run(format!("no {SSH_KEYGEN} here"));
        }
        let dir = aterm_tempfile::tempdir().expect("a temporary directory");
        let live = Live::new();
        for (name, pass) in [("signing", ""), ("recovery", "pw"), ("outsider", "")] {
            let ran = live
                .run(
                    &Cmd::new(SSH_KEYGEN)
                        .args(["-q", "-t", "ed25519", "-N", pass, "-C"])
                        .arg(format!("{name}@test"))
                        .arg("-f")
                        .arg(dir.path().join(name)),
                )
                .expect("ssh-keygen runs");
            assert!(ran.success(), "ssh-keygen -t ed25519: {}", ran.describe());
        }
        let mut signers = String::new();
        for name in ["signing", "recovery"] {
            let public = std::fs::read_to_string(dir.path().join(format!("{name}.pub")))
                .expect("the public key");
            signers.push_str(&signers_line(&format!("{name}@test"), &public).unwrap());
            signers.push('\n');
        }
        check_signers(&signers).unwrap();
        std::fs::write(dir.path().join("allowed_signers"), signers).unwrap();
        live.create_dir_all(&dir.path().join("scratch"), 0o700)
            .unwrap();
        Self { dir }
    }

    fn key(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn signers(&self) -> PathBuf {
        self.dir.path().join("allowed_signers")
    }

    fn scratch(&self) -> PathBuf {
        self.dir.path().join("scratch")
    }

    fn seal(&self, e: &Envelope, key: &str) -> Vec<u8> {
        let mut w = seal_batch(
            &Live::new(),
            std::slice::from_ref(e),
            &self.key(key),
            Signing::Unattended,
            &self.scratch(),
            None,
        )
        .unwrap();
        w.pop().unwrap()
    }

    fn open(&self, wire: &[u8], uuid: &str, floor: u64) -> Result<Opened, Refusal> {
        open_in(
            &Live::new(),
            wire,
            &self.signers(),
            uuid,
            &|_| floor,
            &self.scratch(),
        )
    }
}

#[test]
fn a_real_signature_opens_and_every_tampering_is_refused() {
    let keys = Keys::new();
    let now = Live::new().now();
    let e = envelope("UUID-1", 7, now);
    let wire = keys.seal(&e, "signing");
    let opened = keys.open(&wire, "UUID-1", 6).unwrap();
    assert_eq!(opened.envelope, e);
    assert_eq!(opened.principal, "signing@test");
    assert!(opened.key_id.starts_with("SHA256:"), "{}", opened.key_id);
    assert_eq!(std::fs::read_dir(keys.scratch()).unwrap().count(), 0);

    let mut tampered = wire.clone();
    let at = tampered.len() - 3;
    tampered[at] ^= 1;
    assert!(
        matches!(
            keys.open(&tampered, "UUID-1", 0),
            Err(Refusal::BadSignature(_))
        ),
        "tampered"
    );
    let (sig, _) = split(&wire).unwrap();
    let mut swapped = sig.to_vec();
    swapped.extend_from_slice(body_text(&envelope("UUID-1", 99, now)).unwrap().as_bytes());
    assert!(matches!(
        keys.open(&swapped, "UUID-1", 0),
        Err(Refusal::BadSignature(_))
    ));
    assert_eq!(
        keys.open(&wire, "UUID-2", 0),
        Err(Refusal::WrongHost {
            named: "UUID-1".into()
        })
    );
    assert_eq!(
        keys.open(&wire, "UUID-1", 7),
        Err(Refusal::Replay {
            principal: "signing@test".into(),
            serial: 7,
            floor: 7
        })
    );
    let mut old = envelope("UUID-1", 8, now - 100_000);
    old.expires = now - 10;
    let err = keys
        .open(&keys.seal(&old, "signing"), "UUID-1", 0)
        .unwrap_err();
    assert!(matches!(err, Refusal::Expired { .. }), "{err}");
    assert!(err.to_string().contains("host-clock="), "{err}");
    let ahead = envelope("UUID-1", 9, now + SKEW_SECS + 120);
    assert!(matches!(
        keys.open(&keys.seal(&ahead, "signing"), "UUID-1", 0),
        Err(Refusal::Expired { .. })
    ));
    let err = keys
        .open(&keys.seal(&e, "outsider"), "UUID-1", 0)
        .unwrap_err();
    assert!(matches!(err, Refusal::BadSignature(_)), "{err}");
}

/// A host's UUID is hex, which `is_uuid` reads in either case: an envelope for the
/// lower-case spelling opens on a host that reads its own in upper case. NEGATIVE CONTROL:
/// another UUID is still the wrong host.
#[test]
fn an_envelope_for_the_host_s_uuid_in_another_case_opens() {
    let keys = Keys::new();
    let now = Live::new().now();
    let lower = "0a1b2c3d-0000-4000-8000-00000000abcd";
    let wire = keys.seal(&envelope(lower, 1, now), "signing");
    let opened = keys
        .open(&wire, "0A1B2C3D-0000-4000-8000-00000000ABCD", 0)
        .unwrap();
    assert_eq!(opened.envelope.host, lower);
    assert_eq!(
        keys.open(&wire, "0A1B2C3D-0000-4000-8000-00000000ABCE", 0),
        Err(Refusal::WrongHost {
            named: lower.into()
        })
    );
}

#[test]
fn a_signature_in_another_namespace_is_refused() {
    let keys = Keys::new();
    let now = Live::new().now();
    let body = body_text(&envelope("UUID-1", 1, now)).unwrap();
    let file = keys.scratch().join("other-ns");
    std::fs::write(&file, &body).unwrap();
    let ran = Live::new()
        .run(
            &Cmd::new(SSH_KEYGEN)
                .args(["-Y", "sign", "-f"])
                .arg(keys.key("signing"))
                .args(["-n", "git"])
                .arg(&file),
        )
        .unwrap();
    assert!(ran.success(), "{}", ran.describe());
    let mut wire = std::fs::read(sig_path(&file)).unwrap();
    wire.extend_from_slice(body.as_bytes());
    std::fs::remove_file(&file).unwrap();
    std::fs::remove_file(sig_path(&file)).unwrap();
    let err = keys.open(&wire, "UUID-1", 0).unwrap_err();
    assert!(matches!(err, Refusal::BadSignature(_)), "{err}");
}

#[test]
fn a_passphrase_key_is_told_apart_from_an_open_one() {
    let keys = Keys::new();
    let live = Live::new();
    assert_eq!(
        needs_passphrase(&live, &keys.key("signing")),
        Read::Value(false)
    );
    assert_eq!(
        needs_passphrase(&live, &keys.key("recovery")),
        Read::Value(true)
    );
    assert!(matches!(
        needs_passphrase(&live, &keys.key("missing")),
        Read::Unusable(_)
    ));
}

#[test]
fn a_batch_signed_by_one_run_opens_envelope_by_envelope() {
    let keys = Keys::new();
    let now = Live::new().now();
    let bodies: Vec<Envelope> = (1..=3)
        .map(|i| envelope(&format!("H{i}"), i, now))
        .collect();
    let wires = seal_batch(
        &Live::new(),
        &bodies,
        &keys.key("signing"),
        Signing::Unattended,
        &keys.scratch(),
        None,
    )
    .unwrap();
    for (i, wire) in wires.iter().enumerate() {
        let opened = keys.open(wire, &format!("H{}", i + 1), 0).unwrap();
        assert_eq!(opened.envelope, bodies[i]);
    }
}

/// INVERTED VALIDITY WINDOW IS REFUSED WITHIN CLOCK SKEW: an envelope with `expires < issued`
/// cannot slip through `open_in` when `now <= expires < issued <= now + SKEW_SECS`, nor can
/// `seal_batch` sign it.
/// NEGATIVE CONTROL: a non-inverted window (`expires == issued`) within `SKEW_SECS` opens.
#[test]
fn an_inverted_validity_window_is_refused_in_both_seal_batch_and_open_in() {
    let mut fake = Fake::root();
    fake.now = 1_850;
    private(&fake, "/s");
    script_good(&fake, "/s");

    let mut inverted = envelope("U", 5, 2_000);
    inverted.expires = 1_900;
    let seal_err = seal_batch(
        &fake,
        std::slice::from_ref(&inverted),
        Path::new("/k/signing"),
        Signing::Unattended,
        Path::new("/s"),
        None,
    )
    .expect_err("seal_batch must refuse expires < issued");
    assert!(seal_err.contains("before issued"), "{seal_err}");
    assert!(
        !fake.calls().iter().any(|c| c.contains("-Y sign")),
        "ssh-keygen must not run on an inverted window: {:?}",
        fake.calls()
    );

    let inverted_wire = format!("{SIG}{}", body_text(&inverted).unwrap());
    let open_err = open_in(
        &fake,
        inverted_wire.as_bytes(),
        Path::new("/as"),
        "U",
        &|_| 0,
        Path::new("/s"),
    )
    .expect_err("open_in must refuse expires < issued even within SKEW_SECS");
    assert_eq!(
        open_err,
        Refusal::Expired {
            issued: 2_000,
            expires: 1_900,
            now: 1_850,
        }
    );

    script_good(&fake, "/s");
    let mut instant = envelope("U", 6, 2_000);
    instant.expires = 2_000;
    let instant_wire = format!("{SIG}{}", body_text(&instant).unwrap());
    let opened = open_in(
        &fake,
        instant_wire.as_bytes(),
        Path::new("/as"),
        "U",
        &|_| 0,
        Path::new("/s"),
    )
    .expect("expires == issued within SKEW_SECS remains valid");
    assert_eq!(opened.envelope.serial, 6);
}
