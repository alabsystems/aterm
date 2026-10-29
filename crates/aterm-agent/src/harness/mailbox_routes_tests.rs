// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::headless_oauth::{MailboxConfig, poll_mailbox_config_for_credential};
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The longest any wait here may take before it is a hang.
const HANG: Duration = Duration::from_secs(60);

fn config(text: &str) -> MailboxConfig {
    MailboxConfig::from_toml_str(text, None).expect("mailbox toml parses")
}

/// A loopback IMAP server that counts every connection and every LOGIN it is sent, and refuses
/// each login; its `imap://` URL.
fn refusing_imap() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind imap");
    let url = format!(
        "imap://127.0.0.1:{}/INBOX",
        listener.local_addr().expect("addr").port()
    );
    let (connections, logins) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c, l) = (Arc::clone(&connections), Arc::clone(&logins));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            c.fetch_add(1, Ordering::SeqCst);
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = BufReader::new(stream);
            let _ = writer.write_all(b"* OK [CAPABILITY IMAP4rev1] ready\r\n");
            let _ = writer.flush();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let tag = line.split_whitespace().next().unwrap_or("a").to_owned();
                if line.contains(" LOGIN ") {
                    l.fetch_add(1, Ordering::SeqCst);
                    let _ = write!(
                        writer,
                        "{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n"
                    );
                } else {
                    let _ = write!(writer, "{tag} BAD not logged in\r\n");
                }
                let _ = writer.flush();
                line.clear();
            }
        }
    });
    (url, connections, logins)
}

/// RESOLUTION ORDER: for a target email an exact `receives` pattern (or its base address
/// without a `+tag`) beats `*@domain`, which beats `*`, which beats the top-level fields; among
/// equals the first entry wins; every reader (login, password, second password, TOTP, URLs,
/// configured-for) answers from the one entry. NEGATIVE CONTROL: resolving by first match
/// instead of by tier (`tier < held` read as `best.is_none()`) answers the `*` entry for the
/// exact and domain targets, and this fails.
#[test]
fn resolution_takes_exact_then_domain_then_any_then_the_top_level_mailbox() {
    let text = r#"
user = "legacy@example.test"
password = "legacy-pw"
webmail_url = "https://legacy.example.test/inbox"

[[mailboxes]]
login = "catchall@example.test"
password = "catch-pw"
webmail_url = "https://catch.example.test/inbox"
receives = ["*"]

[[mailboxes]]
login = "domain@example.test"
password = "domain-pw"
webmail_url = "https://domain.example.test/inbox"
receives = ["*@pm.example.test"]

[[mailboxes]]
user = "Exact@Example.TEST"
password = "exact-pw"
mailbox_password = "exact-second"
totp_secret = "JBSWY3DPEHPK3PXP"
imap_url = "imaps://imap.exact.example.test/INBOX"
receives = [" AI1@PM.example.test ", "not a pattern", "ai2@other.example.test"]

[[mailboxes]]
login = "late@example.test"
password = "late-pw"
webmail_url = "https://late.example.test/inbox"
receives = ["ai1@pm.example.test", "*@pm.example.test"]
"#;
    let cfg = config(text);
    assert_eq!(cfg.mailboxes.len(), 4);
    assert_eq!(
        cfg.mailboxes[2].receives,
        ["ai1@pm.example.test", "ai2@other.example.test"],
        "patterns are trimmed and lowercased, and one that is no pattern is dropped"
    );
    let login = |t: &str| cfg.webmail_user_for(Some(t));
    for (target, want) in [
        ("ai1@pm.example.test", "Exact@Example.TEST"),
        ("AI1+claude@pm.example.test", "Exact@Example.TEST"),
        ("ai2@other.example.test", "Exact@Example.TEST"),
        ("someone@pm.example.test", "domain@example.test"),
        ("anyone@elsewhere.example.test", "catchall@example.test"),
    ] {
        assert_eq!(login(target).as_deref(), Some(want), "{target}");
    }
    let exact = Some("ai1@pm.example.test");
    assert_eq!(cfg.password_for(exact).as_deref(), Some("exact-pw"));
    assert_eq!(
        cfg.mailbox_password_for(exact).as_deref(),
        Some("exact-second")
    );
    assert_eq!(
        cfg.totp_secret_for(exact).as_deref(),
        Some("JBSWY3DPEHPK3PXP")
    );
    assert_eq!(
        cfg.effective_imap_url(exact).as_deref(),
        Some("imaps://imap.exact.example.test/INBOX")
    );
    assert_eq!(
        cfg.effective_webmail_url(exact),
        None,
        "an entry that names its own URL gets no other, never the top-level one"
    );
    assert!(cfg.is_configured_for(exact));
    let domain = Some("someone@pm.example.test");
    assert_eq!(cfg.password_for(domain).as_deref(), Some("domain-pw"));
    assert_eq!(
        cfg.totp_secret_for(domain),
        None,
        "no TOTP leaks from another entry"
    );
    assert_eq!(
        cfg.effective_webmail_url(domain).as_deref(),
        Some("https://domain.example.test/inbox")
    );
    let routed = cfg.routed_for(exact);
    assert_eq!(routed.user.as_deref(), Some("Exact@Example.TEST"));
    assert_eq!(routed.password.as_deref(), Some("exact-pw"));
    assert!(routed.passwords.is_empty() && routed.mailboxes.is_empty() && routed.routed);
    assert_eq!(routed.webmail_url, None);
    assert_eq!(routed.webmail_user_for(exact), cfg.webmail_user_for(exact));

    let without_any = config(&text.replacen("receives = [\"*\"]", "receives = []", 1));
    let other = Some("anyone@elsewhere.example.test");
    assert_eq!(
        without_any.webmail_user_for(other).as_deref(),
        Some("legacy@example.test"),
        "no pattern matches: the top-level mailbox"
    );
    assert_eq!(
        without_any.password_for(other).as_deref(),
        Some("legacy-pw")
    );
    assert!(matches!(
        without_any.routed_for(other),
        std::borrow::Cow::Borrowed(_)
    ));
}

/// LEGACY COMPATIBILITY: a file with no `[[mailboxes]]` reads exactly as before (one mailbox,
/// the top-level `user` for every target, per-email passwords by key), and a file that adds an
/// entry for other addresses still answers its own targets from the top level. NEGATIVE
/// CONTROL: a resolver that falls back to the first entry where no pattern matches
/// (`best.map(..).or(routes.first())`) hands the top-level targets the entry's login, and this
/// fails.
#[test]
fn the_single_mailbox_format_reads_as_it_did() {
    let legacy = r#"
user = "box@example.test"
password = "default-pw"
webmail_url = "https://mail.example.test/inbox"

[passwords]
"m27@example.test" = "per-email-pw"
"#;
    let cfg = config(legacy);
    assert!(cfg.mailboxes.is_empty());
    for target in ["m27@example.test", "other@example.test"] {
        assert_eq!(
            cfg.webmail_user_for(Some(target)).as_deref(),
            Some("box@example.test"),
            "the top-level user overrides every target"
        );
        assert!(matches!(
            cfg.routed_for(Some(target)),
            std::borrow::Cow::Borrowed(_)
        ));
    }
    assert_eq!(
        cfg.password_for(Some("m27@example.test")).as_deref(),
        Some("per-email-pw")
    );
    assert_eq!(
        cfg.password_for(Some("other@example.test")).as_deref(),
        Some("default-pw"),
        "the top-level password is every other target's"
    );
    for target in ["m27@example.test", "other@example.test"] {
        assert_eq!(
            cfg.effective_webmail_url(Some(target)).as_deref(),
            Some("https://mail.example.test/inbox")
        );
    }

    let both = config(&format!(
        "{legacy}\n[[mailboxes]]\nlogin = \"own@example.test\"\npassword = \"own-pw\"\n\
         receives = [\"g1@mail.example.test\"]\n"
    ));
    let target = Some("m27@example.test");
    assert_eq!(both.webmail_user_for(target), cfg.webmail_user_for(target));
    assert_eq!(both.password_for(target), cfg.password_for(target));
    assert_eq!(
        both.effective_webmail_url(target),
        cfg.effective_webmail_url(target)
    );
    assert_eq!(
        both.webmail_user_for(Some("g1@mail.example.test"))
            .as_deref(),
        Some("own@example.test")
    );
}

/// AN EMPTY OR MISSING PASSWORD IS NEVER TRIED: an entry with `password = ""` (or none) makes no
/// connection at all, keeps the top-level mailbox off its addresses, and is reported as
/// `mailbox ab…@… has no password yet`, never as a failed sign-in; a top-level IMAP mailbox
/// with no password sends no LOGIN either. NEGATIVE CONTROL: dropping the password from
/// [`MailboxRoute::ready`] (a login alone) hands the entry's IMAP URL to its addresses, and
/// dropping `password.is_empty() ||` from `fetch_imap_credential` lets the top-level poll
/// connect; each fails this.
#[test]
fn a_mailbox_with_no_password_is_never_tried() {
    let (url, connections, logins) = refusing_imap();
    for password_line in ["password = \"\"\n", ""] {
        let cfg = config(&format!(
            "user = \"legacy@example.test\"\npassword = \"legacy-pw\"\nimap_url = \"{url}\"\n\
             [[mailboxes]]\nlogin = \"waiting@example.test\"\n{password_line}imap_url = \"{url}\"\n\
             receives = [\"ai@pm.example.test\"]\n"
        ));
        let target = Some("ai@pm.example.test");
        assert_eq!(cfg.password_for(target), None);
        assert_eq!(cfg.effective_imap_url(target), None);
        assert!(!cfg.is_configured_for(target));
        assert_eq!(
            cfg.unready_words(target),
            Some("mailbox wa…@… has no password yet")
        );
        assert_eq!(
            poll_mailbox_config_for_credential(&cfg, target, HANG),
            Err("mailbox wa…@… has no password yet")
        );
    }
    let legacy = MailboxConfig {
        imap_url: Some(url),
        user: Some("legacy@example.test".to_owned()),
        ..MailboxConfig::default()
    };
    assert!(
        poll_mailbox_config_for_credential(
            &legacy,
            Some("legacy@example.test"),
            Duration::from_millis(300)
        )
        .is_err()
    );
    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "no connection was made"
    );
    assert_eq!(logins.load(Ordering::SeqCst), 0, "no login was sent");
}

/// A REFUSED LOGIN IS NOT RETRIED FAST: the first refusal ends the poll at once with the masked
/// reason, is recorded under the accounts store, and the next poll (another row's sign-in)
/// sends no login until the wait (15 minutes for the first refusal) ends; an accepted login
/// clears the record. NEGATIVE CONTROL: a `note_login` that records no refusal lets the poll
/// log in again every 150 ms until it times out, and this fails on its words.
#[test]
fn a_refused_mailbox_login_waits_before_it_is_tried_again() {
    let (url, _connections, logins) = refusing_imap();
    let store = aterm_tempfile::tempdir().expect("tempdir");
    let record = store.path().join(LOGIN_RECORD_FILE);
    let mut cfg = config(&format!(
        "[[mailboxes]]\nlogin = \"refused@example.test\"\npassword = \"wrong\"\n\
         imap_url = \"{url}\"\nreceives = [\"*@pm.example.test\"]\n"
    ));
    cfg.login_record = Some(record.clone());
    let target = Some("ai@pm.example.test");
    let words = refused_words("refused@example.test");
    assert!(
        words.starts_with("mailbox re…@… refused its last sign-in"),
        "{words}"
    );
    let started = std::time::Instant::now();
    assert_eq!(
        poll_mailbox_config_for_credential(&cfg, target, Duration::from_secs(3)),
        Err(words)
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "it ended at once"
    );
    assert_eq!(logins.load(Ordering::SeqCst), 1);
    let now = unix_now();
    let source = login_source(&url);
    let until = login_held(Some(&record), &source, "REFUSED@example.test", now).expect("held");
    assert!((now + 890..=now + 900).contains(&until), "{until} vs {now}");
    let text = std::fs::read_to_string(&record).expect("record");
    assert!(
        !text.contains("refused") && !text.contains('@'),
        "no login is written: {text}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&record)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    assert_eq!(
        poll_mailbox_config_for_credential(&cfg, target, Duration::from_secs(3)),
        Err(words),
        "the next sign-in is refused from the record"
    );
    assert_eq!(logins.load(Ordering::SeqCst), 1, "no second login was sent");
    assert!(login_held(Some(&record), &source, "refused@example.test", until).is_none());
    assert_eq!(
        login_held(
            Some(&record),
            "https://mail.example.test",
            "refused@example.test",
            now
        ),
        None,
        "a refusal over IMAP holds no other source"
    );

    note_login_refused(Some(&record), &source, "refused@example.test", now);
    let doubled = login_held(Some(&record), &source, "refused@example.test", now).expect("held");
    assert_eq!(
        doubled,
        now + 2 * 900,
        "the second refusal in a row waits twice as long"
    );
    note_login_accepted(Some(&record), &source, "refused@example.test");
    assert_eq!(
        login_held(Some(&record), &source, "refused@example.test", now),
        None
    );
}

const OLD_TOML: &str = "\
# The owner's mailbox file: every byte of it outside `receives` must survive.
user = 'legacy@example.test'
password = 'top \"level\" pw'
totp_secret = \"otpauth://totp/Ex:legacy%40example.test?secret=JBSWY3DPEHPK3PXP\"

[passwords]
\"m27@example.test\" = \"per-email\\\\pw\"

[[mailboxes]]
# the shared inbox
login = \"Owner@Proton.example.test\"
password = 'p@ss#word\"]'
mailbox_password = \"second \\u00e9 pw\"
totp_secret = 'KRSXG5A'
receives = [
  \"*@pm.example.test\",   # every pm alias
  \"moved@gmail.example.test\",
]

[[mailboxes]]
login = \"keep@example.test\"
password = \"keep-pw\"
";

const CSV: &str = "\u{feff}computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)\r\n\
m3,ai1@proton.example.test,claude,Owner@Proton.example.test,proton alias,\r\n\
m3,moved@gmail.example.test,claude,\"Owner@Proton.example.test\",\"guess, wrong\",its own mailbox: moved@gmail.example.test\r\n\
m22,own@gmail.example.test,codex,itself,gmail,\r\n\
m22,keepme@example.test,claude,owner@proton.example.test,guess,keep@example.test\r\n\
m22,ai1@proton.example.test,codex,owner@proton.example.test,same account on two Macs,\r\n\
m29,nobody@example.test,claude,the Proton inbox,no address,\r\n\
m29,,claude,owner@proton.example.test,no email,\r\n";

/// THE IMPORTER KEEPS EVERY SECRET AND APPLIES THE CORRECTION: every byte outside the changed
/// `receives` values and the appended entries is the old file's (comments, quoting, escapes and
/// every password, second password and TOTP secret); a non-blank correction overrides the guess
/// (the moved Gmail address leaves the shared inbox for its own new entry); `itself` routes an
/// address to its own mailbox; a login the file lacks gets an entry with an empty password,
/// which is never tried; rows without an email or a login are counted as skipped; the result
/// is written atomically with mode 600 and only counts and masked logins are printed.
/// NEGATIVE CONTROL: reading the guess even where a correction is given leaves the moved
/// address in the shared inbox, and widening the replaced span past the `receives` value
/// makes the read-back refuse the merged file; each fails this.
#[test]
fn the_importer_keeps_every_secret_and_applies_the_correction() {
    let plan = plan_import(CSV, Some(OLD_TOML), None).expect("planned");
    assert!(plan.changed);
    let old_entries = OLD_TOML.split("receives = [").next().expect("head");
    assert!(
        plan.text.starts_with(old_entries),
        "everything before the first receives is kept byte for byte:\n{}",
        plan.text
    );
    for line in OLD_TOML.lines().filter(|l| {
        l.contains("password") || l.contains("totp") || l.starts_with('#') || l.starts_with("login")
    }) {
        assert!(plan.text.lines().any(|n| n == line), "kept: {line}");
    }
    let new = config(&plan.text);
    let old = config(OLD_TOML);
    assert_eq!(new.password, old.password);
    assert_eq!(new.totp_secret, old.totp_secret);
    assert_eq!(new.passwords, old.passwords);
    for (was, now) in old.mailboxes.iter().zip(&new.mailboxes) {
        assert_eq!(
            (
                &was.login,
                &was.password,
                &was.mailbox_password,
                &was.totp_secret
            ),
            (
                &now.login,
                &now.password,
                &now.mailbox_password,
                &now.totp_secret
            )
        );
    }
    assert_eq!(new.mailboxes[0].password.as_deref(), Some("p@ss#word\"]"));
    assert_eq!(
        new.mailboxes[0].mailbox_password.as_deref(),
        Some("second \u{e9} pw")
    );
    assert_eq!(
        new.mailboxes[0].receives,
        ["*@pm.example.test", "ai1@proton.example.test"],
        "the wildcard stays, the moved address leaves, the guessed one joins"
    );
    assert_eq!(
        new.mailboxes[1].receives,
        ["keepme@example.test"],
        "the correction wins"
    );
    let login = |t: &str| new.webmail_user_for(Some(t));
    assert_eq!(
        login("moved@gmail.example.test").as_deref(),
        Some("moved@gmail.example.test")
    );
    assert_eq!(
        login("own@gmail.example.test").as_deref(),
        Some("own@gmail.example.test")
    );
    assert_eq!(new.mailboxes.len(), 4);
    assert!(new.mailboxes[2..].iter().all(|m| !m.ready()));
    assert!(plan.text.contains("password = \"\"\n"));
    assert_eq!(
        (plan.routed, plan.skipped.len(), plan.moved, plan.created),
        (4, 2, 1, 2)
    );
    assert_eq!(
        plan.skipped,
        ["no…@…", "row 8"],
        "skipped rows are named, masked"
    );
    assert_eq!(
        plan.summary(),
        "4 mailboxes; 2 need a password: mo…@…, ow…@…"
    );

    let again = plan_import(CSV, Some(&plan.text), None).expect("replanned");
    assert!(!again.changed, "a second import changes nothing");

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let (csv, toml) = (
        dir.path().join("accounts.csv"),
        dir.path().join("ai-mailbox.toml"),
    );
    std::fs::write(&csv, CSV).expect("csv");
    std::fs::write(&toml, OLD_TOML).expect("toml");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_import(&csv, &toml, true, None, &mut out, &mut err),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        std::fs::read_to_string(&toml).expect("read"),
        OLD_TOML,
        "--dry-run writes nothing"
    );
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_import(&csv, &toml, false, None, &mut out, &mut err),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(std::fs::read_to_string(&toml).expect("read"), plan.text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&toml).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let printed = String::from_utf8(out).expect("utf8") + &String::from_utf8(err).expect("utf8");
    assert!(
        printed.starts_with("4 mailboxes; 2 need a password: mo…@…, ow…@…\n"),
        "{printed}"
    );
    assert!(
        !printed.contains("example.test"),
        "no address is printed: {printed}"
    );
    assert!(
        !printed.contains("keep-pw") && !printed.contains("p@ss"),
        "{printed}"
    );

    let conflict = format!("{CSV}m30,ai1@proton.example.test,claude,keep@example.test,,\n");
    let why = plan_import(&conflict, Some(OLD_TOML), None).expect_err("two mailboxes");
    assert!(
        why.contains("ai…@…") && !why.contains("example.test"),
        "{why}"
    );
    let nested = plan_import(
        CSV,
        Some("[ai.mailbox]\n[[ai.mailbox.mailboxes]]\nlogin = \"x@example.test\"\n"),
        None,
    );
    assert!(
        nested.is_err(),
        "mailboxes kept under another table are refused"
    );
    let fresh = plan_import(CSV, None, None).expect("a new file");
    assert_eq!(config(&fresh.text).mailboxes.len(), 4);
}

/// MASKED OUTPUT: a mailbox login is printed as at most two characters and `…@…`, never its
/// domain; a route's `Debug` names no secret or address; the ladder names the row's mailbox by
/// its masked login (`mailbox(ab…@…)`, or `has no password yet`). NEGATIVE CONTROL:
/// `mask_login` returning the login itself fails this.
#[test]
fn every_mailbox_login_is_printed_masked() {
    use super::super::sign_in_ladder::{Mailbox, Profile, Read};
    assert_eq!(mask_login("abcdef@example.test"), "ab…@…");
    assert_eq!(mask_login(" a@example.test "), "a…@…");
    assert_eq!(mask_login("plainlogin"), "pl…");
    assert_eq!(mask_login(""), "(no login)");
    let route = MailboxRoute {
        login: "secretlogin@example.test".to_owned(),
        password: Some("hunter2".to_owned()),
        receives: vec!["ai@example.test".to_owned()],
        ..MailboxRoute::default()
    };
    let debug = format!("{route:?}");
    assert!(
        !debug.contains("hunter2") && !debug.contains("example.test"),
        "{debug}"
    );
    assert!(debug.contains("se…@…"), "{debug}");
    let cfg = config(
        "[[mailboxes]]\nlogin = \"owner@example.test\"\npassword = \"pw\"\n\
         webmail_url = \"https://mail.example.test/inbox\"\nreceives = [\"*\"]\n",
    );
    assert_eq!(
        cfg.masked_login_for(Some("ai@example.test")).as_deref(),
        Some("ow…@…")
    );
    let read = |mailbox, login: Option<&str>| Read {
        profile: Profile::None,
        mailbox,
        mailbox_login: login.map(str::to_owned),
        vault: None,
        browser: false,
        row_browser: false,
        hosts: 0,
    };
    let words = read(Mailbox::Ready, Some("ow…@…")).words();
    assert!(words.contains("mailbox(ow…@…) "), "{words}");
    let words = read(Mailbox::WebmailOnly, Some("ow…@…")).words();
    assert!(words.contains("mailbox(ow…@…, webmail only) "), "{words}");
    let waiting = read(Mailbox::NoPassword, Some("wa…@…"));
    assert!(
        waiting
            .words()
            .contains("mailbox(wa…@… has no password yet) ")
    );
    assert_eq!(
        waiting.route(),
        None,
        "a mailbox with no password is no route"
    );
    assert!(
        read(Mailbox::None, None)
            .words()
            .contains("mailbox(none configured) ")
    );
}

/// A mailbox shared by several addresses (or a wildcard) is a catch-all, kept with the stewards;
/// one that receives a single address is that account's own, and one with no password holds no
/// login yet. NEGATIVE CONTROL: dropping `route.ready() &&` from `has_catch_all_login` counts the
/// password-less wildcard as a catch-all, and this fails.
#[cfg(unix)]
#[test]
fn a_shared_mailbox_is_a_catch_all_and_a_private_one_is_not() {
    use super::super::ai_signin::has_catch_all_login;
    let private = config(
        "[[mailboxes]]\nlogin = \"g@example.test\"\npassword = \"pw\"\nreceives = [\"g@example.test\"]\n",
    );
    assert!(private.mailboxes[0].private_to("G@example.test"));
    assert!(!has_catch_all_login(&private));
    let shared = config(
        "[[mailboxes]]\nlogin = \"o@example.test\"\npassword = \"pw\"\n\
         receives = [\"a@example.test\", \"b@example.test\"]\n",
    );
    assert!(has_catch_all_login(&shared));
    let waiting = config("[[mailboxes]]\nlogin = \"o@example.test\"\nreceives = [\"*\"]\n");
    assert!(
        !has_catch_all_login(&waiting),
        "a mailbox with no password holds no login yet"
    );
}

/// `accounts mailboxes import` needs its CSV and `--into`, takes `--dry-run`, and refuses
/// `--json` and `--cli` (one mailbox file serves every pool); `--into` belongs to it alone.
/// NEGATIVE CONTROL: parsing the verb without its `!json` guard accepts `--json`, and this
/// fails.
#[test]
fn the_import_verb_needs_its_files() {
    use super::super::cli::{Cmd, parse};
    let args = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        parse(&args(
            "accounts mailboxes import a.csv --into m.toml --dry-run"
        ))
        .expect("parses")
        .0,
        Cmd::MailboxesImport {
            csv: "a.csv".into(),
            into: "m.toml".into(),
            dry_run: true,
        }
    );
    assert_eq!(
        parse(&args("accounts mailboxes import a.csv --into m.toml"))
            .expect("parses")
            .0,
        Cmd::MailboxesImport {
            csv: "a.csv".into(),
            into: "m.toml".into(),
            dry_run: false,
        }
    );
    for refused in [
        "accounts mailboxes import a.csv",
        "accounts mailboxes import --into m.toml",
        "accounts mailboxes import a.csv --into m.toml --json",
        "accounts mailboxes import a.csv --into m.toml --cli codex",
        "accounts mailboxes list --into m.toml",
        "accounts --into m.toml",
    ] {
        assert!(parse(&args(refused)).is_err(), "{refused}");
    }
}

/// The stand-in webmail pages a poll reads in turn (the last one to every read after it): a
/// loading page with no sign-in form and no message list, the sign-in form submitted, then
/// `after` on every read.
fn webmail_pages(after: aterm_json::Value) -> impl FnMut(&str) -> aterm_json::Value + Send {
    let pages = [
        aterm_json::json!({"state": "inbox", "url": "https://mail.example.test/u/0/inbox", "rows": 0}),
        aterm_json::json!({"state": "submitted_login", "url": "https://account.example.test/login"}),
        after,
    ];
    let mut reads = 0usize;
    move |_| {
        let page = pages[reads.min(pages.len() - 1)].clone();
        reads += 1;
        page
    }
}

/// One webmail poll of `cfg` for `target` against a stand-in browser serving `serve`; whether
/// the poll opened a page at all.
fn poll_webmail(
    cfg: &MailboxConfig,
    target: Option<&str>,
    serve: impl FnMut(&str) -> aterm_json::Value + Send + 'static,
) -> (Result<String, &'static str>, bool) {
    use crate::harness::headless_oauth::vendor_tests::fake_cdp_serving;
    let opened = Arc::new(AtomicUsize::new(0));
    let reads = Arc::clone(&opened);
    let mut serve = serve;
    let (mut cdp, browser) = fake_cdp_serving(false, move |expr| {
        reads.fetch_add(1, Ordering::SeqCst);
        serve(expr)
    });
    let got = crate::harness::headless_oauth::poll_webmail_via_cdp(
        &mut cdp,
        cfg,
        target,
        HANG,
        Duration::ZERO,
        &mut std::collections::BTreeSet::new(),
        &mut std::collections::BTreeSet::new(),
    );
    drop(cdp);
    browser.join().expect("stand-in browser");
    (
        got.map(|_| "a credential".to_owned()),
        opened.load(Ordering::SeqCst) > 0,
    )
}

/// A WEBMAIL PASSWORD COUNTS AS SIGNED IN ONLY ON A POSITIVE SIGN: a first page that merely
/// shows no sign-in form (Proton's shell before its form renders) is no sign, so a password
/// submitted that never reaches the inbox's message list on the configured host is recorded as
/// refused, keyed by that host, and the next sign-in does not open the page; a refusal recorded
/// over IMAP (a local Bridge refusing the account password) holds IMAP alone, never the
/// webmail; and only the message list on the inbox's host, after a submitted password, clears a
/// refusal recorded there. NEGATIVE CONTROL: counting any `inbox` tick as signed in (the
/// branch's first form) records nothing for the refused password, and this fails; keying the
/// record by the login alone lets the IMAP refusal hold the webmail, and this fails.
#[test]
fn a_webmail_login_counts_as_signed_in_only_on_the_inbox_list() {
    let store = aterm_tempfile::tempdir().expect("tempdir");
    let record = store.path().join(LOGIN_RECORD_FILE);
    let login = "owner@proton.example.test";
    let mut cfg = config(&format!(
        "[[mailboxes]]\nlogin = \"{login}\"\npassword = \"stale\"\n\
         webmail_url = \"https://mail.example.test/u/0/inbox\"\n\
         receives = [\"*@pm.example.test\"]\n"
    ));
    cfg.login_record = Some(record.clone());
    let target = Some("ai@pm.example.test");
    let webmail = login_source("https://mail.example.test/u/0/inbox");
    let now = unix_now();

    note_login_refused(Some(&record), "imap://127.0.0.1:1143", login, now);
    let (got, opened) = poll_webmail(
        &cfg,
        target,
        webmail_pages(
            aterm_json::json!({"state": "loading", "url": "https://account.example.test/login"}),
        ),
    );
    assert!(opened, "an IMAP refusal does not hold the webmail");
    assert_eq!(
        got,
        Err("timed out waiting for verification email in webmail inbox")
    );
    let until = login_held(Some(&record), &webmail, login, now).expect("the refusal is recorded");
    assert!(until >= now + 890, "{until} vs {now}");

    let (got, opened) = poll_webmail(&cfg, target, |_| aterm_json::json!({"state": "loading"}));
    assert!(!opened, "the next sign-in opens no page");
    assert_eq!(got, Err(refused_words(login)));

    // A refusal whose wait ended: the inbox's message list after the password clears it.
    std::fs::remove_file(&record).expect("forget");
    note_login_refused(Some(&record), &webmail, login, now - 7 * 3600);
    let listed = aterm_json::json!({
        "state": "inbox", "url": "https://mail.example.test/u/0/inbox", "rows": 3
    });
    let (_, opened) = poll_webmail(&cfg, target, webmail_pages(listed));
    assert!(opened);
    assert!(
        std::fs::read_to_string(&record)
            .expect("record")
            .trim()
            .is_empty(),
        "signed in: the refusal is forgotten"
    );
    // A message list on another host (the sign-in page's) is no sign.
    let elsewhere = aterm_json::json!({
        "state": "inbox", "url": "https://account.example.test/help", "rows": 4
    });
    let (_, opened) = poll_webmail(&cfg, target, webmail_pages(elsewhere));
    assert!(opened);
    assert!(login_held(Some(&record), &webmail, login, now).is_some());
}

/// A ROUTED ADDRESS KEEPS THE PASSWORD THAT WORKS: importing the CSV into a single-mailbox file
/// (a top-level `user` with its password, second password and TOTP secret, and a per-email
/// password for a Gmail address) adds entries that use those very credentials, so every alias
/// and the Gmail address still read their codes with them; the new entries carry no empty
/// `password`, and the summary says they keep the password already set. A per-email password
/// kept only under the row's label (`~/.secrets/<label>_password`) reaches the address's own
/// entry once the label is aliased to it. NEGATIVE CONTROL: a `completed_route` that takes
/// nothing from the top level leaves the aliases with no password, and this fails.
#[test]
fn an_imported_entry_keeps_the_password_its_login_already_has() {
    let single = "\
user = \"owner@proton.example.test\"
password = \"owner-pw\"
mailbox_password = \"owner-second\"
totp_secret = \"JBSWY3DPEHPK3PXP\"
webmail_url = \"https://mail.proton.example.test/u/0/inbox\"

[passwords]
\"g1@gmail.example.test\" = \"g1-app-pw\"
";
    let csv = "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)\n\
m3,ai1@pm.example.test,claude,owner@proton.example.test,proton alias,\n\
m3,ai2@pm.example.test,codex,owner@proton.example.test,proton alias,ok\n\
m22,g1@gmail.example.test,claude,itself (gmail),gmail,\n\
m22,g2@gmail.example.test,claude,its own inbox,gmail,\n";
    let before = config(single);
    let plan = plan_import(csv, Some(single), None).expect("planned");
    let after = config(&plan.text);
    assert_eq!(after.mailboxes.len(), 3);
    for alias in ["ai1@pm.example.test", "ai2@pm.example.test"] {
        let t = Some(alias);
        assert_eq!(
            after.webmail_user_for(t).as_deref(),
            Some("owner@proton.example.test")
        );
        assert_eq!(after.password_for(t), before.password_for(t), "{alias}");
        assert_eq!(
            after.mailbox_password_for(t).as_deref(),
            Some("owner-second")
        );
        assert_eq!(
            after.totp_secret_for(t).as_deref(),
            Some("JBSWY3DPEHPK3PXP")
        );
        assert_eq!(
            after.effective_webmail_url(t).as_deref(),
            Some("https://mail.proton.example.test/u/0/inbox"),
            "the top-level mailbox's own URL"
        );
        assert_eq!(after.unready_words(t), None);
    }
    let gmail = Some("g1@gmail.example.test");
    assert_eq!(
        after.webmail_user_for(gmail).as_deref(),
        Some("g1@gmail.example.test")
    );
    assert_eq!(after.password_for(gmail).as_deref(), Some("g1-app-pw"));
    assert_eq!(
        after.mailbox_password_for(gmail),
        None,
        "no other login's secret"
    );
    assert_eq!(
        plan.text.matches("password = \"\"").count(),
        1,
        "only g2, whose password the file lacks, gets an empty one:\n{}",
        plan.text
    );
    assert_eq!(
        plan.summary(),
        "3 mailboxes; 2 keep the password already set for their login: ow…@…, g1…@…; 1 need a \
         password: g2…@…"
    );

    let g2 = Some("g2@gmail.example.test");
    let mut labelled = config(&plan.text);
    assert_eq!(labelled.password_for(g2), None);
    labelled
        .passwords
        .insert("m22-g2".to_owned(), "g2-from-the-lab".to_owned());
    labelled.alias_label_to_email(Some("m22-g2"), g2);
    assert_eq!(
        labelled.password_for(g2).as_deref(),
        Some("g2-from-the-lab"),
        "the lab's answer, kept under the row's label, is the own entry's password"
    );
}

/// The CSV's free text: a correction that says the guess is right (`ok`, `yes`, `✓`) reads the
/// guess; a cell naming the AI email's own mailbox only as a whole (`itself`, `its own inbox`,
/// `itself (gmail)`) routes it to itself, never one that merely holds such a word (`same as
/// above`, `my own proton`), which is skipped and named masked; an address named outright beats
/// an earlier entry's base address for its `+tag` form. NEGATIVE CONTROL: matching `itself`'s
/// words anywhere in the cell (the branch's first form) routes `same as above` to itself, and
/// this fails.
#[test]
fn the_importer_reads_the_owners_free_text_as_meant() {
    let csv = "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)\n\
m3,a1@pm.example.test,claude,owner@proton.example.test,guess,ok\n\
m3,a2@pm.example.test,claude,owner@proton.example.test,guess,✓\n\
m3,a3@pm.example.test,claude,owner@proton.example.test,guess,Yes.\n\
m3,a4@pm.example.test,claude,owner@proton.example.test,guess,same as above\n\
m3,a5@pm.example.test,claude,my own proton,guess,\n\
m22,g@gmail.example.test,claude,itself (gmail),guess,\n";
    let plan = plan_import(csv, None, None).expect("planned");
    let cfg = config(&plan.text);
    for routed in ["a1", "a2", "a3"] {
        assert_eq!(
            cfg.webmail_user_for(Some(&format!("{routed}@pm.example.test")))
                .as_deref(),
            Some("owner@proton.example.test"),
            "{routed}"
        );
    }
    assert_eq!(
        cfg.webmail_user_for(Some("g@gmail.example.test"))
            .as_deref(),
        Some("g@gmail.example.test")
    );
    assert_eq!(plan.skipped, ["a4…@…", "a5…@…"]);
    let detail = plan.detail();
    assert!(
        detail.ends_with(": a4…@…, a5…@…") && !detail.contains("example.test"),
        "{detail}"
    );
    assert!(cfg.route_for(Some("a4@pm.example.test")).is_none());

    let literal = config(
        "[[mailboxes]]\nlogin = \"q@example.test\"\npassword = \"q\"\nreceives = [\"x@pm.example.test\"]\n\
         [[mailboxes]]\nlogin = \"p@example.test\"\npassword = \"p\"\nreceives = [\"x+t@pm.example.test\"]\n",
    );
    assert_eq!(
        literal
            .webmail_user_for(Some("x+t@pm.example.test"))
            .as_deref(),
        Some("p@example.test"),
        "the address named outright wins over an earlier entry's base address"
    );
    assert_eq!(
        literal
            .webmail_user_for(Some("x+u@pm.example.test"))
            .as_deref(),
        Some("q@example.test")
    );
    let debug = format!("{plan:?}");
    assert!(
        !debug.contains("owner@") && debug.contains("text_bytes"),
        "a plan's Debug never shows the file: {debug}"
    );
}

/// WHICH MAILBOX IS AN ACCOUNT'S OWN: one receiving a single address whose base is its login's
/// base; never one whose single address is read through another login (a shared inbox holding
/// one alias today), and never private to the `+tag` addresses of a base address it names. A
/// password-less own entry is `NoPassword` (the lab may be asked for it), a shared one
/// `SharedNoPassword` (never asked row by row), each named by its masked login. NEGATIVE
/// CONTROL: the branch's `private_to`, which also took the base address, makes the owner's
/// mailbox private to its `+tag` accounts, and this fails; `wants_mailbox` taking a shared
/// entry fails it too.
#[cfg(unix)]
#[test]
fn only_an_accounts_own_mailbox_is_private_and_asked_for() {
    use super::super::sign_in_ladder::{Mailbox, Profile, Read, Vault};
    let base = config(
        "[[mailboxes]]\nlogin = \"owner@proton.example.test\"\npassword = \"pw\"\n\
         receives = [\"owner@proton.example.test\"]\n",
    );
    let route = &base.mailboxes[0];
    assert!(route.own() && !route.shared());
    assert!(route.private_to("owner@proton.example.test"));
    assert!(!route.private_to("owner+a@proton.example.test"));
    let one_alias = config(
        "[[mailboxes]]\nlogin = \"owner@proton.example.test\"\npassword = \"pw\"\n\
         receives = [\"ai1@pm.example.test\"]\n",
    );
    let route = &one_alias.mailboxes[0];
    assert!(!route.own() && route.shared());
    assert!(!route.private_to("ai1@pm.example.test"));
    assert!(super::super::ai_signin::has_catch_all_login(&one_alias));

    let waiting = "[[mailboxes]]\nlogin = \"g@gmail.example.test\"\nreceives = [\"g@gmail.example.test\"]\n\
                   [[mailboxes]]\nlogin = \"owner@proton.example.test\"\npassword = \"\"\n\
                   receives = [\"*@pm.example.test\"]\n";
    let row = |label: &str, email: &str| super::super::accounts::Row {
        label: label.to_owned(),
        email: Some(email.to_owned()),
        ..super::super::accounts::Row::default()
    };
    let offer = |email: &str| {
        super::super::accounts_sync::mailbox_offer(
            false,
            Some(config(waiting)),
            &row("m3-x", email),
        )
    };
    assert_eq!(
        offer("g@gmail.example.test"),
        (Mailbox::NoPassword, Some("g…@…".to_owned()))
    );
    assert_eq!(
        offer("ai@pm.example.test"),
        (Mailbox::SharedNoPassword, Some("ow…@…".to_owned()))
    );
    let read = |mailbox| Read {
        profile: Profile::None,
        mailbox,
        mailbox_login: Some("ow…@…".to_owned()),
        vault: Some(Vault::Unchanged(51)),
        browser: false,
        row_browser: false,
        hosts: 0,
    };
    assert!(read(Mailbox::NoPassword).wants_mailbox());
    assert!(
        !read(Mailbox::SharedNoPassword).wants_mailbox(),
        "a shared mailbox's password is never asked row by row"
    );
    assert!(
        read(Mailbox::SharedNoPassword)
            .words()
            .contains("mailbox(ow…@… has no password yet) ")
    );
}

/// TWO MAILBOX FILES WITH ONE LOGIN: the lower file's entry keeps none of its secrets, but its
/// addresses join the kept entry (each one no entry of the higher file names), so none falls
/// back to the top-level mailbox. NEGATIVE CONTROL: dropping the lower entry whole (the
/// branch's first form) leaves `b` on the top-level login, and this fails.
#[test]
fn a_lower_files_entry_for_the_same_login_keeps_its_addresses() {
    let mut higher = config(
        "user = \"legacy@example.test\"\n[[mailboxes]]\nlogin = \"owner@example.test\"\n\
         password = \"new-pw\"\nreceives = [\"a@pm.example.test\"]\n\
         [[mailboxes]]\nlogin = \"g@example.test\"\npassword = \"g-pw\"\nreceives = [\"c@pm.example.test\"]\n",
    );
    let lower = config(
        "[[mailboxes]]\nlogin = \"OWNER@example.test\"\npassword = \"old-pw\"\n\
         receives = [\"a@pm.example.test\", \"b@pm.example.test\", \"c@pm.example.test\"]\n",
    );
    higher.merge_from(lower);
    assert_eq!(higher.mailboxes.len(), 2);
    assert_eq!(
        higher
            .webmail_user_for(Some("b@pm.example.test"))
            .as_deref(),
        Some("owner@example.test")
    );
    assert_eq!(
        higher.password_for(Some("b@pm.example.test")).as_deref(),
        Some("new-pw"),
        "the higher file's secrets"
    );
    assert_eq!(
        higher
            .webmail_user_for(Some("c@pm.example.test"))
            .as_deref(),
        Some("g@example.test"),
        "an address the higher file routes stays where it routes it"
    );
}

/// AN ENVIRONMENT-NAMED MAILBOX OVERRIDES THE ENTRIES: a login, password, TOTP secret or token
/// set in the environment, as much as a URL, clears the files' `[[mailboxes]]`, so the
/// environment's login is the one tried for every address. NEGATIVE CONTROL: the branch's
/// check of the URL variables alone keeps the entry, and this fails.
#[test]
fn an_environment_mailbox_login_overrides_the_entries() {
    use crate::harness::headless_oauth::{IMAP_USER_ENV, discover_mailbox_config_with_env};
    let home = aterm_tempfile::tempdir().expect("home");
    let secrets = home.path().join(".secrets");
    std::fs::create_dir_all(&secrets).expect("secrets");
    std::fs::write(
        secrets.join("ai-mailbox.toml"),
        "imap_url = \"imaps://imap.example.test/INBOX\"\npassword = \"top\"\n\
         [[mailboxes]]\nlogin = \"entry@example.test\"\npassword = \"entry-pw\"\n\
         receives = [\"ai@pm.example.test\"]\n",
    )
    .expect("toml");
    let target = Some("ai@pm.example.test");
    let files = discover_mailbox_config_with_env(Some(home.path()), None, |_| None).expect("files");
    assert_eq!(
        files.webmail_user_for(target).as_deref(),
        Some("entry@example.test")
    );
    let env = discover_mailbox_config_with_env(Some(home.path()), None, |name| {
        (name == IMAP_USER_ENV).then(|| "env-login@example.test".into())
    })
    .expect("env");
    assert!(env.mailboxes.is_empty());
    assert_eq!(
        env.webmail_user_for(target).as_deref(),
        Some("env-login@example.test")
    );
}

/// THE ROUTED HTTP VIEW: an address's entry reads its own HTTP mailbox with its own login
/// (Basic), never the top-level bearer token; an entry that names no HTTP URL leaves the
/// steward no HTTP backend for its addresses (`http_url_for`), even where the top level has one;
/// and the entry that is the top-level mailbox (its login the top-level `user`, no URL of its
/// own) reads the top-level URL with the top-level token. NEGATIVE CONTROL: a routed view that
/// keeps the top-level bearer token (`routed_for`) sends `Bearer` for the routed address, and
/// this fails.
#[test]
fn a_routed_address_reads_its_own_http_mailbox_with_its_own_login() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind http");
    let port = listener.local_addr().expect("addr").port();
    let headers = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&headers);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 2 {
                if line.to_ascii_lowercase().starts_with("authorization:") {
                    seen.lock().expect("lock").push(line.trim().to_owned());
                }
                line.clear();
            }
            let _ = writer.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]",
            );
        }
    });
    let cfg = config(&format!(
        "user = \"top@example.test\"\nhttp_url = \"http://127.0.0.1:{port}/top\"\n\
         bearer_token = \"top-token\"\npassword = \"top-pw\"\n\
         [[mailboxes]]\nlogin = \"own@example.test\"\npassword = \"own-pw\"\n\
         http_url = \"http://127.0.0.1:{port}/own\"\nreceives = [\"ai@pm.example.test\"]\n\
         [[mailboxes]]\nlogin = \"web@example.test\"\npassword = \"web-pw\"\n\
         webmail_url = \"https://mail.example.test/inbox\"\nreceives = [\"w@pm.example.test\"]\n\
         [[mailboxes]]\nlogin = \"TOP@example.test\"\nreceives = [\"t@pm.example.test\"]\n"
    ));
    assert_eq!(cfg.bearer_token.as_deref(), Some("top-token"));
    let routed = cfg.routed_for(Some("ai@pm.example.test"));
    assert_eq!(routed.bearer_token, None);
    assert_eq!(
        routed.http_url.as_deref(),
        Some(format!("http://127.0.0.1:{port}/own").as_str())
    );
    assert_eq!(cfg.http_url_for(Some("w@pm.example.test")), None);
    let top = cfg.routed_for(Some("t@pm.example.test"));
    assert_eq!(top.bearer_token.as_deref(), Some("top-token"));
    assert_eq!(top.password.as_deref(), Some("top-pw"));

    let _ = poll_mailbox_config_for_credential(
        &cfg,
        Some("ai@pm.example.test"),
        Duration::from_millis(300),
    );
    let sent = headers.lock().expect("lock").clone();
    let basic = format!(
        "Basic {}",
        super::super::open_url::encode_base64(b"own@example.test:own-pw")
    );
    assert!(!sent.is_empty(), "the own mailbox was read");
    assert!(
        sent.iter()
            .all(|h| h.ends_with(&basic) && !h.contains("Bearer")),
        "{sent:?}"
    );
}
