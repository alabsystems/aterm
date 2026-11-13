// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::alab_roster::{self, LabMailbox, Part};
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

/// The registry decides before a file: an `ai-mailbox.toml` entry for the same login fills
/// only what the registry leaves empty, and an entry for another login stays a fallback for
/// the addresses only it receives (until B13).
/// NEGATIVE CONTROL: with `merge_lab` appending the registry's routes after the files', the
/// shared address `ai1@example.test` resolves to the file's other mailbox.
#[test]
fn the_registry_decides_before_the_mailbox_file() {
    let home = bare_home();
    let secrets_dir = home.path().join(".secrets");
    std::fs::create_dir_all(&secrets_dir).unwrap();
    std::fs::write(
        secrets_dir.join(SECRETS_MAILBOX_TOML_FILE),
        "[[mailboxes]]\n\
         login = \"other@example.test\"\n\
         password = \"fixture-file-other\"\n\
         webmail_url = \"https://other.example.test/inbox\"\n\
         receives = [\"ai1@example.test\", \"ai2@example.test\"]\n\
         [[mailboxes]]\n\
         login = \"OWNER@example.test\"\n\
         password = \"fixture-file-owner\"\n\
         totp_secret = \"fixture-file-seed\"\n\
         receives = [\"ai3@example.test\"]\n", // gitleaks:allow
    )
    .unwrap();
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

/// `VendorApprover::new`, which every sign-in driver (`login_drive`, `grok_signin`) builds,
/// finds the registry's mailbox for its row with no file present.
#[test]
fn a_vendor_sign_in_takes_the_registry_mailbox() {
    let home = bare_home();
    let shared = home.path().join("store");
    alab_roster::with_mail(vec![mailbox()], secrets(), || {
        let approver = VendorApprover::new(
            super::super::accounts::Cli::Claude,
            "claude-1",
            Some("ai1@example.test"),
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
