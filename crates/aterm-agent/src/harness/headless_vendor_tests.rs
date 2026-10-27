// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the vendor sign-in pages' decision core ([`vendor_step`], a pure page model), the
//! vendors' mail senders, the per-CLI rosters' label aliases and the fleet rosters' exemption
//! from the `~/.secrets` mailbox catch-all.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::ai::connector::Completion;

const KIMI_GLOBAL: &[&str] = &["auth.kimi.ai", "www.kimi.ai"];
const CODE: &str = "QXR4-MZK7";
const SIGN_IN_ONCE: &str = "aterm harness accounts browser kimi-one --cli kimi";

fn device_gate() -> Gate<'static> {
    Gate {
        hosts: KIMI_GLOBAL,
        completion: Completion::Device,
        code: Some(CODE),
        email: Some("kimi-one@example.com"),
        senders: &["kimi", "moonshot"],
        otp: None,
        pressed: 0,
        confirmed: false,
        filled_code: false,
        filled_email: false,
        filled_otp: false,
        bound: None,
        sign_in_once: None,
        types_email: true,
    }
}

fn page(url: &str, heading: &str, text: &str, buttons: &[&str], inputs: &[Field]) -> Page {
    Page {
        url: url.to_owned(),
        heading: heading.to_owned(),
        text: text.to_owned(),
        buttons: buttons.iter().map(|b| (*b).to_owned()).collect(),
        inputs: inputs.to_vec(),
    }
}

fn device_page(url: &str, code: &str) -> Page {
    page(
        url,
        "authorize device",
        &format!("Signed in as kimi-one@example.com\nConfirm the code below\n{code}\nCancel"),
        &["cancel", "confirm"],
        &[],
    )
}

#[test]
fn a_device_page_on_its_pinned_host_showing_the_printed_code_is_confirmed_once() {
    let gate = device_gate();
    let shown = device_page("https://auth.kimi.ai/device?user_code=QXR4-MZK7", CODE);
    assert_eq!(
        vendor_step(&shown, &gate),
        Step::Press(1),
        "confirm, never cancel"
    );
    let confirmed = Gate {
        confirmed: true,
        ..gate
    };
    assert_eq!(
        vendor_step(&shown, &confirmed),
        Step::Wait,
        "one confirm press, then the CLI is waited for"
    );
    let boxed = page(
        "https://auth.kimi.ai/device",
        "authorize device",
        "Confirm this code: Q X R 4 - M Z K 7",
        &["confirm"],
        &[],
    );
    assert_eq!(
        vendor_step(&boxed, &gate),
        Step::Press(0),
        "a boxed code still reads as it"
    );
}

#[test]
fn the_same_page_off_its_pins_or_showing_another_code_is_never_pressed() {
    let gate = device_gate();
    let off = device_page("https://auth.kimi.com/device", CODE);
    assert!(
        matches!(vendor_step(&off, &gate), Step::Stop(_)),
        "a global row's page on kimi.com is off its pins"
    );
    let lookalike = device_page("https://auth.kimi.ai.example.net/device", CODE);
    assert!(matches!(vendor_step(&lookalike, &gate), Step::Stop(_)));
    let plain = device_page("http://auth.kimi.ai/device", CODE);
    assert!(matches!(vendor_step(&plain, &gate), Step::Stop(_)));
    let other = device_page("https://auth.kimi.ai/device", "ZZT8-KQW2");
    assert!(
        matches!(vendor_step(&other, &gate), Step::Refuse(_)),
        "{:?}",
        vendor_step(&other, &gate)
    );
}

#[test]
fn a_sign_up_page_another_idp_a_password_or_another_account_stops_the_sign_in() {
    let gate = device_gate();
    let sign_up = page(
        "https://www.kimi.ai/signup",
        "create your account",
        "Create your account",
        &["continue"],
        &[Field::Email],
    );
    assert!(matches!(vendor_step(&sign_up, &gate), Step::Stop(_)));
    let headed = page(
        "https://www.kimi.ai/welcome",
        "sign up",
        "Sign up to continue",
        &["continue"],
        &[],
    );
    assert!(matches!(vendor_step(&headed, &gate), Step::Stop(_)));
    let google = page(
        "https://accounts.google.com/o/oauth2/auth",
        "sign in",
        "Choose an account",
        &["continue"],
        &[],
    );
    match vendor_step(&google, &gate) {
        Step::SignIn(why) => assert!(why.contains("another identity provider"), "{why}"),
        step => panic!("{step:?}"),
    }
    let password = page(
        "https://auth.kimi.ai/login",
        "sign in",
        "Password",
        &["sign in"],
        &[Field::Password],
    );
    assert!(matches!(vendor_step(&password, &gate), Step::SignIn(_)));
    let someone = page(
        "https://auth.kimi.ai/device",
        "authorize device",
        &format!("Signed in as someone-else@example.com. Not you?\n{CODE}"),
        &["confirm"],
        &[],
    );
    assert!(matches!(vendor_step(&someone, &gate), Step::Refuse(_)));
}

#[test]
fn an_email_page_is_filled_its_mail_read_and_a_loopback_callback_ends_a_browser_flow() {
    let gate = device_gate();
    let email = page(
        "https://auth.kimi.ai/login",
        "sign in",
        "Continue with email",
        &["continue", "continue with google"],
        &[Field::Email],
    );
    assert_eq!(vendor_step(&email, &gate), Step::FillEmail);
    let sent = Gate {
        filled_email: true,
        ..gate
    };
    let otp = page(
        "https://auth.kimi.ai/verify",
        "check your email",
        "We sent a verification code to kimi-one@example.com",
        &["verify"],
        &[Field::Code],
    );
    assert_eq!(vendor_step(&otp, &sent), Step::Mailbox);
    let got = Gate {
        otp: Some("482913"),
        ..sent
    };
    assert_eq!(vendor_step(&otp, &got), Step::FillOtp);
    let ask = page(
        "https://auth.kimi.ai/device",
        "enter code",
        "Enter the code shown in your terminal",
        &["continue"],
        &[Field::Code],
    );
    assert_eq!(vendor_step(&ask, &gate), Step::FillCode);
    let browser = Gate {
        hosts: &["auth.openai.com"],
        completion: Completion::Loopback,
        code: None,
        ..gate
    };
    let consent = page(
        "https://auth.openai.com/sign-in-with-chatgpt/codex/consent",
        "sign in to codex",
        "Codex wants to access your account kimi-one@example.com",
        &["cancel", "continue"],
        &[],
    );
    assert_eq!(vendor_step(&consent, &browser), Step::Press(1));
    let spent = Gate {
        pressed: MAX_CONSENT_PRESSES,
        ..browser
    };
    assert_eq!(vendor_step(&consent, &spent), Step::Wait);
    let back = page(
        "http://127.0.0.1:1455/auth/callback?code=x",
        "",
        "",
        &[],
        &[],
    );
    assert_eq!(vendor_step(&back, &browser), Step::Done);
    assert!(
        matches!(vendor_step(&back, &gate), Step::Refuse(_)),
        "a device sign-in has no callback"
    );
    let unprompted = page(
        "https://auth.kimi.ai/device",
        "authorize device",
        "Authorize this device?",
        &["authorize"],
        &[],
    );
    assert_eq!(
        vendor_step(&unprompted, &gate),
        Step::Wait,
        "a device page shows its code before anything is pressed"
    );
}

#[test]
fn a_vendors_webmail_rows_are_chosen_by_its_senders() {
    let claude = webmail_step_js(
        (None, None, None, None),
        (false, false, false, false),
        0,
        &[],
    );
    assert!(
        claude.contains(r#"["claude","anthropic"]"#),
        "Claude Code's, by default"
    );
    let kimi = webmail_step_js(
        (None, None, None, None),
        (false, false, false, false),
        0,
        &["Kimi".to_owned(), "moonshot".to_owned()],
    );
    assert!(kimi.contains(r#"["kimi","moonshot"]"#));
    assert!(!kimi.contains(r#"["claude","anthropic"]"#));
}

fn home_with(files: &[(&str, &str)]) -> aterm_tempfile::TempDir {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    for (path, text) in files {
        let at = tmp.path().join(path);
        std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
        std::fs::write(at, text).expect("write");
    }
    tmp
}

#[test]
fn a_clis_roster_aliases_its_labels_to_their_emails() {
    let home = home_with(&[
        (
            ".secrets/ai-mailbox.toml",
            "[passwords]\nkimi-one = \"fixture-pw\"\n",
        ),
        (
            ".config/aterm/accounts-kimi.toml",
            "[[account]]\nlabel = \"kimi-one\"\nclass = \"dedicated\"\n\
             email = \"kimi-one@example.com\"\nregion = \"global\"\n",
        ),
    ]);
    let cfg = discover_mailbox_config_files_raw(Some(home.path()), None, None);
    assert_eq!(
        cfg.password_for(Some("kimi-one@example.com")).as_deref(),
        Some("fixture-pw")
    );
    let bare = home_with(&[(
        ".secrets/ai-mailbox.toml",
        "[passwords]\nkimi-one = \"fixture-pw\"\n",
    )]);
    let cfg = discover_mailbox_config_files_raw(Some(bare.path()), None, None);
    assert!(
        !cfg.passwords.contains_key("kimi-one@example.com"),
        "no roster, no alias"
    );
}

#[test]
fn a_clis_fleet_roster_in_secrets_is_no_mailbox_fragment_and_a_machines_file_still_is() {
    let roster = home_with(&[(
        ".secrets/ai-accounts-m16.toml",
        "password = \"fixture-pw\"\n",
    )]);
    let cfg = discover_mailbox_config_files_raw(Some(roster.path()), None, None);
    assert!(
        !cfg.passwords.values().any(|p| p == "fixture-pw"),
        "ai-accounts-* is fleet data: {:?}",
        cfg.passwords.keys().collect::<Vec<_>>()
    );
    let machine = home_with(&[(".secrets/m16.toml", "password = \"fixture-pw\"\n")]);
    let cfg = discover_mailbox_config_files_raw(Some(machine.path()), None, None);
    assert!(
        cfg.passwords.values().any(|p| p == "fixture-pw"),
        "a machine's own file is still read"
    );
}

/// A CLI'S FLEET in `~/.secrets` (its `ai-accounts-<cli>.toml`, else its column of the sheet)
/// aliases its labels to their emails as its roster does, so a password filed under the label
/// reaches the sign-in of that email. CONTROL: the same password with no fleet naming the
/// label is no alias.
#[test]
fn a_clis_fleet_in_secrets_aliases_its_labels_to_their_emails() {
    let mailbox = (
        ".secrets/ai-mailbox.toml",
        "[passwords]\nkimi-1 = \"fixture-pw\"\nkimi-m16 = \"sheet-pw\"\n",
    );
    let fleet = home_with(&[
        mailbox,
        (
            ".secrets/ai-accounts-kimi.toml",
            "[[account]]\nlabel = \"kimi-1\"\nemail = \"kimi-one@example.com\"\n\
             machines = [\"m16\"]\nregion = \"global\"\n",
        ),
    ]);
    let cfg = discover_mailbox_config_files_raw(Some(fleet.path()), None, None);
    assert_eq!(
        cfg.password_for(Some("kimi-one@example.com")).as_deref(),
        Some("fixture-pw")
    );
    let sheet = home_with(&[
        mailbox,
        (
            ".secrets/ai-accounts.csv",
            "email,computer,claude,kimi\nkimi-sheet@example.com,m16,,global\n",
        ),
    ]);
    let cfg = discover_mailbox_config_files_raw(Some(sheet.path()), None, None);
    assert_eq!(
        cfg.password_for(Some("kimi-sheet@example.com")).as_deref(),
        Some("sheet-pw"),
        "the sheet's kimi column labels its row kimi-m16"
    );
    let bare = home_with(&[mailbox]);
    let cfg = discover_mailbox_config_files_raw(Some(bare.path()), None, None);
    assert!(
        !cfg.passwords.contains_key("kimi-one@example.com")
            && !cfg.passwords.contains_key("kimi-sheet@example.com"),
        "CONTROL: no fleet, no alias"
    );
}

/// Two Kimi rows on one mailbox by plus-addressing are two vendor accounts: the second row's
/// sign-in never runs in the first row's browser profile, is refused on a page signed in as the
/// first, and is proved only by a page that names its own exact email as the one signed in.
#[test]
fn a_plus_addressed_row_is_its_own_account_in_its_profile_its_refusals_and_its_proof() {
    let store = aterm_tempfile::tempdir().expect("tmp");
    let base = vendor_profile_dir(store.path(), "kimi-kimi-1", Some("owner@example.com"));
    std::fs::create_dir_all(&base).expect("kimi-1's signed-in profile");
    let plus = vendor_profile_dir(store.path(), "kimi-kimi-2", Some("owner+2@example.com"));
    assert_ne!(plus, base, "kimi-2 never signs in through kimi-1's profile");
    assert_ne!(
        plus,
        vendor_profile_dir(store.path(), "x", Some("owner2@example.com")),
        "a `+` is no character the profile key may drop"
    );
    assert_eq!(
        vendor_profile_dir(store.path(), "x", Some("Owner@Example.com")),
        base,
        "one email, one profile, whatever its case"
    );

    let gate = Gate {
        email: Some("owner+2@example.com"),
        ..device_gate()
    };
    let as_base = page(
        "https://auth.kimi.ai/device?user_code=QXR4-MZK7",
        "authorize device",
        &format!("Signed in as owner@example.com\nConfirm the code below\n{CODE}\nCancel"),
        &["cancel", "confirm"],
        &[],
    );
    assert!(matches!(vendor_step(&as_base, &gate), Step::Refuse(_)));
    assert!(!shows_email(&as_base, &gate));

    // Negative: the row's own account signed in is confirmed and proves the row; a page that
    // only echoes the email aterm typed proves nothing.
    let own = page(
        "https://auth.kimi.ai/device?user_code=QXR4-MZK7",
        "authorize device",
        &format!("Signed in as owner+2@example.com\nConfirm the code below\n{CODE}\nCancel"),
        &["cancel", "confirm"],
        &[],
    );
    assert_eq!(vendor_step(&own, &gate), Step::Press(1));
    assert!(shows_email(&own, &gate));
    let echo = page(
        "https://www.kimi.ai/login",
        "sign in",
        "We sent a verification code to owner+2@example.com",
        &["continue"],
        &[Field::Code],
    );
    assert!(!shows_email(&echo, &gate));
    // The vendor's own address beside the row's is no other account; `x.ai` names no `max.ai`.
    let support = page(
        "https://auth.kimi.ai/device",
        "authorize device",
        &format!("Signed in as owner+2@example.com\nHelp: support@kimi.ai\n{CODE}"),
        &["confirm"],
        &[],
    );
    assert!(shows_email(&support, &gate));
    assert!(domain_names("mail.x.ai", "x.ai") && !domain_names("max.ai", "x.ai"));
    assert!(domain_names("kimi.ai", "kimi") && !domain_names("kimikimi.ai", "kimi"));
}

fn maildir_with(mails: &[(&str, &str)]) -> (aterm_tempfile::TempDir, PathBuf) {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let maildir = tmp.path().join("Maildir");
    std::fs::create_dir_all(maildir.join("new")).expect("mkdir");
    for (name, text) in mails {
        std::fs::write(maildir.join("new").join(name), text).expect("mail");
    }
    (tmp, maildir)
}

const SINCE: i64 = 1_792_790_000;

/// Fri, 23 Oct 2026 23:20:00 +0000: after [`SINCE`].
const AFTER: &str = "Fri, 23 Oct 2026 23:20:00 +0000";

/// A vendor sign-in's mailbox read takes the vendor's own mail sent after the sign-in began,
/// and nothing else: another service's code is never typed into the vendor's page, nor marked
/// consumed.
#[test]
fn a_vendor_mailbox_read_takes_only_the_vendors_mail_sent_after_the_sign_in_began() {
    assert_eq!(parse_mail_date(AFTER), Some(1_792_797_600));
    assert_eq!(
        parse_mail_date("23 Oct 2026 16:20 -0700"),
        Some(1_792_797_600)
    );
    assert_eq!(
        parse_mail_date("Fri, 23 Oct 2026 23:20:00 GMT (UTC)"),
        Some(1_792_797_600)
    );
    let bank = format!(
        "From: Example Bank <alerts@bank.example>\r\nTo: <kimi-one@example.com>\r\nDate: {AFTER}\r\n\
         Subject: Your sign-in code\r\n\r\nYour verification code is 482913. Do not share it.\r\n"
    );
    let (tmp, maildir) = maildir_with(&[("1.eml", &bank)]);
    let vendor = |since: Option<i64>| MailboxConfig {
        dir: Some(maildir.clone()),
        seen_file: Some(tmp.path().join("seen")),
        senders: vec!["kimi".to_owned(), "moonshot".to_owned()],
        since,
        ..MailboxConfig::default()
    };
    let got = poll_mailbox_config_for_credential(
        &vendor(Some(SINCE)),
        Some("kimi-one@example.com"),
        Duration::from_millis(300),
    );
    assert!(got.is_err(), "bank.example is not Kimi's: {got:?}");
    // Negative: the refusal consumed nothing; a read with no vendor filter still finds it.
    let plain = MailboxConfig {
        senders: Vec::new(),
        since: None,
        ..vendor(None)
    };
    assert_eq!(
        poll_mailbox_config_for_credential(
            &plain,
            Some("kimi-one@example.com"),
            Duration::from_millis(300)
        ),
        Ok(MailboxCredential::Otp("482913".to_owned()))
    );

    let kimi = |date: &str| {
        format!(
            "From: Kimi <noreply@kimi.ai>\r\nTo: <kimi-one@example.com>\r\nDate: {date}\r\n\
             Subject: Kimi verification\r\n\r\nYour verification code is 615204.\r\n"
        )
    };
    let (tmp, maildir) = maildir_with(&[("1.eml", &kimi("Mon, 19 Oct 2026 08:00:00 +0000"))]);
    let old = MailboxConfig {
        dir: Some(maildir),
        seen_file: Some(tmp.path().join("seen")),
        senders: vec!["kimi".to_owned()],
        since: Some(SINCE),
        ..MailboxConfig::default()
    };
    assert!(
        poll_mailbox_config_for_credential(
            &old,
            Some("kimi-one@example.com"),
            Duration::from_millis(300)
        )
        .is_err(),
        "a code Kimi mailed before this sign-in began is never taken"
    );
    let (tmp, maildir) = maildir_with(&[("1.eml", &kimi(AFTER)), ("0.eml", &bank)]);
    let fresh = MailboxConfig {
        dir: Some(maildir),
        seen_file: Some(tmp.path().join("seen")),
        ..old
    };
    assert_eq!(
        poll_mailbox_config_for_credential(
            &fresh,
            Some("kimi-one@example.com"),
            Duration::from_millis(300)
        ),
        Ok(MailboxCredential::Otp("615204".to_owned())),
        "Kimi's own mail, sent after the start, is taken"
    );
    // A mailbox API's JSON item is judged by its own `From` and `Date`.
    let item = |from: &str| {
        format!(
            "{{\"From\":{{\"Address\":\"{from}\"}},\"Date\":\"2026-10-23T23:20:00Z\",\"Text\":\"code 615204\"}}"
        )
    };
    let filter = MailFilter {
        senders: &fresh.senders,
        since: Some(SINCE),
    };
    assert!(filter.admits(&item("noreply@kimi.ai")) && !filter.admits(&item("x@bank.example")));
}

/// Labels are per pool file, `<label>` secrets one name space: a label two pools give to
/// different emails hands its secret to Claude Code's own row alone.
#[test]
fn a_label_two_pools_give_different_mailboxes_hands_its_password_to_no_other_pool() {
    let home = aterm_tempfile::tempdir().expect("tmp");
    for (path, text) in [
        (
            ".secrets/ai-mailbox.toml",
            "[passwords]\nm16-1 = \"claude-mailbox-fixture-pw\"\nkimi-1 = \"kimi-fixture-pw\"\n",
        ),
        (
            ".config/aterm/accounts.toml",
            "[[account]]\nlabel = \"m16-1\"\nclass = \"dedicated\"\n\
             email = \"claude-box@example.com\"\n",
        ),
        (
            ".config/aterm/accounts-kimi.toml",
            "[[account]]\nlabel = \"m16-1\"\nclass = \"dedicated\"\n\
             email = \"personal@example.org\"\nregion = \"global\"\n\n[[account]]\n\
             label = \"kimi-1\"\nclass = \"dedicated\"\nemail = \"kimi-box@example.net\"\n\
             region = \"global\"\ndir = \"/nonexistent/aterm-fixture/logins/kimi/kimi-1\"\n",
        ),
    ] {
        let at = home.path().join(path);
        std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
        std::fs::write(at, text).expect("write");
    }
    let kimi = home.path().join(".config/aterm/accounts-kimi.toml");
    assert_eq!(
        super::super::accounts::Roster::load_for(super::super::accounts::Cli::Kimi, &kimi)
            .map(|r| r.rows.len()),
        Ok(2)
    );
    let mut cfg = discover_mailbox_config_files_raw(Some(home.path()), None, None);
    assert_ne!(
        cfg.password_for(Some("personal@example.org")).as_deref(),
        Some("claude-mailbox-fixture-pw"),
        "the Kimi row's mailbox never carries Claude Code's row's password"
    );
    cfg.alias_label_to_email(Some("m16-1"), Some("personal@example.org"));
    assert_ne!(
        cfg.password_for(Some("personal@example.org")).as_deref(),
        Some("claude-mailbox-fixture-pw"),
        "nor does the vendor approver's own alias of its row"
    );
    // Negative: Claude Code's row keeps its label's password, and an uncontested label of
    // another pool still names its own.
    assert_eq!(
        cfg.password_for(Some("claude-box@example.com")).as_deref(),
        Some("claude-mailbox-fixture-pw")
    );
    assert_eq!(
        cfg.password_for(Some("kimi-box@example.net")).as_deref(),
        Some("kimi-fixture-pw")
    );
}

#[test]
fn contested_mailbox_labels_cannot_be_recovered_from_an_email_spelling() {
    let home = aterm_tempfile::tempdir().expect("tmp");
    for (path, text) in [
        (
            ".secrets/ai-mailbox.toml",
            r#"
[passwords]
m27_2 = "claude-password-fixture"
[mailbox_passwords]
m27_2 = "claude-mailbox-password-fixture"
[totp_secrets]
m27_2 = "claude-totp-fixture"
"#,
        ),
        (
            ".config/aterm/accounts.toml",
            r#"
[[account]]
label = "m27-2"
class = "dedicated"
email = "claude-box@example.com"
"#,
        ),
        (
            ".config/aterm/accounts-kimi.toml",
            r#"
[[account]]
label = "m27-2"
class = "dedicated"
email = "sun.m27.2@example.org"
region = "global"
"#,
        ),
    ] {
        let at = home.path().join(path);
        std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
        std::fs::write(at, text).expect("write");
    }
    let cfg = discover_mailbox_config_files_raw(Some(home.path()), None, None);
    assert!(cfg.contested_labels.contains("m27-2"));
    for (blocked, own) in [
        (
            cfg.password_for(Some("sun.m27.2@example.org")),
            cfg.password_for(Some("claude-box@example.com")),
        ),
        (
            cfg.mailbox_password_for(Some("sun.m27.2@example.org")),
            cfg.mailbox_password_for(Some("claude-box@example.com")),
        ),
        (
            cfg.totp_secret_for(Some("sun.m27.2@example.org")),
            cfg.totp_secret_for(Some("claude-box@example.com")),
        ),
    ] {
        assert!(
            own.is_some(),
            "the owning roster keeps its explicit email alias"
        );
        assert!(
            blocked.is_none(),
            "email-derived labels cannot bypass a contest"
        );
    }
}

pub(crate) fn mailbox_selection_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "AnotherCase" {
        mailbox_selection_environment();
        next.insert("case", state["case"] + 1);
        return next;
    }
    assert_eq!(action, "Lookup");
    let (label, contested, exact, default, wildcard) = match state["case"] {
        0 => (true, true, false, false, false),
        1 => (true, true, true, false, false),
        2 => (true, false, false, false, false),
        3 => (true, true, false, true, false),
        4 => (true, true, false, false, true),
        5 => (false, false, false, false, false),
        6 => (true, true, true, true, true),
        7 => (true, false, false, true, true),
        8 => (false, false, false, true, true),
        _ => panic!("unknown credential configuration"),
    };
    let mut selected = None;
    for key in ["m27-2", "m27_2", "m27.2"] {
        for email in [
            "m27-2@example.org",
            "m27.2@example.org",
            "sun.m27.2@example.org",
        ] {
            let mut values = BTreeMap::new();
            if label {
                values.insert(key.to_owned(), "2".to_owned());
            }
            if exact {
                values.insert(email.to_owned(), "1".to_owned());
            }
            if wildcard {
                values.insert("*".to_owned(), "4".to_owned());
            }
            let cfg = MailboxConfig {
                passwords: values.clone(),
                mailbox_passwords: values.clone(),
                totp_secrets: values,
                password: default.then(|| "3".to_owned()),
                mailbox_password: default.then(|| "3".to_owned()),
                totp_secret: default.then(|| "3".to_owned()),
                contested_labels: if contested {
                    BTreeSet::from(["m27-2".to_owned()])
                } else {
                    BTreeSet::new()
                },
                ..MailboxConfig::default()
            };
            let before = cfg.clone();
            for result in [
                cfg.password_for(Some(email)),
                cfg.mailbox_password_for(Some(email)),
                cfg.totp_secret_for(Some(email)),
            ] {
                let value = result.map_or(0, |v| v.parse::<i64>().unwrap());
                if let Some(prior) = selected {
                    assert_eq!(prior, value, "{key} {email}");
                }
                selected = Some(value);
            }
            assert_eq!(cfg, before, "lookup never changes a credential binding");
        }
    }
    next.insert("phase", 1);
    next.insert("selected", selected.unwrap());
    next
}

#[test]
fn mailbox_credential_selection_conforms_with_negative_controls() {
    let model = aterm_models_harness::account_mailbox_selection_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 18);
    let mut witnessed = BTreeMap::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = mailbox_selection_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
                if action.name == "Lookup" {
                    witnessed.insert(state["case"], (state.clone(), next));
                }
            }
        }
    }
    for (case, wrong, law) in [
        (0, 2, "ContestedRefuses"),
        (1, 2, "ExactWins"),
        (2, 0, "UncontestedWorks"),
        (3, 0, "DefaultWorks"),
        (4, 2, "WildcardWorks"),
        (5, 2, "MissingRefuses"),
    ] {
        let (before, after) = &witnessed[&case];
        let mut mutant = after.clone();
        mutant.insert("selected", wrong);
        assert!(!model.check_invariant(law, &mutant));
        assert!(!model.successors("Lookup", before).contains(&mutant));
    }
}

#[test]
fn mailbox_conflicts_include_csv_and_configured_fleets_before_totp_aliasing() {
    for source in ["config-fleet", "sibling-fleet", "claude-csv", "vendor-csv"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let label = if source == "vendor-csv" {
            "kimi-m27-2"
        } else {
            "m27-2"
        };
        let own = format!(
            "[[account]]\nlabel='{label}'\nclass='dedicated'\nemail='claude-box@example.com'\n"
        );
        let fleet = format!("{own}machines=['m27']\n");
        let other = format!(
            "[[account]]\nlabel='{label}'\nclass='dedicated'\nemail='vendor-box@example.org'\nregion='global'\n"
        );
        let (own_path, own_text, other_path, other_text) = match source {
            "config-fleet" => (
                ".config/aterm/fleet-accounts.toml",
                fleet,
                ".config/aterm/accounts-kimi.toml",
                other,
            ),
            "sibling-fleet" => (
                "custom/aterm/fleet-accounts.toml",
                fleet,
                "custom/aterm/accounts-kimi.toml",
                other,
            ),
            "claude-csv" => (
                ".secrets/ai-accounts.csv",
                "email,computer,claude\nclaude-box@example.com,m27.2,1\n".to_owned(),
                ".config/aterm/accounts-kimi.toml",
                other,
            ),
            "vendor-csv" => (
                ".config/aterm/accounts.toml",
                own,
                ".secrets/ai-accounts.csv",
                "email,computer,kimi\nvendor-box@example.org,m27.2,global\n".to_owned(),
            ),
            _ => unreachable!(),
        };
        let secret = format!(
            "[passwords]\n'{label}'='password-fixture'\n\
             [mailbox_passwords]\n'{label}'='mailbox-fixture'\n\
             [totp_secrets]\n'{label}'='otpauth://totp/Test:uri-recipient%40example.org?secret=JBSWY3DPEHPK3PXP&issuer=Test'\n"
        );
        for (path, text) in [
            (own_path, own_text.as_str()),
            (other_path, other_text.as_str()),
            (".secrets/ai-mailbox.toml", secret.as_str()),
        ] {
            let path = home.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let custom_config = home.path().join("custom/aterm/aterm.toml");
        let cfg = discover_mailbox_config_files_raw(
            Some(home.path()),
            None,
            (source == "sibling-fleet").then_some(custom_config.as_path()),
        );
        let override_path = home.path().join("explicit-mailbox.toml");
        std::fs::write(
            &override_path,
            secret.replace("password-fixture", "override-fixture"),
        )
        .unwrap();
        let overridden =
            discover_mailbox_config_with_env(Some(home.path()), None, |name| match name {
                MAILBOX_ENV => Some(override_path.as_os_str().to_owned()),
                "XDG_CONFIG_HOME" if source == "sibling-fleet" => {
                    Some(home.path().join("custom").into_os_string())
                }
                _ => None,
            })
            .unwrap();
        for (cfg, expected) in [(cfg, "password-fixture"), (overridden, "override-fixture")] {
            assert!(cfg.contested_labels.contains(label), "{source}");
            for email in ["vendor-box@example.org", "uri-recipient@example.org"] {
                assert!(cfg.password_for(Some(email)).is_none(), "{source} {email}");
                assert!(
                    cfg.mailbox_password_for(Some(email)).is_none(),
                    "{source} {email}"
                );
            }
            assert!(
                cfg.totp_secret_for(Some("uri-recipient@example.org"))
                    .is_some()
            );
            assert_eq!(
                cfg.password_for(Some("claude-box@example.com")).as_deref(),
                Some(expected)
            );
        }
    }
}

#[test]
fn mailbox_aliases_respect_the_winning_source_before_deriving_email_keys() {
    for source in ["override", "store", "raw-override", "raw-store"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let old = r#"
[passwords]
m27 = "old-password-fixture"
[mailbox_passwords]
m27 = "old-second-fixture"
[totp_secrets]
m27 = "otpauth://totp/Test:mailbox%40example.org?secret=JBSWY3DPEHPK3PXP&issuer=Test"
"#;
        let new = r#"
[passwords]
m27 = "new-password-fixture"
[mailbox_passwords]
m27 = "new-second-fixture"
[totp_secrets]
m27 = "KRSXG5A"
"#;
        let raw = "old-password-fixture\nold-second-fixture\notpauth://totp/Test:mailbox%40example.org?secret=JBSWY3DPEHPK3PXP&issuer=Test\n";
        let (old_path, old) = if source.starts_with("raw-") {
            (".secrets/m27", raw)
        } else {
            (".secrets/ai-mailbox.toml", old)
        };
        for (name, text) in [
            (old_path, old),
            (
                ".config/aterm/accounts.toml",
                "[[account]]\nlabel='m27'\nclass='dedicated'\nemail='mailbox@example.org'\n",
            ),
            ("store/mailbox.toml", new),
        ] {
            let path = home.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let store = home.path().join("store");
        let cfg = if source.ends_with("store") {
            discover_mailbox_config_files_raw(Some(home.path()), Some(&store), None)
        } else {
            discover_mailbox_config_with_env(Some(home.path()), None, |name| {
                (name == MAILBOX_ENV).then(|| store.join("mailbox.toml").into_os_string())
            })
            .unwrap()
        };
        assert_eq!(
            cfg.password_for(Some("m27")).as_deref(),
            Some("new-password-fixture")
        );
        assert_eq!(
            cfg.password_for(Some("mailbox@example.org")).as_deref(),
            Some("new-password-fixture"),
            "{source}: a derived email alias must use the winning label credential"
        );
        assert_eq!(
            cfg.mailbox_password_for(Some("mailbox@example.org"))
                .as_deref(),
            Some("new-second-fixture"),
            "{source}: second-stage credentials obey the same source precedence"
        );
        assert_eq!(
            cfg.totp_secret_for(Some("mailbox@example.org")).as_deref(),
            Some("KRSXG5A"),
            "{source}: a superseded TOTP URI must not keep its old email value"
        );
    }
}

/// A vendor sign-in stays within the flow it started: a mail from another sender is not
/// followed, and a page naming another client, or handing its login elsewhere, is never pressed.
#[test]
fn a_vendor_sign_in_presses_nothing_outside_the_flow_it_started() {
    let start = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_CODEX\
                 &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=s0&scope=openid";
    let bound = FlowBound::of(start).expect("unambiguous starting flow");
    assert_eq!(bound.client_id.as_deref(), Some("app_CODEX"));
    assert_eq!(bound.callback_port, Some(1455));
    let lure = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_OTHER\
                &redirect_uri=https%3A%2F%2Fcollector.example.net%2Fcb&state=s1&scope=openid";
    let (tmp, maildir) = maildir_with(&[(
        "1.eml",
        &format!(
            "From: Someone <promo@lure.example>\r\nTo: <codex-one@example.com>\r\nDate: {AFTER}\r\n\
             Subject: Verify your login\r\n\r\nVerify your login: {lure}\r\n"
        ),
    )]);
    let cfg = MailboxConfig {
        dir: Some(maildir),
        seen_file: Some(tmp.path().join("seen")),
        senders: vec!["openai".to_owned(), "chatgpt".to_owned()],
        since: Some(SINCE),
        ..MailboxConfig::default()
    };
    assert!(
        poll_mailbox_config_for_credential(
            &cfg,
            Some("codex-one@example.com"),
            Duration::from_millis(300)
        )
        .is_err(),
        "lure.example is not OpenAI"
    );
    let gate = Gate {
        hosts: &["auth.openai.com"],
        completion: Completion::Loopback,
        code: None,
        email: Some("codex-one@example.com"),
        senders: &["openai", "chatgpt"],
        filled_email: true,
        bound: Some(&bound),
        ..device_gate()
    };
    let consent = |url: &str| Page {
        url: url.to_owned(),
        heading: "authorize app".to_owned(),
        text: "An app wants to access your OpenAI account".to_owned(),
        buttons: vec!["cancel".to_owned(), "allow".to_owned()],
        inputs: vec![],
    };
    assert!(matches!(
        vendor_step(&consent(lure), &gate),
        Step::Refuse(_)
    ));
    let elsewhere = "https://auth.openai.com/oauth/authorize?client_id=app_CODEX\
                     &redirect_uri=https%3A%2F%2Fcollector.example.net%2Fcb";
    assert!(matches!(
        vendor_step(&consent(elsewhere), &gate),
        Step::Refuse(_)
    ));
    assert!(
        bound.outside(lure, gate.hosts).is_some(),
        "a mailed link of another client is not followed"
    );
    let other_port = Page {
        url: "http://localhost:8080/auth/callback?code=x".to_owned(),
        ..consent(start)
    };
    assert!(matches!(vendor_step(&other_port, &gate), Step::Refuse(_)));

    // Negative: the started flow's own pages are pressed, and its callback ends the drive.
    assert_eq!(vendor_step(&consent(start), &gate), Step::Press(1));
    assert_eq!(
        vendor_step(
            &consent("https://auth.openai.com/sign-in-with-chatgpt/codex/consent"),
            &gate
        ),
        Step::Press(1)
    );
    let back = Page {
        url: "http://localhost:1455/success?x=1".to_owned(),
        ..consent(start)
    };
    assert_eq!(vendor_step(&back, &gate), Step::Done);
    assert_eq!(bound.outside(start, gate.hosts), None);
}

/// A page that asks nothing aterm does (Kimi's sign-in through another provider, a phone
/// number) stops the drive for a person once it has stood unchanged for [`VENDOR_STALL`],
/// naming the page without its query, code or email; a page still changing, a step aterm takes,
/// or a confirmed device page waiting on its CLI never does.
#[test]
fn an_unchanged_page_that_asks_nothing_of_aterm_stops_for_a_person_naming_it() {
    let gate = device_gate();
    let login = page(
        &format!("https://www.kimi.ai/code/authorize_device?user_code={CODE}"),
        "log in to kimi-one@example.com",
        "Continue with Google\nPhone number",
        &["continue with google"],
        &[Field::Other],
    );
    assert_eq!(vendor_step(&login, &gate), Step::Wait);
    let t0 = Instant::now();
    let mut stall = PageStall::default();
    assert_eq!(stall.note(&login, &Step::Wait, &gate, t0), None);
    assert_eq!(
        stall.note(
            &login,
            &Step::Wait,
            &gate,
            t0 + VENDOR_STALL - Duration::from_secs(1)
        ),
        None
    );
    let why = stall
        .note(&login, &Step::Wait, &gate, t0 + VENDOR_STALL)
        .expect("a stalled page stops for a person");
    assert!(why.contains("`www.kimi.ai/code/authorize_device`"), "{why}");
    assert!(
        !why.contains(CODE) && !why.contains("user_code") && !why.contains('@'),
        "{why}"
    );
    assert!(
        !why.contains("accounts browser"),
        "no command to name: {why}"
    );
    let named = Gate {
        sign_in_once: Some(SIGN_IN_ONCE),
        ..gate
    };
    let mut stall = PageStall::default();
    let _ = stall.note(&login, &Step::Wait, &named, t0);
    let why = stall
        .note(&login, &Step::Wait, &named, t0 + VENDOR_STALL)
        .expect("a stalled page stops for a person");
    assert!(
        why.contains(&format!("sign it in once with `{SIGN_IN_ONCE}`")),
        "{why}"
    );

    // Negative: a changed page restarts the clock, a step aterm takes clears it, and a device
    // page already confirmed waits on the CLI.
    let mut stall = PageStall::default();
    let _ = stall.note(&login, &Step::Wait, &gate, t0);
    let moved = Page {
        heading: "verify your phone".to_owned(),
        ..login.clone()
    };
    assert_eq!(
        stall.note(&moved, &Step::Wait, &gate, t0 + VENDOR_STALL),
        None
    );
    assert_eq!(
        stall.note(&moved, &Step::Press(0), &gate, t0 + 2 * VENDOR_STALL),
        None
    );
    assert_eq!(
        stall.note(&moved, &Step::Wait, &gate, t0 + 2 * VENDOR_STALL),
        None
    );
    let confirmed = Gate {
        confirmed: true,
        ..device_gate()
    };
    let mut stall = PageStall::default();
    let _ = stall.note(&login, &Step::Wait, &confirmed, t0);
    assert_eq!(
        stall.note(&login, &Step::Wait, &confirmed, t0 + 10 * VENDOR_STALL),
        None
    );
}

/// The word after `--` that makes [`browser_stand_in`] a stand-in; a test run without it is an
/// empty test.
pub(crate) const STAND_IN: &str = "aterm-browser-stand-in";

/// What a browser stand-in leads its arguments with: this test binary's own `--exact` run of
/// [`browser_stand_in`], [`STAND_IN`], then `mode`'s words (a window's own arguments may
/// follow, unread). Unix's, as the window tests that lead with it are.
#[cfg(unix)]
pub(crate) fn stand_in_lead(mode: &[&str]) -> Vec<OsString> {
    let mut lead: Vec<OsString> = [
        "harness::headless_oauth::vendor_tests::browser_stand_in",
        "--exact",
        "--nocapture",
        "--",
        STAND_IN,
    ]
    .map(OsString::from)
    .to_vec();
    lead.extend(mode.iter().map(OsString::from));
    lead
}

#[cfg(unix)]
static HUNG_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn note_hangup(_: libc::c_int) {
    HUNG_UP.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// A browser as this test binary run in place, its mode after [`STAND_IN`]: `exit <code> <ms>`
/// exits `code` after `ms` milliseconds; `hang` waits out its one-minute hang detector;
/// `hup <dir>` writes `<dir>/ready` once it handles a hangup, then writes `<dir>/hup` and exits 0
/// on one; `hup <dir> quit <code> <ms>` exits `code` after `ms` if no hangup came before;
/// `ignore-hup <dir>` ignores
/// hangups, writes `<dir>/ready` and hangs. Anywhere else it is an empty test.
#[test]
fn browser_stand_in() {
    let args: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    let Some(mode) = args.strip_prefix(&[STAND_IN.to_owned()]) else {
        return;
    };
    let words: Vec<&str> = mode.iter().map(String::as_str).collect();
    let millis = |w: &str| Duration::from_millis(w.parse().unwrap_or(0));
    match words.as_slice() {
        ["exit", code, ms, ..] => {
            std::thread::sleep(millis(ms));
            std::process::exit(code.parse().unwrap_or(1));
        }
        #[cfg(unix)]
        ["hup", dir, rest @ ..] => {
            // SAFETY: `sigaction` is plain data for which all-zero is valid; the handler only
            // stores to an atomic.
            unsafe {
                let mut on: libc::sigaction = std::mem::zeroed();
                on.sa_sigaction = note_hangup as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::sigaction(libc::SIGHUP, &on, std::ptr::null_mut());
            }
            let dir = PathBuf::from(dir);
            std::fs::write(dir.join("ready"), "").expect("ready");
            let quit = match rest {
                ["quit", code, ms, ..] => {
                    Some((code.parse().unwrap_or(1), Instant::now() + millis(ms)))
                }
                _ => None,
            };
            let hang = Instant::now() + Duration::from_secs(60);
            while Instant::now() < hang {
                if HUNG_UP.load(std::sync::atomic::Ordering::SeqCst) {
                    std::fs::write(dir.join("hup"), "").expect("marker");
                    std::process::exit(0);
                }
                if let Some((code, at)) = quit
                    && Instant::now() >= at
                {
                    std::process::exit(code);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            std::process::exit(1);
        }
        #[cfg(unix)]
        ["ignore-hup", dir, ..] => {
            // SAFETY: ignoring a signal is a valid disposition.
            unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
            std::fs::write(Path::new(dir).join("ready"), "").expect("ready");
            std::thread::sleep(Duration::from_secs(60));
            std::process::exit(1);
        }
        _ => {
            std::thread::sleep(Duration::from_secs(60));
            std::process::exit(1);
        }
    }
}

/// [`browser_stand_in`] in `mode`, started as a headless browser is: its own child, nothing on
/// its stdio.
#[cfg(unix)]
pub(crate) fn spawn_stand_in(mode: &[&str]) -> aterm_uds::spawnfd::LaneChild {
    use aterm_uds::spawnfd::{ChildStdio, LaneCommand};
    let exe = std::env::current_exe().expect("test exe");
    let mut cmd = LaneCommand::new(&exe);
    cmd.args(stand_in_lead(mode))
        .stdin(ChildStdio::Null)
        .stdout(ChildStdio::Null)
        .stderr(ChildStdio::Null);
    aterm_uds::spawnfd::spawn(cmd).expect("the stand-in starts")
}

/// A headless browser asked to close is given its grace to exit by itself, so the profile it
/// wrote is whole: its status is its own exit, not a kill. CONTROL: one that never exits is
/// killed once a short grace runs out.
#[cfg(unix)]
#[test]
fn a_headless_browser_gets_its_grace_before_any_kill() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut quits = BrowserChild::with_grace(
        spawn_stand_in(&["exit", "0", "100"]),
        Duration::from_secs(60),
    );
    let status = quits.close().expect("waited");
    assert_eq!(
        (status.code(), status.signal()),
        (Some(0), None),
        "its own exit"
    );

    let mut stuck = BrowserChild::with_grace(spawn_stand_in(&["hang"]), Duration::from_millis(300));
    let status = stuck.close().expect("waited");
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "killed after its grace"
    );
}

/// Kimi's device card as the lab read it on www.kimi.ai (2026-10-24): it names the account
/// signed in and its contact, offers `Log in`, `Cancel` and `Switch account`, and shows its code
/// only in its URL.
fn kimi_card(url: &str, contact: &str) -> Page {
    page(
        url,
        "",
        &format!("Allow this account to log in?\nKimi One\n{contact}\nCurrent"),
        &["log in", "cancel", "switch account"],
        &[],
    )
}

const KIMI_CARD_URL: &str = "https://www.kimi.ai/code/authorize_device?user_code=QXR4-MZK7";

/// KIMI'S CARD, signed in, its URL naming the printed code, is confirmed by its one `Log in`
/// press, then the CLI is waited for. CONTROLS: another `user_code`; the logged-out modal at the
/// same URL; an enabled `log in` with no signed-in words; the card off the row's region; and a
/// code-shaped nickname are never pressed.
#[test]
fn kimis_signed_in_card_naming_the_printed_code_in_its_url_is_confirmed_once() {
    let gate = device_gate();
    let card = kimi_card(KIMI_CARD_URL, "kimi-one@example.com");
    assert_eq!(
        vendor_step(&card, &gate),
        Step::Press(0),
        "log in, never cancel"
    );
    let confirmed = Gate {
        confirmed: true,
        ..gate
    };
    assert_eq!(vendor_step(&card, &confirmed), Step::Wait);
    assert_eq!(device_code(&card, CODE), (false, true));

    let other = kimi_card(
        "https://www.kimi.ai/code/authorize_device?user_code=ZZT8-KQW2",
        "kimi-one@example.com",
    );
    assert!(
        matches!(vendor_step(&other, &gate), Step::Refuse(_)),
        "{:?}",
        vendor_step(&other, &gate)
    );
    let logged_out = page(
        KIMI_CARD_URL,
        "",
        "Log in to Kimi\nContinue with Google\nSign in with SSO\nFeedback",
        &["continue with google", "sign in with sso", "feedback"],
        &[Field::Other, Field::Other, Field::Other],
    );
    assert_eq!(vendor_step(&logged_out, &gate), Step::Wait);
    let bare = page(KIMI_CARD_URL, "", "Kimi Code", &["log in"], &[]);
    assert_eq!(
        vendor_step(&bare, &gate),
        Step::Wait,
        "`log in` confirms only a card that names the account signed in"
    );
    let form = page(KIMI_CARD_URL, "", &format!("Code {CODE}"), &["log in"], &[]);
    assert_eq!(
        vendor_step(&form, &gate),
        Step::Wait,
        "a page showing the code with only `log in` is a sign-in form, never a confirm"
    );
    let unnamed = page(KIMI_CARD_URL, "", "Kimi Code", &["confirm"], &[]);
    assert_eq!(
        vendor_step(&unnamed, &gate),
        Step::Wait,
        "the URL's code counts only on a page that names the account signed in"
    );
    let mainland = kimi_card(
        "https://www.kimi.com/code/authorize_device?user_code=QXR4-MZK7",
        "kimi-one@example.com",
    );
    assert!(matches!(vendor_step(&mainland, &gate), Step::Stop(_)));
    let nickname = page(
        KIMI_CARD_URL,
        "",
        "Allow this account to log in?\nZZT8-KQW2\nkimi-one@example.com\nCurrent",
        &["log in", "cancel", "switch account"],
        &[],
    );
    assert!(matches!(vendor_step(&nickname, &gate), Step::Refuse(_)));
}

/// KIMI'S MAINLAND CARD (kimi.com, as its bundle spells it) reads the same in Chinese.
/// CONTROL: `取消` (cancel) alone is never pressed.
#[test]
fn kimis_mainland_card_reads_the_same_in_chinese() {
    let gate = Gate {
        hosts: &["auth.kimi.com", "www.kimi.com"],
        ..device_gate()
    };
    let url = "https://www.kimi.com/code/authorize_device?user_code=QXR4-MZK7";
    let card = page(
        url,
        "",
        "是否允许登录此账号？\nKimi One\nkimi-one@example.com\n切换账号",
        &["登录", "取消", "切换账号"],
        &[],
    );
    assert_eq!(vendor_step(&card, &gate), Step::Press(0));
    let cancel = page(
        url,
        "",
        "是否允许登录此账号？\nKimi One\nkimi-one@example.com\n切换账号",
        &["取消", "切换账号"],
        &[],
    );
    assert_eq!(vendor_step(&cancel, &gate), Step::Wait);
}

/// The card proves the row by its exact email; another email is another account, refused; a
/// phone contact is pressed but proves nothing.
#[test]
fn kimis_card_proves_the_row_by_its_email_and_a_phone_contact_proves_nothing() {
    let gate = device_gate();
    let own = kimi_card(KIMI_CARD_URL, "kimi-one@example.com");
    assert!(shows_email(&own, &gate));
    let other = kimi_card(KIMI_CARD_URL, "other@example.com");
    assert!(matches!(vendor_step(&other, &gate), Step::Refuse(_)));
    assert!(!shows_email(&other, &gate));
    let phone = kimi_card(KIMI_CARD_URL, "+1 555 0100");
    assert_eq!(vendor_step(&phone, &gate), Step::Press(0));
    assert!(!shows_email(&phone, &gate), "a phone number names no email");
}

/// Nothing labelled agree is ever pressed, nor anything on a page that asks to agree: Kimi's
/// terms dialog over its card, and an `I agree` or `同意` after the code was typed, wait for the
/// person. CONTROL: `Allow` there is pressed.
#[test]
fn nothing_labelled_agree_is_ever_pressed() {
    let gate = device_gate();
    let terms = page(
        KIMI_CARD_URL,
        "",
        "Agree to Kimi's terms\nKimi One\nkimi-one@example.com\nSwitch account",
        &["agree to kimi's terms", "log in", "cancel"],
        &[],
    );
    assert_eq!(
        vendor_step(&terms, &gate),
        Step::Wait,
        "not even `log in` while the page asks to agree"
    );
    assert!(!pressable("agree to kimi's terms") && !pressable("同意") && !pressable("取消"));
    let typed = Gate {
        filled_code: true,
        ..gate
    };
    for label in ["i agree", "同意", "continue (i agree to kimi's terms)"] {
        let consent = page(
            "https://auth.kimi.ai/device",
            "authorize",
            "Kimi Code wants to access your account",
            &[label],
            &[],
        );
        assert_eq!(vendor_step(&consent, &typed), Step::Wait, "{label}");
    }
    let allow = page(
        "https://auth.kimi.ai/device",
        "authorize",
        "Kimi Code wants to access your account",
        &["allow"],
        &[],
    );
    assert_eq!(vendor_step(&allow, &typed), Step::Press(0));
}

/// A bot-protection block page on a pinned host stops the drive at once, its text (this
/// machine's address, the ray id) kept out of the reason. CONTROL: the same host without the
/// block words waits.
#[test]
fn a_bot_protection_block_page_stops_at_once_and_keeps_its_text_out_of_the_reason() {
    let gate = Gate {
        hosts: &["auth.x.ai", "accounts.x.ai"],
        ..device_gate()
    };
    let blocked = page(
        "https://accounts.x.ai/sign-in",
        "sorry, you have been blocked",
        "Sorry, you have been blocked\nYou are unable to access x.ai\nCloudflare Ray ID: \
         8f00aa11bb22cc33 • Your IP: 192.0.2.1 • Performance & security by Cloudflare",
        &[],
        &[],
    );
    match vendor_step(&blocked, &gate) {
        Step::Stop(why) => {
            assert!(why.contains("bot-protection block page"), "{why}");
            assert!(
                !why.contains("192.0.2.1") && !why.to_ascii_lowercase().contains("ray id"),
                "{why}"
            );
        }
        step => panic!("{step:?}"),
    }
    let plain = page(
        "https://accounts.x.ai/sign-in",
        "sign in",
        "Sign in to xAI",
        &[],
        &[],
    );
    assert_eq!(vendor_step(&plain, &gate), Step::Wait);
}

/// A stand-in browser over the CDP pipes that serves `page` to every read, or, with
/// `refuse_target`, answers `Target.createTarget` with an error.
fn fake_cdp(page: Value, refuse_target: bool) -> (CdpClient, std::thread::JoinHandle<()>) {
    let (to_browser_r, to_browser_w) = std::io::pipe().expect("pipe");
    let (from_browser_r, mut from_browser_w) = std::io::pipe().expect("pipe");
    let browser = std::thread::spawn(move || {
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                let reply = match method {
                    "Target.createTarget" if refuse_target => {
                        json!({"id": id, "error": {"code": -32000, "message": "no target"}})
                    }
                    "Target.createTarget" => json!({"id": id, "result": {"targetId": "t1"}}),
                    "Target.attachToTarget" => json!({"id": id, "result": {"sessionId": "s1"}}),
                    "Runtime.evaluate" => json!({
                        "id": id,
                        "result": {"result": {"type": "object", "value": page.clone()}}
                    }),
                    _ => json!({"id": id, "result": {}}),
                };
                let mut bytes = aterm_json::to_vec(&reply).expect("reply");
                bytes.push(0);
                if from_browser_w.write_all(&bytes).is_err() || method == "Browser.close" {
                    return;
                }
            }
        }
    });
    (CdpClient::new(to_browser_w, from_browser_r), browser)
}

/// A page that wants a person's own sign-in (another identity provider) ends the drive
/// `NeedsSignIn`, naming the command that signs the profile in once. CONTROL: a browser that
/// never opened the page is not a sign-in to ask for.
#[test]
fn a_page_wanting_a_persons_sign_in_ends_needs_sign_in_and_a_browser_that_never_opened_does_not() {
    use super::super::login_drive::Approval;
    let google = json!({
        "url": "https://accounts.google.com/o/oauth2/auth",
        "heading": "sign in",
        "text": "Choose an account",
        "buttons": ["continue"],
        "inputs": []
    });
    let gate = Gate {
        sign_in_once: Some(SIGN_IN_ONCE),
        ..device_gate()
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let no_mail = |_: &mut CdpClient, _: Option<&str>| Err("no mailbox in a test");
    let (mut cdp, browser) = fake_cdp(google.clone(), false);
    let got = drive_cdp_vendor(&mut cdp, KIMI_CARD_URL, gate, &|| false, deadline, no_mail);
    drop(cdp);
    browser.join().expect("stand-in browser");
    match got {
        Approval::NeedsSignIn(why) => assert!(why.contains(SIGN_IN_ONCE), "{why}"),
        other => panic!("{other:?}"),
    }

    let (mut cdp, browser) = fake_cdp(google, true);
    let got = drive_cdp_vendor(&mut cdp, KIMI_CARD_URL, gate, &|| false, deadline, no_mail);
    drop(cdp);
    browser.join().expect("stand-in browser");
    assert!(matches!(got, Approval::NeedsYou(_)), "{got:?}");
}

/// `accounts browser` opens the very profile the headless approver signs in through: a vendor
/// row's is its email's under the shared store, Claude Code's its own store's. CONTROL: two
/// emails, or a plus-addressed one, are two profiles.
#[test]
fn the_window_and_the_headless_approver_open_one_profile_per_row() {
    use super::super::accounts::Cli;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let shared = tmp.path().join("state/accounts");
    let store = tmp.path().join("state/accounts-kimi");
    let home = tmp.path().join("home");
    let email = "kimi-one@example.com";
    let window = row_browser_profile(Cli::Kimi, &store, &shared, "kimi-one", Some(email));
    let approver = VendorApprover::new(Cli::Kimi, "kimi-one", Some(email), &shared, Some(&home));
    assert_eq!(window, approver.profile_dir(Some(email)));
    assert!(window.starts_with(shared.join(VENDOR_PROFILES_DIR)));
    assert_eq!(
        approver.sign_in_once,
        "aterm harness accounts browser kimi-one --cli kimi"
    );
    assert_eq!(
        row_browser_profile(
            Cli::Claude,
            &shared,
            &shared,
            "work-1",
            Some("w@example.com")
        ),
        resolve_browser_profile_dir(&shared, Some("work-1"), Some("w@example.com"))
    );
    assert_ne!(
        window,
        row_browser_profile(
            Cli::Kimi,
            &store,
            &shared,
            "kimi-one",
            Some("kimi-two@example.com")
        )
    );
    assert_ne!(
        window,
        row_browser_profile(
            Cli::Kimi,
            &store,
            &shared,
            "kimi-one",
            Some("kimi-one+2@example.com")
        )
    );
}

/// A VENDOR'S SIGN-IN NEVER MOVES CLAUDE CODE'S RENEWAL TO ANOTHER PROFILE: a vendor row's
/// profile is made under the vendors' own folder, so a Claude Code row keeps its label profile
/// (a plain email and a plus-addressed one on that base alike), and a row with none is still
/// read as having none (so its renewal still opens the person's browser). CONTROL: where Claude
/// Code's own profile of that exact email exists, the vendors share it; and once a vendor's own
/// profile exists, a later Claude Code profile of the email does not move the vendor off it.
#[test]
fn a_vendor_sign_in_makes_no_profile_claude_codes_renewal_would_pick() {
    use super::super::accounts::Cli;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let shared = tmp.path().join("state/accounts");
    let home = tmp.path().join("home");
    let make = |dir: &Path| std::fs::create_dir_all(dir).expect("profile");

    let m1 = browser_profile_dir(&shared, Some("m1"));
    make(&m1);
    assert_eq!(
        resolve_browser_profile_dir(&shared, Some("m1"), Some("a@example.com")),
        m1,
        "CONTROL: the row's label profile"
    );
    let kimi = VendorApprover::new(Cli::Kimi, "kimi-m1", Some("a@example.com"), &shared, None);
    make(&kimi.profile_dir(Some("a@example.com")));
    let codex = row_browser_profile(
        Cli::Codex,
        &tmp.path().join("state/accounts-codex"),
        &shared,
        "codex-m1",
        Some("a@example.com"),
    );
    make(&codex);
    assert_eq!(
        codex,
        kimi.profile_dir(Some("a@example.com")),
        "one per email"
    );
    assert_eq!(
        resolve_browser_profile_dir(&shared, Some("m1"), Some("a@example.com")),
        m1,
        "a vendor's sign-in of the email leaves Claude Code's row on its label profile"
    );

    let m2 = browser_profile_dir(&shared, Some("m2"));
    make(&m2);
    assert_eq!(
        resolve_browser_profile_dir(&shared, Some("m2"), Some("a+m2@example.com")),
        m2,
        "a plus-addressed Claude Code row keeps its label profile beside a vendor of its base"
    );

    assert!(!has_browser_profile(&shared, "m3", Some("b@example.com")));
    let grok = VendorApprover::new(
        Cli::Grok,
        "grok-m3",
        Some("b@example.com"),
        &shared,
        Some(&home),
    );
    make(&grok.profile_dir(Some("b@example.com")));
    assert!(
        !has_browser_profile(&shared, "m3", Some("b@example.com")),
        "a row with no profile is not given the vendor's"
    );
    make(&grok.profile_dir(None));
    assert!(
        !has_browser_profile(&shared, "grok-m3", None),
        "nor by its label key"
    );

    let claudes = browser_profile_dir(&shared, Some("c@example.com"));
    make(&claudes);
    let shares = VendorApprover::new(Cli::Kimi, "kimi-c", Some("c@example.com"), &shared, None);
    assert_eq!(
        shares.profile_dir(Some("c@example.com")),
        resolve_browser_profile_dir(&shared, Some("c"), Some("c@example.com")),
        "CONTROL: Claude Code's own profile of the exact email is shared"
    );
    assert_ne!(
        shares.profile_dir(Some("c+2@example.com")),
        claudes,
        "CONTROL: a plus-addressed vendor row never shares its base's"
    );
    let kept = kimi.profile_dir(Some("a@example.com"));
    make(&browser_profile_dir(&shared, Some("a@example.com")));
    assert_eq!(
        kimi.profile_dir(Some("a@example.com")),
        kept,
        "CONTROL: a vendor keeps its own profile once it has one"
    );
}

/// KIMI'S CARD with a banner over it: an `Accept` or `Accept terms` waits for the person, since
/// the vendor's terms are theirs to accept; a cookie banner's `Accept all cookies`, `Allow all`
/// and a stray `Continue` are never the card's confirm, which is its own `Log in` alone (the code
/// is only in its URL). After the confirm, a terms page's `Accept` or `Continue` is never pressed
/// as consent. CONTROL: a consent page's `Allow` after the confirm is pressed.
#[test]
fn an_accept_or_a_banners_button_on_kimis_card_is_never_its_confirm() {
    let gate = device_gate();
    let card = |first: &str| {
        page(
            KIMI_CARD_URL,
            "",
            "Allow this account to log in?\nKimi One\nkimi-one@example.com\nCurrent",
            &[first, "log in", "cancel", "switch account"],
            &[],
        )
    };
    for banner in [
        "accept all cookies",
        "allow all",
        "continue",
        "allow",
        "yes",
    ] {
        assert_eq!(
            vendor_step(&card(banner), &gate),
            Step::Press(1),
            "{banner}: the card's own `log in`"
        );
    }
    for terms in ["accept", "accept terms", "i accept", "接受"] {
        assert_eq!(vendor_step(&card(terms), &gate), Step::Wait, "{terms}");
    }
    assert!(!pressable("accept") && !pressable("accept all cookies") && !pressable("terms"));
    assert!(
        !pressable("allow all") && pressable("allow"),
        "a banner's `Allow all` alone"
    );
    let shown = device_page("https://auth.kimi.ai/device?user_code=QXR4-MZK7", CODE);
    let banner = Page {
        buttons: vec![
            "allow all".to_owned(),
            "cancel".to_owned(),
            "confirm".to_owned(),
        ],
        ..shown
    };
    assert_eq!(
        vendor_step(&banner, &gate),
        Step::Press(2),
        "the page's own confirm"
    );
    let confirmed = Gate {
        confirmed: true,
        ..gate
    };
    for buttons in [&["decline", "accept"][..], &["decline", "continue"][..]] {
        let terms = page(
            "https://www.kimi.ai/terms-update",
            "we have updated our terms of service",
            "We have updated our Terms of Service",
            buttons,
            &[],
        );
        assert_eq!(vendor_step(&terms, &confirmed), Step::Wait, "{buttons:?}");
    }
    let consent = page(
        "https://auth.kimi.ai/device",
        "authorize",
        "Kimi Code wants to access your account",
        &["allow"],
        &[],
    );
    assert_eq!(
        vendor_step(&consent, &confirmed),
        Step::Press(0),
        "CONTROL: consent is pressed"
    );
}

/// A KIMI row's email is never typed: its sign-in form signs a new account up as readily as it
/// signs one in, so an email field, a mailed code's page and a code to type all stop for the
/// person's own sign-in, naming `accounts browser`; and Kimi's approver says so. CONTROL: a
/// vendor whose email may be typed fills it, and Grok's and Codex's approvers may.
#[test]
fn a_kimi_rows_email_is_never_typed_and_its_mail_is_never_read() {
    use super::super::accounts::Cli;
    let kimi = Gate {
        types_email: false,
        sign_in_once: Some(SIGN_IN_ONCE),
        ..device_gate()
    };
    let form = page(
        KIMI_CARD_URL,
        "log in to kimi",
        "Log in to Kimi\nEmail\nSend code",
        &["send code"],
        &[Field::Email],
    );
    let mailed = page(
        "https://www.kimi.ai/login",
        "check your email",
        "Enter the verification code we sent",
        &["verify"],
        &[Field::Code],
    );
    let sent = Gate {
        filled_email: true,
        ..kimi
    };
    let with_code = Gate {
        otp: Some("123456"),
        ..sent
    };
    for (page, gate) in [(&form, kimi), (&mailed, sent), (&mailed, with_code)] {
        match vendor_step(page, &gate) {
            Step::SignIn(why) => {
                assert!(
                    why.contains("only from a person") && why.contains(SIGN_IN_ONCE),
                    "{why}"
                )
            }
            step => panic!("{step:?}"),
        }
    }
    assert_eq!(
        vendor_step(&form, &device_gate()),
        Step::FillEmail,
        "CONTROL"
    );
    assert_eq!(
        vendor_step(
            &mailed,
            &Gate {
                filled_email: true,
                ..device_gate()
            }
        ),
        Step::Mailbox
    );
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let approver = |cli| {
        VendorApprover::new(cli, "one", Some("one@example.com"), tmp.path(), Some(&home))
            .types_email
    };
    assert!(!approver(Cli::Kimi));
    assert!(approver(Cli::Grok) && approver(Cli::Codex), "CONTROL");
}

/// A row's own email is never read as a code: Kimi's card names it as its contact, and an
/// address such as `andy-2024@…` or `…@corp-2025.example` has the code's shape in it. CONTROL:
/// the same token on its own, outside an address, is still another code.
#[test]
fn a_code_shaped_part_of_the_rows_email_is_no_code() {
    for email in ["andy-2024@example.com", "kimi@corp-2025.example"] {
        let gate = Gate {
            email: Some(email),
            ..device_gate()
        };
        let card = kimi_card(KIMI_CARD_URL, email);
        assert_eq!(vendor_step(&card, &gate), Step::Press(0), "{email}");
        assert!(shows_email(&card, &gate), "{email}");
    }
    let gate = Gate {
        email: Some("andy-2024@example.com"),
        ..device_gate()
    };
    let nickname = page(
        KIMI_CARD_URL,
        "",
        "Allow this account to log in?\nandy-2024\nandy-2024@example.com\nCurrent",
        &["log in", "cancel", "switch account"],
        &[],
    );
    assert!(
        matches!(vendor_step(&nickname, &gate), Step::Refuse(_)),
        "CONTROL: {:?}",
        vendor_step(&nickname, &gate)
    );
}

/// A press on Kimi's card, whose code is only in its URL, is the drive's one confirm, never a
/// consent press. CONTROL: a press on a page that names no code is a consent press.
#[test]
fn a_press_on_kimis_card_is_its_one_confirm_and_another_press_is_consent() {
    let mut gate = device_gate();
    note_acted(
        &mut gate,
        &kimi_card(KIMI_CARD_URL, "kimi-one@example.com"),
        &Step::Press(0),
    );
    assert!(gate.confirmed && gate.pressed == 0, "{gate:?}");
    let mut other = device_gate();
    let consent = page(
        "https://auth.kimi.ai/device",
        "authorize",
        "Kimi Code wants to access your account",
        &["allow"],
        &[],
    );
    note_acted(&mut other, &consent, &Step::Press(0));
    assert!(!other.confirmed && other.pressed == 1, "CONTROL: {other:?}");
}

/// A stand-in browser that serves `page` to every read and answers each action as done, counting
/// its reads and its presses.
fn counting_cdp(
    page: Value,
    reads: Arc<AtomicUsize>,
    presses: Arc<AtomicUsize>,
) -> (CdpClient, std::thread::JoinHandle<()>) {
    let (to_browser_r, to_browser_w) = std::io::pipe().expect("pipe");
    let (from_browser_r, mut from_browser_w) = std::io::pipe().expect("pipe");
    let browser = std::thread::spawn(move || {
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                let expr = msg
                    .pointer("/params/expression")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let reply = match method {
                    "Target.createTarget" => json!({"id": id, "result": {"targetId": "t1"}}),
                    "Target.attachToTarget" => json!({"id": id, "result": {"sessionId": "s1"}}),
                    "Runtime.evaluate" if expr.contains("const verb = \"press\"") => {
                        presses.fetch_add(1, Ordering::SeqCst);
                        json!({"id": id, "result": {"result": {"value": {"ok": true}}}})
                    }
                    "Runtime.evaluate" => {
                        reads.fetch_add(1, Ordering::SeqCst);
                        json!({"id": id, "result": {"result": {"type": "object", "value": page.clone()}}})
                    }
                    _ => json!({"id": id, "result": {}}),
                };
                let mut bytes = aterm_json::to_vec(&reply).expect("reply");
                bytes.push(0);
                if from_browser_w.write_all(&bytes).is_err() || method == "Browser.close" {
                    return;
                }
            }
        }
    });
    (CdpClient::new(to_browser_w, from_browser_r), browser)
}

fn kimi_card_value(url: &str) -> Value {
    json!({
        "url": url,
        "heading": "",
        "text": "Allow this account to log in?\nKimi One\nkimi-one@example.com\nCurrent",
        "buttons": ["log in", "cancel", "switch account"],
        "inputs": []
    })
}

/// The DRIVE presses Kimi's card once: its press is the confirm, so the card read again is
/// waited on. CONTROL: a card naming another code is never pressed.
#[test]
fn the_drive_presses_kimis_card_once_and_never_one_naming_another_code() {
    use super::super::login_drive::Approval;
    let deadline = Instant::now() + Duration::from_secs(60);
    let no_mail = |_: &mut CdpClient, _: Option<&str>| Err("no mailbox in a test");
    let presses_of = |url: &str| {
        let reads = Arc::new(AtomicUsize::new(0));
        let presses = Arc::new(AtomicUsize::new(0));
        let (mut cdp, browser) = counting_cdp(
            kimi_card_value(url),
            Arc::clone(&reads),
            Arc::clone(&presses),
        );
        let seen = Arc::clone(&reads);
        let finished = move || seen.load(Ordering::SeqCst) >= 4;
        let got = drive_cdp_vendor(&mut cdp, url, device_gate(), &finished, deadline, no_mail);
        drop(cdp);
        browser.join().expect("stand-in browser");
        (got, presses.load(Ordering::SeqCst))
    };
    let (got, presses) = presses_of(KIMI_CARD_URL);
    assert_eq!(got, Approval::Approved { page: true });
    assert_eq!(presses, 1, "one confirm, then the card is waited on");
    let (got, presses) =
        presses_of("https://www.kimi.ai/code/authorize_device?user_code=ZZT8-KQW2");
    assert!(matches!(got, Approval::Refused(_)), "CONTROL: {got:?}");
    assert_eq!(presses, 0);
}

/// A page that waits on a person past the drive's stall ends it `NeedsSignIn`, naming the
/// command that signs the profile in once, never `NeedsYou`. CONTROL: a deadline that comes first
/// ends it `NeedsYou`, not approved in time.
#[test]
fn a_page_that_stalls_ends_the_drive_needs_sign_in() {
    use super::super::login_drive::Approval;
    let logged_out = json!({
        "url": KIMI_CARD_URL,
        "heading": "",
        "text": "Log in to Kimi\nContinue with Google\nSign in with SSO",
        "buttons": ["continue with google", "sign in with sso"],
        "inputs": []
    });
    let gate = Gate {
        sign_in_once: Some(SIGN_IN_ONCE),
        ..device_gate()
    };
    let no_mail = |_: &mut CdpClient, _: Option<&str>| Err("no mailbox in a test");
    let run = |stall: Duration, budget: Duration| {
        let (mut cdp, browser) = fake_cdp(logged_out.clone(), false);
        let got = drive_cdp_vendor_within(
            &mut cdp,
            KIMI_CARD_URL,
            gate,
            (&|| false, Instant::now() + budget),
            PageStall::within(stall),
            no_mail,
        );
        drop(cdp);
        browser.join().expect("stand-in browser");
        got
    };
    match run(Duration::from_millis(500), Duration::from_secs(60)) {
        Approval::NeedsSignIn(why) => {
            assert!(
                why.contains("waits on") && why.contains(SIGN_IN_ONCE),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    match run(Duration::from_secs(60), Duration::from_millis(1500)) {
        Approval::NeedsYou(why) => assert!(why.contains("not approved in time"), "{why}"),
        other => panic!("CONTROL: {other:?}"),
    }
}

/// The APPROVER opens nothing on a profile another of aterm's launches holds (a headless shell
/// writes no `SingletonLock`): it stops `NeedsYou`, naming that holder by its pid, before it looks
/// for a browser. CONTROL: once that launch ends, the profile is free to claim.
#[cfg(unix)]
#[test]
fn the_approver_opens_nothing_on_a_profile_another_launch_holds() {
    use super::super::accounts::Cli;
    use super::super::browser_window::ProfileLock;
    use super::super::login_drive::{Approval, Approver as _, Handoff};
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let shared = tmp.path().join("state/accounts");
    let home = tmp.path().join("home");
    let email = "kimi-one@example.com";
    let mut approver =
        VendorApprover::new(Cli::Kimi, "kimi-one", Some(email), &shared, Some(&home));
    let profile = approver.profile_dir(Some(email));
    std::fs::create_dir_all(&profile).expect("profile");
    let held = ProfileLock::claim(&profile).expect("free");
    let handoff = Handoff {
        url: KIMI_CARD_URL,
        code: Some(CODE),
        completion: Completion::Device,
        hosts: KIMI_GLOBAL,
        email: Some(email),
        finished: &|| false,
        deadline: Instant::now() + Duration::from_secs(60),
    };
    match approver.approve(&handoff) {
        Approval::NeedsYou(why) => assert!(
            why.contains("in use by another of aterm's sign-ins (pid")
                && why.contains(&std::process::id().to_string()),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    drop(held);
    assert!(
        ProfileLock::claim(&profile).is_ok(),
        "CONTROL: free once released"
    );
}

/// Claude Code's headless OAuth opens nothing on a persistent profile another launch holds, and
/// a lock naming another machine says to remove it rather than quit a browser that is not here.
/// CONTROL: a free profile is handed to the authorization, held for it.
#[cfg(unix)]
#[test]
fn claudes_headless_profile_is_claimed_for_its_authorization() {
    use super::super::browser_window::{PROFILE_LOCK_FILE, ProfileLock, profile_holder};
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let store = tmp.path().join("accounts");
    let profile = browser_profile_dir(&store, Some("work-1@example.com"));
    std::fs::create_dir_all(&profile).expect("profile");
    let held = ProfileLock::claim(&profile).expect("free");
    let ran = std::cell::Cell::new(false);
    let got = with_browser_profile(
        Some(&store),
        Some("work-1"),
        Some("work-1@example.com"),
        |_| {
            ran.set(true);
            Ok(())
        },
    );
    assert!(
        got.is_err_and(|why| why.contains("in use by another of aterm's sign-ins")),
        "{got:?}"
    );
    assert!(!ran.get());
    drop(held);
    std::os::unix::fs::symlink("other-desk.example-4242", profile.join("SingletonLock"))
        .expect("lock");
    let got = with_browser_profile(
        Some(&store),
        Some("work-1"),
        Some("work-1@example.com"),
        |_| {
            ran.set(true);
            Ok(())
        },
    );
    assert!(
        got.is_err_and(|why| why.contains("remove the profile's SingletonLock")),
        "{got:?}"
    );
    assert!(!ran.get());
    std::fs::remove_file(profile.join("SingletonLock")).expect("unlock");
    let got = with_browser_profile(
        Some(&store),
        Some("work-1"),
        Some("work-1@example.com"),
        |dir| {
            ran.set(true);
            assert!(profile_holder(dir).is_some(), "held while it runs");
            Ok(())
        },
    );
    assert_eq!(got, Ok(()), "CONTROL");
    assert!(ran.get() && profile.join(PROFILE_LOCK_FILE).is_file());
    assert_eq!(profile_holder(&profile), None, "released after it");
}

#[test]
fn plus_addressed_mail_with_base_delivered_to_and_json_vendor_filters() {
    // 1. Plus-addressed mail (`To: owner+2@example.com`) delivered into the base mailbox
    // (`Delivered-To: owner@example.com`, `Received: ... for <owner@example.com>`) must NOT
    // be taken by `owner@example.com` or `owner+1@example.com`, only by `owner+2@example.com`.
    let plus2_eml = format!(
        "Delivered-To: owner@example.com\r\n\
         Received: from mail.kimi.ai by mx.example.com for <owner@example.com>; {AFTER}\r\n\
         From: =?UTF-8?B?S2ltaSBBSSA8bm9yZXBseUBraW1pLmFpPg==?=\r\n\
         To: <owner+2@example.com>\r\n\
         Date: 2026-10-23T23:20:00Z\r\n\
         Subject: Kimi verification code\r\n\r\n\
         Your verification code is 918273.\r\n"
    );
    let (tmp, maildir) = maildir_with(&[("plus2.eml", &plus2_eml)]);
    let cfg = MailboxConfig {
        dir: Some(maildir),
        seen_file: Some(tmp.path().join("seen")),
        senders: vec!["kimi".to_owned()],
        since: Some(SINCE),
        ..MailboxConfig::default()
    };
    assert!(
        poll_mailbox_config_for_credential(
            &cfg,
            Some("owner@example.com"),
            Duration::from_millis(50)
        )
        .is_err(),
        "base mailbox owner@example.com must not steal owner+2@example.com's mail via Delivered-To"
    );
    assert!(
        poll_mailbox_config_for_credential(
            &cfg,
            Some("owner+1@example.com"),
            Duration::from_millis(50)
        )
        .is_err()
    );
    assert_eq!(
        poll_mailbox_config_for_credential(
            &cfg,
            Some("owner+2@example.com"),
            Duration::from_millis(50)
        ),
        Ok(MailboxCredential::Otp("918273".to_owned())),
        "owner+2@example.com receives its own mail with RFC 2047 From and ISO-8601 Date"
    );

    // 2. Commented From header (`noreply@kimi.ai (Kimi AI)`) and JSON mailbox APIs
    // (Microsoft Graph, Gmail API, MailHog v2) pass `MailFilter`.
    let filter = MailFilter {
        senders: &["kimi".to_owned(), "openai".to_owned()],
        since: Some(SINCE),
    };
    let commented_eml =
        format!("From: noreply@kimi.ai (Kimi AI)\r\nDate: {AFTER}\r\n\r\nCode: 123456\r\n");
    assert!(filter.admits(&commented_eml));

    let graph_item = r#"{
        "from": {"emailAddress": {"name": "OpenAI", "address": "noreply@tm.openai.com"}},
        "receivedDateTime": "2026-10-23T23:20:00Z",
        "body": {"content": "Your code is 654321"}
    }"#;
    assert!(filter.admits(graph_item));

    let gmail_item = r#"{
        "id": "msg_1",
        "internalDate": "1792797600000",
        "payload": {
            "headers": [
                {"name": "From", "value": "Kimi <noreply@kimi.ai>"},
                {"name": "To", "value": "owner@example.com"}
            ]
        }
    }"#;
    assert!(filter.admits(gmail_item));

    let mailhog_item = r#"{
        "From": {"Mailbox": "noreply", "Domain": "openai.com"},
        "Created": "2026-10-23T23:20:00Z"
    }"#;
    assert!(filter.admits(mailhog_item));

    // 3. `pressable` rejects negative compound labels starting with confirm/continue.
    let gate = Gate {
        hosts: &["auth.openai.com"],
        completion: Completion::Loopback,
        code: None,
        ..device_gate()
    };
    let traps = page(
        "https://auth.openai.com/consent",
        "authorize app",
        "Authorize access",
        &[
            "confirm logout",
            "confirm revoke",
            "continue without account",
            "continue to another account",
            "allow",
        ],
        &[],
    );
    assert_eq!(
        vendor_step(&traps, &gate),
        Step::Press(4),
        "negative compound buttons must be skipped in favor of allow"
    );
}

#[test]
fn cjk_punctuated_emails_readonly_inputs_and_vendor_otp_retry_work() {
    use super::super::login_drive::Approval;

    // 1. `emails_in` / `shows_email` / `another_account` extract emails joined to colons,
    // em-dashes, or CJK fullwidth punctuation (`账号：kimi-one@example.com（切换账号）`).
    let gate = Gate {
        hosts: &["auth.kimi.com", "www.kimi.com"],
        ..device_gate()
    };
    let cjk_own = page(
        "https://www.kimi.com/code/authorize_device?user_code=QXR4-MZK7",
        "",
        "是否允许登录此账号？\n当前账号：kimi-one@example.com（切换账号）",
        &["登录", "取消", "切换账号"],
        &[],
    );
    assert!(shows_email(&cjk_own, &gate));
    assert_eq!(vendor_step(&cjk_own, &gate), Step::Press(0));

    let cjk_other = page(
        "https://www.kimi.com/code/authorize_device?user_code=QXR4-MZK7",
        "",
        "是否允许登录此账号？\n当前账号：other@example.com—切换账号",
        &["登录", "取消", "切换账号"],
        &[],
    );
    assert!(!shows_email(&cjk_other, &gate));
    assert!(matches!(vendor_step(&cjk_other, &gate), Step::Refuse(_)));

    // 2. `oauth_step_js`, `webmail_step_js`, `VENDOR_PAGE_JS`, and `vendor_act_js` guard
    // against `readOnly` inputs, plus-sibling collisions, and step-1 username traps on 2FA.
    let oauth_js = oauth_step_js(Some("owner+2@example.com"), None, false, false);
    assert!(
        oauth_js
            .contains("!e.includes(\"+\") || !targetEmail.includes(\"+\") || e === targetEmail")
    );
    assert!(oauth_js.contains("!el.readOnly") && oauth_js.contains(".find(visibleControl)"));

    let wm_js = webmail_step_js(
        (Some("owner@example.com"), Some("pw"), None, Some("123456")),
        (true, true, false, false),
        0,
        &[],
    );
    assert!(wm_js.contains("const editableControl = (el) => visibleControl(el) && !el.readOnly"));
    assert!(wm_js.contains("if (userEl && !passEl && !totpInputs.length && !mpwEl && emailVal)"));

    assert!(VENDOR_PAGE_JS.contains("!i.readOnly"));
    let act_code = vendor_act_js(&Step::FillOtp, Some("123456"));
    assert!(act_code.contains("input[name*='code' i]"));
    let act_email = vendor_act_js(&Step::FillEmail, Some("owner@example.com"));
    assert!(act_email.contains("!el.readOnly"));

    // 3. `drive_cdp_vendor_within` retries mailbox polling when the first OTP is rejected and
    // the page remains on the verification code screen, then completes with the second OTP.
    let (to_browser_r, to_browser_w) = std::io::pipe().expect("pipe");
    let (from_browser_r, mut from_browser_w) = std::io::pipe().expect("pipe");
    let submitted_otps = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let submitted_clone = Arc::clone(&submitted_otps);
    let browser = std::thread::spawn(move || {
        let mut reader = to_browser_r;
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        let mut accepted = false;
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            for msg in split_nul_messages(&mut pending) {
                let id = msg.get("id").and_then(Value::as_u64).unwrap_or(0);
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                let expr = msg
                    .pointer("/params/expression")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let reply = match method {
                    "Target.createTarget" => json!({"id": id, "result": {"targetId": "t1"}}),
                    "Target.attachToTarget" => json!({"id": id, "result": {"sessionId": "s1"}}),
                    "Runtime.evaluate" if expr.contains("const verb = \"code\"") => {
                        if expr.contains("\"111111\"") {
                            submitted_clone.lock().unwrap().push("111111".to_owned());
                        } else if expr.contains("\"222222\"") {
                            submitted_clone.lock().unwrap().push("222222".to_owned());
                            accepted = true;
                        }
                        json!({"id": id, "result": {"result": {"value": {"ok": true}}}})
                    }
                    "Runtime.evaluate" => {
                        let page_val = if accepted {
                            json!({
                                "url": "http://127.0.0.1:1455/auth/callback?code=ok",
                                "heading": "done",
                                "text": "You can close this window",
                                "buttons": [],
                                "inputs": []
                            })
                        } else {
                            json!({
                                "url": "https://auth.openai.com/email-verification",
                                "heading": "check your email",
                                "text": "Enter the verification code we sent to your inbox",
                                "buttons": ["continue"],
                                "inputs": ["code"]
                            })
                        };
                        json!({"id": id, "result": {"result": {"type": "object", "value": page_val}}})
                    }
                    _ => json!({"id": id, "result": {}}),
                };
                let mut bytes = aterm_json::to_vec(&reply).expect("reply");
                bytes.push(0);
                if from_browser_w.write_all(&bytes).is_err() || method == "Browser.close" {
                    return;
                }
            }
        }
    });
    let mut cdp = CdpClient::new(to_browser_w, from_browser_r);
    let mut poll_count = 0usize;
    let otp_gate = Gate {
        hosts: &["auth.openai.com"],
        completion: Completion::Loopback,
        code: None,
        email: Some("codex-one@example.com"),
        senders: &["openai"],
        filled_email: true,
        types_email: true,
        ..device_gate()
    };
    let got = drive_cdp_vendor_within(
        &mut cdp,
        "https://auth.openai.com/email-verification",
        otp_gate,
        (&|| false, Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        PageStall::within(aterm_spec::wait::MUST_HAPPEN),
        |_, _| {
            poll_count += 1;
            if poll_count == 1 {
                Ok(MailboxCredential::Otp("111111".to_owned()))
            } else {
                Ok(MailboxCredential::Otp("222222".to_owned()))
            }
        },
    );
    drop(cdp);
    browser.join().expect("stand-in browser");
    assert_eq!(got, Approval::Approved { page: false });
    assert_eq!(
        *submitted_otps.lock().unwrap(),
        vec!["111111".to_owned(), "222222".to_owned()],
        "first rejected OTP must be followed by a second mailbox poll and OTP submission"
    );
}

#[test]
fn parameter_pollution_userinfo_loopback_json_plus_siblings_and_input_guards() {
    // 1. Conflicting duplicate query parameters (`query_value`, `extract_query_param`,
    // `FlowBound::outside`, `device_code`) and `@` userinfo loopback spoofing are rejected.
    assert_eq!(
        query_value("https://example.com/auth?state=s0&state=s0", "state").as_deref(),
        Some("s0")
    );
    assert_eq!(
        query_value("https://example.com/auth?state=s0&state=s1", "state"),
        None
    );
    assert_eq!(
        super::super::open_url::extract_login_hint(
            "https://claude.ai/oauth/authorize?login_hint=a%40example.com&login_hint=b%40example.com"
        ),
        None
    );
    let start = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_CODEX\
                 &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=s0";
    let bound = FlowBound::of(start).expect("unambiguous starting flow");
    assert_eq!(
        FlowBound::of("https://auth.openai.com/oauth/authorize"),
        Ok(FlowBound::default()),
        "absent parameters remain absent, not conflicting"
    );
    let empty =
        FlowBound::of("https://auth.openai.com/oauth/authorize?client_id=&redirect_uri=&state=")
            .expect("present empty parameters remain distinct from absence");
    assert_eq!(empty.client_id.as_deref(), Some(""));
    assert_eq!(empty.redirect_uri.as_deref(), Some(""));
    assert_eq!(empty.state.as_deref(), Some(""));
    let identical = FlowBound::of(&format!(
        "{start}&client_id=app_CODEX&redirect_uri=http%3a%2f%2flocalhost%3a1455%2fauth%2fcallback&state=s0"
    ))
    .expect("identical decoded duplicates keep their binding");
    assert_eq!(identical, bound);
    for conflicting in [
        "client_id=app_OTHER",
        "redirect_uri=http%3A%2F%2Flocalhost%3A1999%2Fauth%2Fcallback",
        "state=s1",
    ] {
        assert!(
            FlowBound::of(&format!("{start}&{conflicting}")).is_err(),
            "an ambiguous starting flow must never become an unbound flow"
        );
    }
    for valid in [&bound, &identical] {
        let gate = Gate {
            hosts: &["auth.openai.com"],
            completion: Completion::Loopback,
            code: None,
            bound: Some(valid),
            ..device_gate()
        };
        assert_eq!(
            vendor_step(
                &page(
                    "http://localhost:1455/auth/callback?code=x&state=s0",
                    "",
                    "",
                    &[],
                    &[]
                ),
                &gate
            ),
            Step::Done,
            "the identical starting flow's own callback still completes"
        );
        assert!(matches!(
            vendor_step(
                &page(
                    "http://localhost:7777/auth/callback?code=x&state=s0",
                    "",
                    "",
                    &[],
                    &[]
                ),
                &gate
            ),
            Step::Refuse(_)
        ));
    }
    assert!(
        bound
            .outside(
                "https://auth.openai.com/oauth/authorize?client_id=app_CODEX&client_id=app_OTHER",
                &["auth.openai.com"]
            )
            .is_some(),
        "HTTP parameter pollution on client_id must be refused"
    );
    assert!(
        bound
            .outside(
                "https://auth.openai.com/oauth/authorize?client_id=app_CODEX\
                 &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback\
                 &redirect_uri=https%3A%2F%2Fevil.example%2Fcb",
                &["auth.openai.com"]
            )
            .is_some(),
        "HTTP parameter pollution on redirect_uri must be refused"
    );
    assert!(
        bound
            .outside(
                "https://auth.openai.com/oauth/authorize?client_id=app_CODEX\
                 &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=s1",
                &["auth.openai.com"]
            )
            .is_some(),
        "mismatched OAuth state on authorize URL must be refused"
    );
    let unbound = FlowBound::default();
    assert!(
        unbound
            .outside(
                "https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1455%40evil.example%2Fcb",
                &["auth.openai.com"]
            )
            .is_some(),
        "userinfo spoofing http://localhost:1455@evil.example must not pass as loopback"
    );

    let loopback_gate = Gate {
        hosts: &["auth.openai.com"],
        completion: Completion::Loopback,
        code: None,
        email: Some("codex-one@example.com"),
        senders: &["openai"],
        filled_email: true,
        bound: Some(&bound),
        ..device_gate()
    };
    assert!(matches!(
        vendor_step(
            &page(
                "http://localhost:1455@evil.example/auth/callback?code=x&state=s0",
                "",
                "",
                &[],
                &[]
            ),
            &loopback_gate
        ),
        Step::Stop(_)
    ));
    assert!(matches!(
        vendor_step(
            &page(
                "http://localhost:1455/auth/callback?code=x&state=s1",
                "",
                "",
                &[],
                &[]
            ),
            &loopback_gate
        ),
        Step::Refuse(_)
    ));
    assert!(matches!(
        vendor_step(
            &page(
                "http://localhost:1455/auth/callback?error=access_denied&state=s0",
                "",
                "",
                &[],
                &[]
            ),
            &loopback_gate
        ),
        Step::Refuse(_)
    ));

    // Duplicate conflicting `user_code` in device URL is refused.
    let dev_gate = device_gate();
    let polluted_card = page(
        &format!("https://www.kimi.ai/code/authorize_device?user_code={CODE}&user_code=XXXX-9999"),
        "authorize device",
        "Signed in as kimi-one@example.com\nSwitch account",
        &["log in", "switch account"],
        &[],
    );
    assert!(matches!(
        vendor_step(&polluted_card, &dev_gate),
        Step::Refuse(_)
    ));

    // 2. Regional IDP domains and CJK sign-up / reject / logout labels.
    assert!(matches!(
        vendor_step(
            &page(
                "https://accounts.google.co.uk/o/oauth2/v2/auth",
                "sign in",
                "",
                &[],
                &[]
            ),
            &dev_gate
        ),
        Step::SignIn(_)
    ));
    assert!(matches!(
        vendor_step(
            &page(
                "https://www.kimi.com/auth/login",
                "注册账号",
                "欢迎注册",
                &["continue"],
                &[]
            ),
            &Gate {
                hosts: &["www.kimi.com"],
                ..dev_gate
            }
        ),
        Step::Stop(_)
    ));
    assert_eq!(
        vendor_step(
            &page(
                "https://www.kimi.com/code/authorize_device",
                "设备授权",
                &format!("账号：kimi-one@example.com\n{CODE}"),
                &["取消授权", "退出登录", "注册新账号", "confirm"],
                &[]
            ),
            &Gate {
                hosts: &["www.kimi.com"],
                ..dev_gate
            }
        ),
        Step::Press(3)
    );

    // 3. JSON mailbox payload with primary `To: owner+2@example.com` and envelope/header
    // `Delivered-To: owner@example.com` must NOT match `owner@example.com` or `owner+1@example.com`.
    let json_plus_mail = r#"{
        "payload": {
            "headers": [
                {"name": "Delivered-To", "value": "owner@example.com"},
                {"name": "To", "value": "owner+2@example.com"},
                {"name": "From", "value": "OpenAI <noreply@tm.openai.com>"}
            ]
        },
        "envelope": {"to": ["owner@example.com"]},
        "snippet": "Your verification code is 864209"
    }"#;
    assert!(
        !super::super::open_url::email_matches_target(json_plus_mail, "owner@example.com"),
        "base mailbox must not steal JSON mail explicitly addressed to owner+2@example.com"
    );
    assert!(
        !super::super::open_url::email_matches_target(json_plus_mail, "owner+1@example.com"),
        "sibling +1 must not steal JSON mail explicitly addressed to owner+2@example.com"
    );
    assert!(
        super::super::open_url::email_matches_target(json_plus_mail, "owner+2@example.com"),
        "target owner+2@example.com must match its own JSON mail"
    );

    // 4. `VENDOR_PAGE_JS` classifies `<input type="password" name="email_password">` as
    // `"password"` before `"email"`, and `CODE_INPUT_HELPERS_JS` / `vendor_act_js` exclude
    // `submit`/`button`/`password` inputs from code and email filling.
    let pw_idx = VENDOR_PAGE_JS
        .find("if (t === \"password\") return \"password\";")
        .expect("password check");
    let em_idx = VENDOR_PAGE_JS
        .find("if (t === \"email\"")
        .expect("email check");
    assert!(
        pw_idx < em_idx,
        "password inputs must be classified before name*='email' checks"
    );
    let act_code = vendor_act_js(&Step::FillOtp, Some("123456"));
    assert!(act_code.contains("\"submit\"") && act_code.contains(".filter(textInput)"));
}

#[test]
fn cjk_otp_extraction_webmail_plus_siblings_and_spa_submit_guards() {
    let link = "https://claude.ai/magic-link?token=recipient-bound&state=own";
    let check_identity = |address: &str| {
        assert!(super::super::accounts::email_refusal(address).is_none());
        let plain = format!(
            "To: {address}\r\n\r\nYour verification code is 654321.\r\nSign in: {link}\r\n"
        );
        let json = format!(
            r#"{{"to":"{address}","text":"Your verification code is 654321. Sign in: {link}"}}"#
        );
        for mail in [plain, json] {
            assert!(super::super::open_url::email_matches_target(&mail, address));
            assert!(!super::super::open_url::email_matches_target(
                &mail,
                "owner@example.com"
            ));
            assert_eq!(
                extract_otps_from_email(&mail, Some(address)),
                ["654321".to_owned()],
                "{address}"
            );
            assert!(extract_otps_from_email(&mail, Some("owner@example.com")).is_empty());
            assert_eq!(
                super::super::open_url::extract_magic_links_from_email(&mail, Some(address)),
                [link.to_owned()],
                "{address}"
            );
            assert!(
                super::super::open_url::extract_magic_links_from_email(
                    &mail,
                    Some("owner@example.com")
                )
                .is_empty()
            );
        }
    };
    for punctuation in "!#$%&'*+-/=?^_`{|}~".chars() {
        for local in [
            format!("{punctuation}owner"),
            format!("audit{punctuation}owner"),
            format!("owner{punctuation}"),
        ] {
            check_identity(&format!("{local}@example.com"));
        }
    }
    for local in [
        "审计owner",
        "e\u{301}owner",
        "审计（owner",
        "审计：owner",
        "审计，owner",
    ] {
        check_identity(&format!("{local}@example.com"));
    }
    for recipient in [
        r#""audit,owner@example.com,else""#,
        r#""audit owner@example.com else""#,
        r#""audit,owner@example.com"#,
        r#""audit\" owner@example.com else""#,
        r#""audit\" owner@example.com"#,
        r#"audit\owner@example.com"#,
    ] {
        let mail = format!(
            "To: {recipient}\r\n\r\nYour verification code is 654321.\r\nSign in: {link}\r\n"
        );
        assert!(!super::super::open_url::email_matches_target(
            &mail,
            "owner@example.com"
        ));
        assert!(extract_otps_from_email(&mail, Some("owner@example.com")).is_empty());
        assert!(
            super::super::open_url::extract_magic_links_from_email(
                &mail,
                Some("owner@example.com")
            )
            .is_empty()
        );
    }
    let named = "To: \"Owner Name\" <owner@example.com>\r\n\r\nYour code is 654321.\r\n";
    assert_eq!(
        extract_otps_from_email(named, Some("owner@example.com")),
        ["654321".to_owned()]
    );

    // 1. CJK OTP extraction: unspaced CJK verification lines with embedded ASCII letters
    // (`您的Kimi登录验证码是839201，请在5分钟内使用。`, `【Kimi】验证码 123-456（5分钟内有效）`)
    // prioritize the real 6-digit OTP and do not promote unspaced non-context CJK numbers
    // (`客服电话：100080`) as standalone OTPs ahead of context-matched codes.
    let cjk_mail = concat!(
        "From: Kimi <noreply@kimi.com>\r\n",
        "To: owner@example.com\r\n",
        "Subject: 登录验证码\r\n\r\n",
        "客服邮编：100080\r\n",
        "您的Kimi登录验证码是839201，请在5分钟内使用。\r\n",
    );
    assert_eq!(
        extract_otps_from_email(cjk_mail, Some("owner@example.com")),
        vec!["839201".to_owned(), "100080".to_owned()],
        "CJK context OTP must be extracted even when surrounded by CJK chars and ASCII brand/minute numbers, and prioritized ahead of non-context CJK numbers"
    );

    let cjk_hyphen_mail = concat!(
        "From: Kimi <noreply@kimi.com>\r\n",
        "To: owner@example.com\r\n\r\n",
        "【Kimi】您的登录邮箱为：owner@example.com，安全码：456-789（10分钟内有效）。\r\n",
    );
    assert_eq!(
        extract_otps_from_email(cjk_hyphen_mail, Some("owner@example.com")),
        vec!["456789".to_owned()]
    );

    // 2. Base64 / Quoted-Printable / HTML-entity body recipient matching and envelope-only
    // (`Delivered-To:`) plus-sibling protection (used by `poll_webmail_via_cdp` and catch-all
    // mailboxes).
    let b64_plus_body = "Sign in code for owner+1@example.com: 654321\r\n";
    let b64_encoded = "U2lnbiBpbiBjb2RlIGZvciBvd25lcisxQGV4YW1wbGUuY29tOiA2NTQzMjE=";
    let _ = b64_plus_body;
    let envelope_plus_eml = format!(
        "Delivered-To: owner@example.com\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n\
         {b64_encoded}\r\n"
    );
    assert!(
        !super::super::open_url::email_matches_target(&envelope_plus_eml, "owner@example.com"),
        "Delivered-To envelope match must be rejected when decoded Base64 body names plus-sibling owner+1@example.com"
    );
    assert!(
        !super::super::open_url::email_matches_target(&envelope_plus_eml, "owner+2@example.com"),
        "Delivered-To envelope match must be rejected for sibling owner+2@example.com"
    );
    assert!(
        super::super::open_url::email_matches_target(&envelope_plus_eml, "owner+1@example.com"),
        "owner+1@example.com matches its own Base64-encoded body"
    );

    let cjk_body_only_eml = concat!(
        "From: Kimi <noreply@kimi.com>\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n\r\n",
        "亲爱的用户（owner+2@example.com）：您的验证码为 314159。\r\n",
    );
    assert!(
        !super::super::open_url::email_matches_target(cjk_body_only_eml, "owner@example.com"),
        "CJK fullwidth parens around owner+2@example.com must isolate it from base owner@example.com"
    );
    assert!(
        super::super::open_url::email_matches_target(cjk_body_only_eml, "owner+2@example.com"),
        "CJK fullwidth parens around owner+2@example.com must match target owner+2@example.com"
    );

    // 3. `MailboxFlow` enforces `FlowBound` (rejecting conflicting `client_id`, state mismatch,
    // external `redirect_uri`, and callback `?error=`), and `loopback_http_exchange` rejects
    // userinfo spoofing (`http://127.0.0.1:80@evil.example:80/`).
    let auth_url = "https://claude.ai/oauth/authorize?client_id=cid_1\
                    &redirect_uri=http%3A%2F%2F127.0.0.1%3A43123%2Fcallback&state=st_1";
    let flow = super::super::open_url::MailboxFlow::new(auth_url).expect("valid flow");
    assert!(
        super::super::open_url::MailboxFlow::new(
            "https://claude.ai/oauth/authorize?client_id=cid_1&client_id=cid_2\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A43123%2Fcallback&state=st_1"
        )
        .is_err(),
        "conflicting client_id parameters must be rejected by MailboxFlow"
    );
    assert!(
        !flow.admits_provider_url("https://claude.ai/oauth/authorize?client_id=cid_2&state=st_1"),
        "MailboxFlow must reject provider URL for another client_id"
    );
    assert!(
        !flow.admits_provider_url(
            "https://claude.ai/login?redirect_uri=https%3A%2F%2Fevil.example%2Fcb"
        ),
        "MailboxFlow must reject provider URL with external redirect_uri"
    );
    assert!(
        !flow
            .is_callback("http://127.0.0.1:43123/callback?code=abc&state=st_1&error=access_denied"),
        "MailboxFlow must reject callback URLs carrying an OAuth error parameter"
    );
    assert!(flow.is_callback("http://127.0.0.1:43123/callback?code=abc&state=st_1"));

    let mut cookies = std::collections::BTreeMap::new();
    assert_eq!(
        super::super::open_url::loopback_http_exchange(
            "GET",
            "http://127.0.0.1:80@evil.example:80/cb",
            &mut cookies,
            None
        )
        .err(),
        Some("loopback URL authority is not localhost or 127.0.0.1")
    );

    // 4. `VENDOR_PAGE_JS`, `vendor_act_js`, and `SUBMIT_INPUT_HELPERS_JS` classify
    // `<input type="text" name="email_code">` as `"code"` before `"email"` and guard
    // form-less SPA `submitInput` against `"Continue with Google"` / cancel buttons.
    let code_idx = VENDOR_PAGE_JS
        .find("return \"code\";")
        .expect("code classification");
    let name_email_idx = VENDOR_PAGE_JS
        .find("if (n.includes(\"email\")) return \"email\";")
        .expect("name email classification");
    assert!(
        code_idx < name_email_idx,
        "code attributes (name*='code', name*='otp', one-time-code, numeric) must be checked before name.includes('email')"
    );
    let act_email = vendor_act_js(&Step::FillEmail, Some("owner@example.com"));
    assert!(
        act_email.contains("isCodeField") && act_email.contains("disallowedSubmitLabel"),
        "vendor_act_js must exclude code fields when filling email and guard submitInput against third-party/cancel buttons"
    );
}
