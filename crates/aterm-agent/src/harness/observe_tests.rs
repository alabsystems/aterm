// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code observer's tests (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1): one
//! fixture home per selection the profile reader makes, an empty environment, the token route
//! and a signed-out file; and the journal notes a tab's sightings make.

use super::*;
use crate::harness::accounts::{ProcessLogin, Roster};
use aterm_tempfile::TempDir;
use std::fs;

const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";
const UUID: &str = "11111111-2222-3333-4444-555555555555";

fn home(tag: &str) -> TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-observe-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn profile(email: &str) -> String {
    format!(r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"{UUID}"}}}}"#)
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn env(pairs: &[(&str, &str)]) -> Vec<String> {
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect()
}

fn seen(observation: &Observation) -> &Observed {
    observation
        .seen()
        .unwrap_or_else(|| panic!("an observed login: {observation:?}"))
}

/// The default home's `~/.claude.json` is what a process with no home variables reads: the live
/// bug's tab, which runs on unicorn while the roster believes its default row.
/// NEGATIVE CONTROL: with `claude` reading `login_record(user_home, None)` of another home, the
/// address differs: `` left: "brontosaurus…" right: "unicorn…" ``.
#[test]
fn claude_default_home_reads_dot_claude_json() {
    let home = home("default");
    write(&home.path().join(".claude.json"), &profile(UNICORN));
    let env = env(&[("HOME", home.path().to_str().unwrap())]);
    let got = claude(&env, home.path(), AuthRoute::Stored, None);
    let got = seen(&got);
    assert_eq!(got.email, UNICORN);
    assert_eq!(got.key, AccountKey::claude(UUID));
    assert_eq!(got.src, ObservedSrc::Profile);
    assert_eq!(got.path, home.path().join(".claude.json"));
    assert!(got.mtime.is_some());
}

/// `CLAUDE_CONFIG_DIR` moves the profile into that directory: the default home's file is not
/// what the process runs on.
/// NEGATIVE CONTROL: with `login_record_of` reading `effective_of(&[], …)`, the default home's
/// bronto is read: `` left: "brontosaurus…" right: "unicorn…" ``.
#[test]
fn claude_config_dir_wins() {
    let home = home("config");
    write(&home.path().join(".claude.json"), &profile(BRONTO));
    let dir = home.path().join("logins/u");
    write(&dir.join(".claude.json"), &profile(UNICORN));
    let env = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("CLAUDE_CONFIG_DIR", dir.to_str().unwrap()),
    ]);
    let got = claude(&env, home.path(), AuthRoute::Stored, None);
    assert_eq!(seen(&got).email, UNICORN);
    assert_eq!(seen(&got).path, dir.join(".claude.json"));
}

/// A non-empty legacy `.config.json` in the records directory is what Claude Code reads.
/// NEGATIVE CONTROL: with `effective` ignoring the legacy file, the plain profile is read:
/// `` left: "brontosaurus…" right: "unicorn…" ``.
#[test]
fn claude_legacy_config_json() {
    let home = home("legacy");
    write(&home.path().join(".claude.json"), &profile(BRONTO));
    write(&home.path().join(".claude/.config.json"), &profile(UNICORN));
    let env = env(&[("HOME", home.path().to_str().unwrap())]);
    let got = claude(&env, home.path(), AuthRoute::Stored, None);
    assert_eq!(seen(&got).email, UNICORN);
    assert_eq!(seen(&got).path, home.path().join(".claude/.config.json"));
}

/// `CLAUDE_CODE_CUSTOM_OAUTH_URL` selects `.claude-custom-oauth.json`.
/// NEGATIVE CONTROL: with the selector dropped from `effective_of`, the plain file is read:
/// `` left: "brontosaurus…" right: "unicorn…" ``.
#[test]
fn claude_custom_oauth_selected() {
    let home = home("custom");
    write(&home.path().join(".claude.json"), &profile(BRONTO));
    write(
        &home.path().join(".claude-custom-oauth.json"),
        &profile(UNICORN),
    );
    let env = env(&[
        ("HOME", home.path().to_str().unwrap()),
        (
            "CLAUDE_CODE_CUSTOM_OAUTH_URL",
            "https://login.example/oauth",
        ),
    ]);
    let got = claude(&env, home.path(), AuthRoute::Stored, None);
    assert_eq!(seen(&got).email, UNICORN);
}

/// On a token route the profile may be stale: the account is the one whose entry the marker
/// names, by its roster row, whatever the profile says; a marker no row names is unread, and an
/// environment login is never observed.
/// NEGATIVE CONTROL: with the token arm reading the profile, unicorn is read:
/// `` left: "unicorn…" right: "brontosaurus…" ``.
#[test]
fn token_route_ignores_profile() {
    fn keyer(_realm: &str, email: &str) -> Option<String> {
        match email {
            BRONTO => Some("ac-00000000000000b2".to_owned()),
            UNICORN => Some("ac-00000000000000a1".to_owned()),
            _ => None,
        }
    }
    let home = home("token");
    write(&home.path().join(".claude.json"), &profile(UNICORN));
    let roster = Roster::parse_for(
        Cli::Claude,
        &format!("[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"),
    )
    .unwrap();
    let env = env(&[("HOME", home.path().to_str().unwrap())]);
    crate::harness::journal::with_keyer(keyer, || {
        let entry = TokenEntry::of_row(roster.row("m22").unwrap()).unwrap();
        let got = claude(
            &env,
            home.path(),
            AuthRoute::VaultToken { entry },
            Some(&roster),
        );
        assert_eq!(seen(&got).email, BRONTO);
        assert_eq!(seen(&got).src, ObservedSrc::VaultToken);
        let stray =
            TokenEntry::of_key(crate::harness::journal::Key::new("ac-00000000000000ff").unwrap());
        let got = claude(
            &env,
            home.path(),
            AuthRoute::VaultToken {
                entry: stray.unwrap(),
            },
            Some(&roster),
        );
        assert_eq!(got, Observation::Unread, "a marker no row names");
    });
    let got = claude(&env, home.path(), AuthRoute::Outside, None);
    assert_eq!(got, Observation::Unread, "an environment login");
    // The route an exec environment with an API key reads as is the outside one.
    let who = ProcessLogin::of_exec_env(&[
        format!("HOME={}", home.path().display()),
        "ANTHROPIC_API_KEY=sk-test".to_owned(),
    ])
    .unwrap();
    assert_eq!(who.route, AuthRoute::Outside);
}

/// A process whose captured environment carries none of the profile selectors (no `HOME`, no
/// `CLAUDE_CONFIG_DIR`) reads the default home's profile, as `login_record`'s default login does:
/// an empty environment is no reason to observe nothing.
/// NEGATIVE CONTROL: with `login_profile_of` refusing an empty environment, both are unread:
/// `` an observed login: Unread ``.
#[test]
fn an_empty_environment_reads_the_default_home() {
    let home = home("empty-env");
    write(&home.path().join(".claude.json"), &profile(UNICORN));
    let got = claude(&[], home.path(), AuthRoute::Stored, None);
    assert_eq!(seen(&got).email, UNICORN);
    assert_eq!(seen(&got).path, home.path().join(".claude.json"));
    let record = profile::login_record(home.path(), None).expect("the default login's record");
    assert_eq!(record.email, UNICORN);
    assert_eq!(record.key, AccountKey::claude(UUID));
}

/// A profile that names no account is signed out, and a missing one is unread: neither is an
/// account.
/// NEGATIVE CONTROL: with `SignedOut` reported for a missing file too, `` left: SignedOut { … }
/// right: Unread ``.
#[test]
fn signed_out_profile_is_not_an_account() {
    let home = home("signed-out");
    let env = env(&[("HOME", home.path().to_str().unwrap())]);
    assert_eq!(
        claude(&env, home.path(), AuthRoute::Stored, None),
        Observation::Unread
    );
    write(&home.path().join(".claude.json"), r#"{"numStartups":3}"#);
    let got = claude(&env, home.path(), AuthRoute::Stored, None);
    assert_eq!(
        got,
        Observation::SignedOut {
            path: home.path().join(".claude.json")
        }
    );
    assert_eq!(got.email(), None);
}

fn keyer(_realm: &str, email: &str) -> Option<String> {
    match email {
        UNICORN => Some("ac-00000000000000a1".to_owned()),
        BRONTO => Some("ac-00000000000000b2".to_owned()),
        _ => None,
    }
}

fn unicorn_seen() -> Observation {
    Observation::Seen(Observed {
        cli: Cli::Claude,
        email: UNICORN.to_owned(),
        key: None,
        src: ObservedSrc::Profile,
        path: PathBuf::from("/home/someone/.claude.json"),
        mtime: Some(1),
    })
}

fn m22() -> Roster {
    Roster::parse_for(
        Cli::Claude,
        &format!("[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"),
    )
    .unwrap()
}

fn stored() -> ProcessLogin {
    ProcessLogin {
        login: None,
        default_home: true,
        route: AuthRoute::Stored,
    }
}

fn kinds(sighted: &Sighted) -> Vec<&'static str> {
    sighted.notes.iter().map(|n| n.kind).collect()
}

/// THE LIVE BUG's notes: the first pass of the m65 tab writes its sighting of unicorn, one
/// `wrong-account` naming unicorn with `to` brontosaurus and the tab, and a `session-start`
/// keyed by unicorn (no local row records it); later passes in the hour write none of them
/// again, and the registry hears the login once.
/// NEGATIVE CONTROL: with `wrong_once` always true, the second pass writes another
/// `wrong-account`: `` left: ["wrong-account"] right: [] ``.
#[test]
fn account_note_writes_wrong_account_once() {
    crate::harness::journal::with_keyer(keyer, || {
        let roster = m22();
        let tab = Tab::of("machine", "s-41a2");
        let mut kept = Sightings::default();
        let first = sighting(
            &roster,
            &stored(),
            &unicorn_seen(),
            &mut kept,
            tab,
            "claude",
            1_000,
        );
        assert_eq!(kinds(&first), ["seen", "wrong-account", "session-start"]);
        let wrong = first.notes[1];
        assert_eq!(wrong.account.unwrap().as_str(), "ac-00000000000000a1");
        assert_eq!(wrong.to.unwrap().as_str(), "ac-00000000000000b2");
        assert_eq!(wrong.tab, Some(tab));
        assert_eq!(first.notes[0].why, Some("first"));
        let login = first.login.unwrap();
        assert_eq!(login.email, UNICORN);
        assert_eq!(login.state, LoginState::Running);
        let again = sighting(
            &roster,
            &stored(),
            &unicorn_seen(),
            &mut kept,
            tab,
            "claude",
            1_060,
        );
        assert_eq!(kinds(&again), Vec::<&str>::new());
        assert_eq!(again.login, None);
    });
}

/// A running tab's sighting is noted again once an hour, never on every pass, and at once when
/// the tab's account changes.
/// NEGATIVE CONTROL: with `SEEN_REFRESH_S` at 0, the pass a minute later notes: `` left:
/// Some(Refresh) right: None ``.
#[test]
fn seen_refreshes_hourly_not_per_pass() {
    let mut kept = Sightings::default();
    assert_eq!(kept.due(UNICORN, 0), Some(SeenWhy::First));
    assert_eq!(kept.due(UNICORN, 60), None);
    assert_eq!(
        kept.due(&UNICORN.to_ascii_uppercase(), SEEN_REFRESH_S - 1),
        None
    );
    assert_eq!(kept.due(UNICORN, SEEN_REFRESH_S), Some(SeenWhy::Refresh));
    assert_eq!(kept.due(BRONTO, SEEN_REFRESH_S + 1), Some(SeenWhy::Changed));
    assert_eq!(kept.due(BRONTO, SEEN_REFRESH_S + 2), None);
}

/// A tab that agrees with its row writes its sighting and nothing else; an unread one writes
/// nothing.
/// NEGATIVE CONTROL: with `disagrees` true for `Agrees`, a `wrong-account` is written: `` left:
/// ["seen", "wrong-account"] right: ["seen"] ``.
#[test]
fn an_agreeing_tab_writes_its_sighting_alone() {
    crate::harness::journal::with_keyer(keyer, || {
        let roster = m22();
        let tab = Tab::of("machine", "s-41a2");
        let bronto = Observation::Seen(Observed {
            email: BRONTO.to_owned(),
            ..unicorn_seen().seen().unwrap().clone()
        });
        let mut kept = Sightings::default();
        let got = sighting(&roster, &stored(), &bronto, &mut kept, tab, "claude", 1_000);
        assert_eq!(kinds(&got), ["seen"]);
        let mut kept = Sightings::default();
        let got = sighting(
            &roster,
            &stored(),
            &Observation::Unread,
            &mut kept,
            tab,
            "claude",
            1,
        );
        assert_eq!(got, Sighted::default());
    });
}

/// A session journaled on an account no row lists is closed: by a `session-end` keyed by that
/// account when the tab moves to another account, and when the tab ends ([`Sightings::end`]),
/// which also hands the registry its last login as no longer this tab's. A tab that never opened
/// one ends with only that.
/// NEGATIVE CONTROL: with `Sightings::end` leaving `open` alone, the ended tab writes no
/// `session-end`: `` left: [] right: ["session-end"] ``.
#[test]
fn an_unlisted_session_ends_when_the_tab_moves_or_ends() {
    crate::harness::journal::with_keyer(keyer, || {
        let roster = m22();
        let tab = Tab::of("machine", "s-41a2");
        let bronto = Observation::Seen(Observed {
            email: BRONTO.to_owned(),
            ..unicorn_seen().seen().unwrap().clone()
        });
        let mut kept = Sightings::default();
        let first = sighting(
            &roster,
            &stored(),
            &unicorn_seen(),
            &mut kept,
            tab,
            "claude",
            10,
        );
        assert_eq!(kinds(&first), ["seen", "wrong-account", "session-start"]);
        let moved = sighting(&roster, &stored(), &bronto, &mut kept, tab, "claude", 20);
        assert_eq!(kinds(&moved), ["seen", "session-end"]);
        let end = moved.notes[1];
        assert_eq!(end.account.unwrap().as_str(), "ac-00000000000000a1");
        assert_eq!((end.tab, end.t), (Some(tab), 20));
        let ended = kept.end(30);
        assert_eq!(
            kinds(&ended),
            Vec::<&str>::new(),
            "bronto's row notes its own end"
        );
        let login = ended.login.unwrap();
        assert_eq!(
            (login.email.as_str(), login.state, login.at),
            (BRONTO, LoginState::Idle, 30)
        );
        let mut kept = Sightings::default();
        let _ = sighting(
            &roster,
            &stored(),
            &unicorn_seen(),
            &mut kept,
            tab,
            "claude",
            10,
        );
        let ended = kept.end(40);
        assert_eq!(kinds(&ended), ["session-end"]);
        assert_eq!(ended.login.unwrap().state, LoginState::Idle);
        assert_eq!(kept, Sightings::default(), "everything kept is forgotten");
    });
}
