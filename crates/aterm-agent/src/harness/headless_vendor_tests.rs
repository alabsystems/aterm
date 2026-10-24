// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the vendor sign-in pages' decision core ([`vendor_step`], a pure page model), the
//! vendors' mail senders, the per-CLI rosters' label aliases and the fleet rosters' exemption
//! from the `~/.secrets` mailbox catch-all.

use super::*;
use crate::ai::connector::Completion;

const KIMI_GLOBAL: &[&str] = &["auth.kimi.ai", "www.kimi.ai"];
const CODE: &str = "QXR4-MZK7";

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
        Step::Stop(why) => assert!(why.contains("another identity provider"), "{why}"),
        step => panic!("{step:?}"),
    }
    let password = page(
        "https://auth.kimi.ai/login",
        "sign in",
        "Password",
        &["sign in"],
        &[Field::Password],
    );
    assert!(matches!(vendor_step(&password, &gate), Step::Stop(_)));
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

/// A vendor sign-in stays within the flow it started: a mail from another sender is not
/// followed, and a page naming another client, or handing its login elsewhere, is never pressed.
#[test]
fn a_vendor_sign_in_presses_nothing_outside_the_flow_it_started() {
    let start = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_CODEX\
                 &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=s0&scope=openid";
    let bound = FlowBound::of(start);
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
