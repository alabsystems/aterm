// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stopped token step's record (FL-136): what holds it, what it keeps, and its file.

use super::*;
use crate::harness::accounts_tokens::tests::{KEY, Scratch};
use crate::harness::journal;

const AT: u64 = 1_800_000_000;

fn stopped(word: MetWord) -> Stopped {
    Stopped::new(word, AT, "2.1.318", "0.199.0+g69ee51e2f4b5").expect("a record")
}

/// Only an event ends a hold: a new Claude Code or aterm build where both versions are known,
/// a window store or mailbox configuration written after the stop, a token made after it, and,
/// for a code page that waited on a mailbox alone, the window reading the account's mailbox now.
/// No time does, and nothing unknown does; a mailbox read now lets no other stop through.
/// NEGATIVE CONTROL: with `moved` counting an unknown version as a change (`then != now` alone),
/// a build nobody knew ends the hold: `` an unknown build ended the hold ``. With `holds`
/// reading `mailbox_read` for every word, a mailbox released a human check: `` a mailbox the
/// window reads ended a human check's hold ``.
#[test]
fn only_an_event_ends_a_hold() {
    let step = stopped(MetWord::WindowHumanCheck);
    assert!(step.holds(&Since::default()), "nothing changed");
    let same = Since {
        cli: Some("2.1.318"),
        aterm: Some("0.199.0+g69ee51e2f4b5"),
        route_at: Some(AT),
        token_at: Some(AT),
        mailbox_read: None,
    };
    assert!(step.holds(&same), "the stop's own second is no event");
    assert!(
        step.holds(&Since {
            mailbox_read: Some(true),
            ..same
        }),
        "a mailbox the window reads ended a human check's hold"
    );
    let no_mailbox = stopped(MetWord::WindowCodeNoMailbox);
    assert!(no_mailbox.waits_on_a_mailbox() && !step.waits_on_a_mailbox());
    for read in [None, Some(false)] {
        assert!(
            no_mailbox.holds(&Since {
                mailbox_read: read,
                ..same
            }),
            "a mailbox the window does not read ({read:?}) ended the hold"
        );
    }
    assert!(
        !no_mailbox.holds(&Since {
            mailbox_read: Some(true),
            ..same
        }),
        "a mailbox the window reads now did not end a code page's hold"
    );
    for (since, what) in [
        (
            Since {
                cli: Some("2.1.319"),
                ..same
            },
            "a new Claude Code build",
        ),
        (
            Since {
                aterm: Some("0.200.0+g0123456789ab"),
                ..same
            },
            "a new aterm build",
        ),
        (
            Since {
                route_at: Some(AT + 1),
                ..same
            },
            "a window store or mailbox written after the stop",
        ),
        (
            Since {
                token_at: Some(AT + 1),
                ..same
            },
            "a token made after the stop",
        ),
    ] {
        assert!(!step.holds(&since), "{what} did not end the hold");
    }
    let unknown = Since {
        cli: Some(""),
        aterm: None,
        ..Since::default()
    };
    assert!(step.holds(&unknown), "an unknown build ended the hold");
    let unknown_then = Stopped::new(MetWord::WindowHumanCheck, AT, "", "").unwrap();
    assert!(unknown_then.holds(&same), "an unknown build ended the hold");
}

/// A record is made only of a word that holds a step, and of builds a record keeps: each stop
/// word journals `token-stopped` with one of the journal's reason words, which the lab's journal
/// takes, and the record's words name no host.
/// NEGATIVE CONTROL: with `Stopped::new` dropping its word test, a page that did not load
/// leaves a record: `` BrowserSignIn made a record ``.
#[test]
fn a_record_is_only_of_a_stop_and_of_its_builds() {
    for word in [
        MetWord::BrowserStart,
        MetWord::BrowserControl,
        MetWord::BrowserSignIn,
        MetWord::Stall,
        MetWord::NotAuthorized,
    ] {
        assert!(
            Stopped::new(word, AT, "2.1.318", "").is_none(),
            "{word:?} made a record"
        );
    }
    // The lab's journal takes each stop's note: its kind and its reason.
    assert!(aterm_db::alab::journal::Kind::parse(journal::TokenEvent::Stopped.kind()).is_some());
    for word in [
        MetWord::BotCheck,
        MetWord::BlockPage,
        MetWord::HumanCheck,
        MetWord::ConsentUnproven,
        MetWord::ConsentWrongAccount,
        MetWord::WindowNoDesktop,
        MetWord::WindowBotCheck,
        MetWord::WindowBlockPage,
        MetWord::WindowHumanCheck,
        MetWord::WindowClosed,
        MetWord::WindowWrongAccount,
        MetWord::WindowCodeNoMailbox,
        MetWord::WindowDeadline,
    ] {
        let step = stopped(word);
        assert!(
            journal::WHYS.contains(&step.why()),
            "{word:?}: {}",
            step.why()
        );
        assert!(
            aterm_db::alab::journal::Word::<16>::new(step.why()).is_some(),
            "{word:?}"
        );
        assert_ne!(step.why(), "failed", "{word:?}");
        let words = step.words(Some("aterm harness accounts token work"));
        assert!(words.starts_with("stopped 2027-01-15: "), "{words}");
        assert!(
            words.ends_with(", or by `aterm harness accounts token work`"),
            "{words}"
        );
        assert!(!words.contains(" on claude"), "{words}");
    }
    let long = "9".repeat(MAX_BUILD + 1);
    for build in ["2.1.318 x", "a/b", "who@where", long.as_str()] {
        assert!(
            Stopped::new(MetWord::WindowClosed, AT, build, "").is_none(),
            "{build:?}"
        );
        assert!(
            Stopped::new(MetWord::WindowClosed, AT, "", build).is_none(),
            "{build:?}"
        );
    }
}

/// The file holds each account's record under tokens.lock: set, replaced and cleared; a record
/// of a word or key no record holds is left out of a read, and kept as it is by another
/// account's write; a file that does not read is never written over, so no account's hold is
/// dropped by a write.
/// NEGATIVE CONTROL: with `put` starting from no rows where the file does not read (the write
/// it then makes holds this record alone), the unread file is written over: `` a put wrote over
/// a file that does not read ``.
#[test]
fn the_file_holds_each_accounts_record_and_nothing_else() {
    let scratch = Scratch::new("steps-file");
    let store = scratch.store();
    assert!(read(&store).unwrap().is_empty());
    put(&store, KEY, None).unwrap();
    assert!(
        !store.shared_dir().join(STEPS_FILE).exists(),
        "clearing nothing wrote a file"
    );
    let first = stopped(MetWord::WindowBotCheck);
    put(&store, KEY, Some(&first)).unwrap();
    assert_eq!(read(&store).unwrap().get(KEY), Some(&first));
    let other = "ac-0123456789abcdef";
    let second = stopped(MetWord::WindowCodeNoMailbox);
    put(&store, other, Some(&second)).unwrap();
    put(&store, KEY, None).unwrap();
    let steps = read(&store).unwrap();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps.get(other), Some(&second));
    let raw = std::fs::read_to_string(store.shared_dir().join(STEPS_FILE)).unwrap();
    assert!(raw.contains("\"word\":\"window-code-no-mailbox\""), "{raw}");
    // A record no build writes is left out; a file of another schema does not read.
    aterm_uds::private_file::write_private(
        &store.shared_dir(),
        STEPS_FILE,
        format!(
            "{{\"schema\":1,\"steps\":[{{\"key\":\"{KEY}\",\"word\":\"browser-sign-in\",\"at\":1,\
             \"cli\":\"\",\"aterm\":\"\"}},{{\"key\":\"person@example.com\",\"word\":\
             \"window-closed\",\"at\":1,\"cli\":\"\",\"aterm\":\"\"}}]}}"
        )
        .as_bytes(),
    )
    .unwrap();
    assert!(read(&store).unwrap().is_empty());
    // Another account's write keeps both rows this build does not read, as they were.
    put(&store, other, Some(&second)).unwrap();
    let raw = std::fs::read_to_string(store.shared_dir().join(STEPS_FILE)).unwrap();
    assert!(raw.contains("\"word\":\"browser-sign-in\""), "{raw}");
    assert!(raw.contains("\"key\":\"person@example.com\""), "{raw}");
    assert_eq!(read(&store).unwrap().get(other), Some(&second));
    // A file of another schema (a later build's) does not read, and no write goes over it: every
    // step stays held while it does not read.
    let later = b"{\"schema\":2,\"steps\":[]}";
    aterm_uds::private_file::write_private(&store.shared_dir(), STEPS_FILE, later).unwrap();
    assert!(read(&store).is_err());
    for step in [None, Some(&first)] {
        assert!(put(&store, KEY, step).is_err(), "{step:?}");
        assert_eq!(
            std::fs::read(store.shared_dir().join(STEPS_FILE)).unwrap(),
            later,
            "a put wrote over a file that does not read"
        );
    }
}

/// What lets a held step through without a person is dated by its own file: the account's
/// window store's record (on macOS), each mailbox configuration file the window reads, and the
/// account's saved Google mail grant, which lets the window read a Gmail row with no password.
/// A file under `~/.secrets` dates nothing: the window reads none (the mailbox configurations
/// there are vault entries, judged by [`mailbox_read`]).
/// NEGATIVE CONTROL: with `route_at` reading no Google grant, a grant saved after the
/// configuration is not seen: `` a saved Google grant was not seen ``. With `route_at` stating
/// `~/.secrets/ai-mailbox.toml` again, `` a file under ~/.secrets dated a route ``.
#[test]
fn a_route_is_dated_by_its_own_files() {
    let scratch = Scratch::new("steps-route");
    let store = scratch.store();
    store.ensure().unwrap();
    let home = aterm_tempfile::Builder::new()
        .prefix("aterm-token-steps-home-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    assert_eq!(
        route_at(&store, Some(home.path()), "person@example.com"),
        None
    );
    let secrets = home.path().join(".secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    for name in [
        super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
        super::super::headless_oauth::MAILBOX_TOML_FILE,
    ] {
        std::fs::write(secrets.join(name), b"").unwrap();
    }
    assert_eq!(
        route_at(&store, Some(home.path()), "person@example.com"),
        None,
        "a file under ~/.secrets dated a route"
    );
    let config_dir = home.path().join(".config/aterm");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config = config_dir.join(super::super::headless_oauth::MAILBOX_TOML_FILE);
    std::fs::write(&config, b"").unwrap();
    let written = |path: &std::path::Path| {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    assert_eq!(
        route_at(&store, Some(home.path()), "person@example.com"),
        Some(written(&config))
    );
    // The account's saved Google mail grant, where the mail crate keeps it, written later.
    let grant = aterm_mail::oauth::session::grant_file(home.path(), "Person@Example.com ")
        .expect("a Google account's grant file");
    std::fs::create_dir_all(grant.parent().unwrap()).unwrap();
    std::fs::write(&grant, b"").unwrap();
    let later =
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(written(&config) + 60);
    std::fs::File::options()
        .write(true)
        .open(&grant)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert_eq!(
        route_at(&store, Some(home.path()), "person@example.com"),
        Some(written(&grant)),
        "a saved Google grant was not seen"
    );
    assert!(written(&grant) > written(&config));
    #[cfg(target_os = "macos")]
    {
        let marker = super::super::signin_window::store_marker(&store.dir, "person@example.com");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"person@example.com").unwrap();
        assert_eq!(
            route_at(&store, None, "person@example.com"),
            Some(written(&marker))
        );
    }
}
