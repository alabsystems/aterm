// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::alab_roster::{self, LabMailbox, Part};
use super::vendor_tests::mailbox_paths;
use super::*;

/// RFC 6238's SHA-1 test seed, `12345678901234567890` in Base32.
pub(in super::super) const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"; // gitleaks:allow

/// The registry's mailbox `mb-fixture`: login `owner@example.test`, webmail only, receiving
/// `ai1@example.test` and all of `pm.example.test`, its three credentials held.
pub(in super::super) fn mailbox() -> LabMailbox {
    LabMailbox {
        pk: "mb-fixture".into(),
        login: "owner@example.test".into(),
        service: Some("other".into()),
        webmail: Some("https://mail.example.test/inbox".into()),
        receives: vec!["ai1@example.test".into(), "*@pm.example.test".into()],
        held: vec![Part::Password, Part::MailboxPassword, Part::Totp],
    }
}

/// What the registry's cells open to for [`mailbox`].
pub(in super::super) fn secrets() -> Vec<((&'static str, Part), &'static str)> {
    vec![
        (("mb-fixture", Part::Password), "fixture-login-pass"), // gitleaks:allow
        (("mb-fixture", Part::MailboxPassword), "fixture-second-pass"), // gitleaks:allow
        (("mb-fixture", Part::Totp), SEED),
    ]
}

/// An empty HOME: no mailbox file, no sheet, no `~/.secrets`.
fn bare_home() -> aterm_tempfile::TempDir {
    let home = aterm_tempfile::tempdir().unwrap();
    assert!(!home.path().join(".secrets").exists());
    home
}

/// With no file present, discovery finds the registry's mailbox for the addresses it
/// receives, by its login; the value getters reveal its login password, second password and
/// TOTP seed, each once per discovery, and the seed yields the sign-in code RFC 6238 gives.
/// NEGATIVE CONTROL: with `merge_lab` left out of `discover_mailbox_config_sources`, the
/// discovery is not configured (`None`) and the first assertion fails.
#[test]
fn a_registry_mailbox_resolves_its_login_passwords_and_totp_code_with_no_file() {
    let home = bare_home();
    alab_roster::with_mail(vec![mailbox()], secrets(), || {
        let cfg = discover_mailbox_config_files(Some(home.path()), None, None)
            .expect("the registry's mailbox configures discovery");
        for email in [
            "ai1@example.test",
            "ai1+grok@example.test",
            "x@pm.example.test",
        ] {
            let target = Some(email);
            assert_eq!(
                cfg.webmail_user_for(target).as_deref(),
                Some("owner@example.test"),
                "{email}"
            );
            assert!(cfg.is_configured_for(target), "{email}");
            assert_eq!(
                cfg.effective_webmail_url(target).as_deref(),
                Some("https://mail.example.test/inbox")
            );
        }
        assert_eq!(
            alab_roster::reveals(),
            0,
            "routing and presence reveal nothing"
        );
        let target = Some("ai1@example.test");
        assert_eq!(
            cfg.password_for(target).as_deref(),
            Some("fixture-login-pass")
        );
        assert_eq!(
            cfg.mailbox_password_for(target).as_deref(),
            Some("fixture-second-pass")
        );
        let seed = cfg.totp_secret_for(target).unwrap();
        assert_eq!(generate_totp_at(&seed, 59).as_deref(), Some("287082"));
        assert_eq!(alab_roster::reveals(), 3);
        // A poller reads the routed view, as the webmail reader does each step.
        let view = cfg.routed_for(target);
        for _ in 0..3 {
            assert_eq!(
                view.password_for(target).as_deref(),
                Some("fixture-login-pass")
            );
            assert_eq!(
                view.totp_secret_for(target)
                    .and_then(|seed| generate_totp_at(&seed, 59)),
                Some("287082".to_owned())
            );
        }
        assert_eq!(
            alab_roster::reveals(),
            3,
            "one discovery reveals each part once"
        );
        assert!(
            cfg.route_for(Some("stranger@other.test")).is_none(),
            "an address the registry's mailbox does not receive has no route"
        );
    });
}

/// The upkeep's look at a row (`route_for`, `routed_for`, `is_configured_for`,
/// `masked_login_for`, `holds_for`, the cleartext refusal) reveals nothing; a part the
/// registry does not hold is none; a mailbox whose password the registry lacks is unready
/// and never tried.
/// NEGATIVE CONTROL: with `routed_for` filling its password from `self.password_for`, the
/// first `reveals() == 0` assertion reads 3.
#[test]
fn looking_at_a_registry_mailbox_reveals_nothing() {
    let home = bare_home();
    let mut partial = mailbox();
    partial.held = vec![Part::Password];
    alab_roster::with_mail(vec![partial], secrets(), || {
        let cfg = discover_mailbox_config_files(Some(home.path()), None, None).unwrap();
        let target = Some("ai1@example.test");
        let route = cfg.route_for(target).expect("routed");
        assert!(route.ready() && route.registry.is_some());
        let view = cfg.routed_for(target);
        assert!(view.is_configured_for(target));
        assert!(view.masked_login_for(target).is_some());
        assert!(cfg.holds_for(Part::Password, target));
        assert!(!cfg.holds_for(Part::Totp, target));
        assert!(plaintext_credential_refusal(&cfg, target).is_none());
        assert_eq!(alab_roster::reveals(), 0);
        assert_eq!(cfg.totp_secret_for(target), None, "a part not held is none");
        assert_eq!(alab_roster::reveals(), 0);
    });
    let mut unready = mailbox();
    unready.held.clear();
    alab_roster::with_mail(vec![unready], Vec::new(), || {
        let cfg = discover_mailbox_config_files(Some(home.path()), None, None).unwrap();
        let target = Some("ai1@example.test");
        assert!(!cfg.is_configured_for(target));
        assert_eq!(
            cfg.unready_words(target),
            Some("mailbox ow…@… has no password yet")
        );
        assert_eq!(cfg.password_for(target), None);
        assert_eq!(alab_roster::reveals(), 0);
    });
}

/// The registry's mailbox `mb-fixture` as a Proton one: a route the window reads by itself,
/// with the login's password, where the registry holds one.
fn proton_mailbox() -> LabMailbox {
    LabMailbox {
        service: Some("proton".into()),
        webmail: Some("https://mail.proton.me/u/0/inbox".into()),
        held: vec![Part::Password],
        ..mailbox()
    }
}

/// On a fresh discovery, the value-free look answers exactly as the getter a sign-in reads
/// would, on the configuration (by its route) and on its routed view (by its registry
/// mailbox): none where the getter returns none, else whether the value holds a character
/// other than whitespace. Returns that answer.
fn look_matches_the_getter(home: &std::path::Path, target: Option<&str>) -> Option<bool> {
    let looked = discover_mailbox_config_files(Some(home), None, None).unwrap();
    let read = discover_mailbox_config_files(Some(home), None, None).unwrap();
    let usable = |value: String| !value.chars().all(char::is_whitespace);
    let by_route = looked.password_standing(target);
    assert_eq!(by_route, read.password_for(target).map(usable));
    let by_view = looked.routed_for(target).password_standing(target);
    assert_eq!(
        by_view,
        read.routed_for(target).password_for(target).map(usable)
    );
    assert_eq!(by_route, by_view);
    by_route
}

/// Whether the window reads a registry mailbox by itself, the question every accounts upkeep
/// asks of every signed-in row, is answered with nothing revealed: the plan asks the registry
/// what the password holds, once a discovery, and the answer follows the value a sign-in
/// would be handed. Only the sign-in's own read of the password is a reveal, and what a
/// discovery revealed answers its later looks.
/// The look answers as the getter would for each of them ([`look_matches_the_getter`]).
/// NEGATIVE CONTROLS: a password the registry lists and this Mac cannot open (withheld), one
/// of whitespace alone and an empty one each read as no mailbox, so the judge is not the
/// `held` list; with `native_mail::plan` reading `password_for` (before this change) the
/// first `reveals() == 0` reads 1.
#[test]
fn judging_whether_the_window_reads_a_registry_mailbox_reveals_nothing() {
    let home = bare_home();
    let target = Some("ai1@example.test");
    let reads = |cfg: &MailboxConfig| cfg.window_reads_with(target, &|_| GoogleGrant::Absent);
    // Only the unix plan asks the registry: elsewhere the judge reads no native route.
    let asked = |looks: usize| if cfg!(unix) { looks } else { 0 };
    let password = (("mb-fixture", Part::Password), "fixture-login-pass"); // gitleaks:allow
    alab_roster::with_mail(vec![proton_mailbox()], vec![password], || {
        let cfg = discover_mailbox_config_files(Some(home.path()), None, None).unwrap();
        // Proton's own API is counted on macOS alone.
        assert_eq!(reads(&cfg), cfg!(target_os = "macos"));
        assert_eq!(reads(&cfg), cfg!(target_os = "macos"));
        assert_eq!(alab_roster::reveals(), 0, "the judge reveals nothing");
        assert_eq!(alab_roster::looks(), asked(1), "and asks once a discovery");
        #[cfg(unix)]
        assert!(matches!(
            native_mail::plan(&cfg, target, |_| Ok(None::<()>)),
            native_mail::Plan::Proton { login } if login == "owner@example.test"
        ));
        assert_eq!(
            (alab_roster::reveals(), alab_roster::looks()),
            (0, asked(1))
        );

        // The next upkeep pass makes its own discovery: one more look, still no reveal.
        let next = discover_mailbox_config_files(Some(home.path()), None, None).unwrap();
        assert_eq!(reads(&next), cfg!(target_os = "macos"));
        assert_eq!(
            (alab_roster::reveals(), alab_roster::looks()),
            (0, asked(2))
        );

        // The sign-in's read is the one reveal, and it answers the discovery's later looks.
        let signing_in = discover_mailbox_config_files(Some(home.path()), None, None).unwrap();
        assert_eq!(
            signing_in
                .routed_for(target)
                .password_for(target)
                .as_deref(),
            Some("fixture-login-pass")
        );
        assert_eq!(reads(&signing_in), cfg!(target_os = "macos"));
        assert_eq!(
            (alab_roster::reveals(), alab_roster::looks()),
            (1, asked(2))
        );
        assert_eq!(look_matches_the_getter(home.path(), target), Some(true));
    });
    for (what, secrets, standing) in [
        ("withheld", Vec::new(), None),
        (
            "blank",
            vec![(("mb-fixture", Part::Password), " \t ")],
            Some(false),
        ),
        ("empty", vec![(("mb-fixture", Part::Password), "")], None),
    ] {
        alab_roster::with_mail(vec![proton_mailbox()], secrets, || {
            let cfg = discover_mailbox_config_files(Some(home.path()), None, None).unwrap();
            assert!(
                cfg.holds_for(Part::Password, target),
                "{what}: listed as held"
            );
            assert!(!reads(&cfg), "{what}");
            #[cfg(unix)]
            assert!(
                matches!(
                    native_mail::plan(&cfg, target, |_| Ok(None::<()>)),
                    native_mail::Plan::Defer
                ),
                "{what}"
            );
            assert_eq!(
                (alab_roster::reveals(), alab_roster::looks()),
                (0, asked(1)),
                "{what}"
            );
            assert_eq!(
                look_matches_the_getter(home.path(), target),
                standing,
                "{what}"
            );
        });
    }
}

/// The registry decides before a file: an `ai-mailbox.toml` entry for the same login fills
/// only what the registry leaves empty, and an entry for another login stays a fallback for
/// the addresses only it receives (until B13).
/// NEGATIVE CONTROL: with `merge_lab` appending the registry's routes after the files', the
/// shared address `ai1@example.test` resolves to the file's other mailbox.
#[test]
fn the_registry_decides_before_the_mailbox_file() {
    for &mailbox_path in mailbox_paths() {
        let home = bare_home();
        let text = b"[[mailboxes]]\n\
         login = \"other@example.test\"\n\
         password = \"fixture-file-other\"\n\
         webmail_url = \"https://other.example.test/inbox\"\n\
         receives = [\"ai1@example.test\", \"ai2@example.test\"]\n\
         [[mailboxes]]\n\
         login = \"OWNER@example.test\"\n\
         password = \"fixture-file-owner\"\n\
         totp_secret = \"fixture-file-seed\"\n\
         receives = [\"ai3@example.test\"]\n"; // gitleaks:allow
        if let Some(entry) = mailbox_path.strip_prefix(".secrets/") {
            #[cfg(unix)]
            crate::vault_entry::seed(home.path(), &[(entry, text)]);
            #[cfg(not(unix))]
            {
                let _ = entry;
                panic!("this fixture source requires the Unix vault");
            }
        } else {
            let path = home.path().join(mailbox_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            aterm_uds::atomic_file::write(&path, text, 0o600).unwrap();
        }
        let mut registry = mailbox();
        registry.held = vec![Part::Password];
        alab_roster::with_mail(vec![registry], secrets(), || {
            let cfg = discover_mailbox_config_files(Some(home.path()), None, None).unwrap();
            assert_eq!(
                cfg.webmail_user_for(Some("ai1@example.test")).as_deref(),
                Some("owner@example.test"),
                "the registry's route comes first among equal patterns"
            );
            assert_eq!(
                cfg.password_for(Some("ai1@example.test")).as_deref(),
                Some("fixture-login-pass"),
                "the registry's held password, not the file's"
            );
            assert_eq!(
                cfg.totp_secret_for(Some("ai1@example.test")).as_deref(),
                Some("fixture-file-seed"),
                "a part the registry holds no cell for falls back to the file's entry"
            );
            assert_eq!(
                cfg.webmail_user_for(Some("ai2@example.test")).as_deref(),
                Some("other@example.test"),
                "an address only the file routes keeps its route"
            );
            assert_eq!(
                cfg.mailboxes
                    .iter()
                    .filter(|m| m.login.eq_ignore_ascii_case("owner@example.test"))
                    .count(),
                1,
                "one login is one route"
            );
        });
    }
}

/// `VendorApprover::new`, which every sign-in driver (`login_drive`, `grok_signin`) builds,
/// finds the registry's mailbox for its row with no file present.
#[test]
fn a_vendor_sign_in_takes_the_registry_mailbox() {
    let home = bare_home();
    let shared = home.path().join("store");
    alab_roster::with_mail(vec![mailbox()], secrets(), || {
        let approver = VendorApprover::new(
            super::super::accounts::Cli::Claude,
            (
                &super::super::accounts::Row {
                    label: "claude-1".to_owned(),
                    email: Some("ai1@example.test".to_owned()),
                    ..super::super::accounts::Row::default()
                },
                None,
            ),
            &shared,
            Some(home.path()),
        );
        let mailbox = approver.mailbox.as_ref().expect("a mailbox for the row");
        assert_eq!(
            mailbox.password_for(Some("ai1@example.test")).as_deref(),
            Some("fixture-login-pass")
        );
        assert_eq!(approver.mailbox_unready, None);
    });
}
