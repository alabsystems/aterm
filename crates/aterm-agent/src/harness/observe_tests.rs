// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The observers' tests (`docs/DESIGN-account-discovery-2026-11-09.md`, steps 1 to 3). Claude
//! Code's: one fixture home per selection the profile reader makes, an empty environment, the
//! token route and a signed-out file. Codex's, agy's, Muse Code's, Grok Build's and Kimi
//! Code's: one fixture home each, by the file the process's own environment selects, and each
//! case that names no account. And the journal notes a tab's sightings make. Every fixture is made up: no file here holds a
//! credential.

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
        cli: Cli::Claude.into(),
        email: UNICORN.to_owned(),
        key: None,
        region: None,
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
            Some(&roster),
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
            Some(&roster),
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
        let got = sighting(
            Some(&roster),
            &stored(),
            &bronto,
            &mut kept,
            tab,
            "claude",
            1_000,
        );
        assert_eq!(kinds(&got), ["seen"]);
        let mut kept = Sightings::default();
        let got = sighting(
            Some(&roster),
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
            Some(&roster),
            &stored(),
            &unicorn_seen(),
            &mut kept,
            tab,
            "claude",
            10,
        );
        assert_eq!(kinds(&first), ["seen", "wrong-account", "session-start"]);
        let moved = sighting(
            Some(&roster),
            &stored(),
            &bronto,
            &mut kept,
            tab,
            "claude",
            20,
        );
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
            Some(&roster),
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

/// The login places of a Mac are its default home, where no row names it, and each row's
/// directory, each read as a process started there would read it; a CLI with no observer, and a
/// Mac whose home is not known, read nothing.
/// NEGATIVE CONTROL: with `places` leaving out the default home no row names (the row-only loop
/// the holdings report had), the Mac that lists no account has no place: `left: 0 right: 1`.
#[test]
fn places_are_the_default_home_and_each_rows_directory() {
    let home = home("places");
    write(&home.path().join(".claude.json"), &profile(UNICORN));
    let dir = home.path().join("logins").join("m22");
    write(&dir.join(".claude.json"), &profile(BRONTO));
    let row = |dir: Option<&str>| Row {
        label: "m22".into(),
        email: Some(BRONTO.into()),
        dir: dir.map(str::to_owned),
        ..Row::default()
    };
    let none = Roster::default();
    let got = places(Cli::Claude, &none, Some(home.path()));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].row, None);
    assert_eq!(got[0].login.seen.email(), Some(UNICORN));
    assert_eq!(got[0].login.name(&none).as_deref(), Some(UNICORN));

    let rows = Roster {
        rows: vec![row(Some(dir.to_str().unwrap()))],
    };
    let got = places(Cli::Claude, &rows, Some(home.path()));
    let seen: Vec<_> = got
        .iter()
        .map(|place| {
            (
                place.row.map(|row| row.label.as_str()),
                place.login.seen.email(),
            )
        })
        .collect();
    assert_eq!(seen, [(None, Some(UNICORN)), (Some("m22"), Some(BRONTO))]);
    assert!(matches!(
        got[1].login.reconciled(&rows),
        Reconciled::Agrees(row) if row.label == "m22"
    ));

    let default_row = Roster {
        rows: vec![row(None)],
    };
    let got = places(Cli::Claude, &default_row, Some(home.path()));
    assert_eq!(got.len(), 1, "the default home is the row's place");
    assert!(matches!(
        got[0].login.reconciled(&default_row),
        Reconciled::Unlisted(seen) if seen.email == UNICORN
    ));

    for place in places(Cli::Codex, &rows, Some(home.path()))
        .iter()
        .chain(&places(Cli::Claude, &rows, None))
    {
        assert_eq!(place.login.seen, Observation::Unread);
    }
}

/// A tab on a Mac that lists no account is sighted like any other: its `seen`, the login for the
/// registry, and its session journaled under the observed account, since no row records it; a
/// later pass in the hour writes nothing, and the tab's end closes the session. A roster that
/// could not be read still gives the `seen` and the login, and opens no session a row may be
/// recording. The synthetic own row is never a believed account to contradict.
/// NEGATIVE CONTROL: with `sighting` returning nothing for a roster that lists no row (the gate
/// its one caller sat behind), the first pass is empty: `` left: [] right: ["seen",
/// "session-start"] ``.
#[test]
fn a_tab_on_a_mac_that_lists_no_account_is_sighted() {
    crate::harness::journal::with_keyer(keyer, || {
        let tab = Tab::of("machine", "s-41a2");
        let none = Roster::default();
        let mut kept = Sightings::default();
        let pass = |kept: &mut Sightings, roster: Option<&Roster>, now| {
            sighting(roster, &stored(), &unicorn_seen(), kept, tab, "claude", now)
        };
        let first = pass(&mut kept, Some(&none), 10);
        assert_eq!(kinds(&first), ["seen", "session-start"]);
        assert_eq!(
            first.notes[1].account.unwrap().as_str(),
            "ac-00000000000000a1"
        );
        let login = first.login.unwrap();
        assert_eq!(
            (login.email.as_str(), login.state, login.tab),
            (UNICORN, LoginState::Running, Some(tab))
        );
        let again = pass(&mut kept, Some(&none), 70);
        assert_eq!((kinds(&again), again.login), (Vec::new(), None));
        assert_eq!(kinds(&kept.end(90)), ["session-end"]);

        let mut kept = Sightings::default();
        let unread = pass(&mut kept, None, 10);
        assert_eq!(kinds(&unread), ["seen"]);
        assert_eq!(unread.login.unwrap().state, LoginState::Running);
        assert_eq!(kinds(&kept.end(20)), Vec::<&str>::new());

        let mut kept = Sightings::default();
        let own = pass(&mut kept, Some(&Roster::own()), 10);
        assert_eq!(kinds(&own), ["seen"], "no `wrong-account` against `main`");
    });
}
const ACCOUNT_ID: &str = "acct-fixture-0001";
const USER_ID: &str = "user-fixture-0001";

/// An unsigned stand-in for a ChatGPT token: a header, the claims and a signature word, which
/// is all the reader decodes. It authenticates nothing.
fn claims_token(email: &str) -> String {
    let part = |bytes: &[u8]| {
        aterm_codec::base64::encode(bytes)
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_")
    };
    let claims = format!(
        r#"{{"email":"{email}","https://api.openai.com/auth":{{"chatgpt_account_id":"{ACCOUNT_ID}","chatgpt_user_id":"{USER_ID}"}}}}"#
    );
    format!(
        "{}.{}.fixture",
        part(br#"{"alg":"none"}"#),
        part(claims.as_bytes())
    )
}

/// A Codex `auth.json` whose tokens' claims name `email`.
fn codex_auth(email: &str) -> String {
    let token = claims_token(email);
    format!(
        r#"{{"auth_mode":"chatgpt","tokens":{{"id_token":"{token}","access_token":"{token}"}}}}"#
    )
}

fn process<'a>(env: &'a [String], cwd: Option<&'a Path>) -> Process<'a> {
    Process {
        argv: &[],
        env,
        cwd,
    }
}

fn codex_who(env: &[String]) -> ProcessLogin {
    ProcessLogin::of_env_for(Cli::Codex, env.iter().filter_map(|kv| kv.split_once('=')))
}

/// Codex's login is the one in the `CODEX_HOME` its own exec environment names: the default
/// home's file is another account's. With no `CODEX_HOME` it is `~/.codex`'s.
/// NEGATIVE CONTROL: with `codex` passing `None` for `CODEX_HOME`, the default home's bronto is
/// read under the variable: `` left: "brontosaurus…" right: "unicorn…" ``.
#[test]
fn codex_home_comes_from_the_exec_env() {
    let home = home("codex-home");
    write(&home.path().join(".codex/auth.json"), &codex_auth(BRONTO));
    let other = home.path().join("elsewhere");
    write(&other.join("auth.json"), &codex_auth(UNICORN));
    let moved = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("CODEX_HOME", other.to_str().unwrap()),
    ]);
    let got = codex(&process(&moved, None), home.path(), &codex_who(&moved));
    let got = seen(&got);
    assert_eq!(got.cli, ObservedCli::Pool(Cli::Codex));
    assert_eq!(got.email, UNICORN);
    assert_eq!(got.key, AccountKey::codex(ACCOUNT_ID, USER_ID));
    assert!(got.key.is_some());
    assert_eq!(got.src, ObservedSrc::AuthFile);
    assert_eq!(got.path, other.join("auth.json"));
    assert!(got.mtime.is_some());
    let default = env(&[("HOME", home.path().to_str().unwrap())]);
    let got = codex(&process(&default, None), home.path(), &codex_who(&default));
    assert_eq!(seen(&got).email, BRONTO);
    assert_eq!(seen(&got).path, home.path().join(".codex/auth.json"));
    // A kernel environment with no `HOME` reads the user's own home.
    let got = codex(&process(&[], None), home.path(), &codex_who(&[]));
    assert_eq!(seen(&got).email, BRONTO);
}

/// A pooled Codex tab's login is the file its `CODEX_AUTH_PATH` names, whatever its home's
/// own `auth.json` says; [`pooled`] reaches the same observer.
/// NEGATIVE CONTROL: with `codex` ignoring `who.login`, the home's bronto is read: `` left:
/// "brontosaurus…" right: "unicorn…" ``.
#[test]
fn codex_auth_path_names_the_login_file() {
    let home = home("codex-path");
    write(&home.path().join(".codex/auth.json"), &codex_auth(BRONTO));
    let login = home.path().join("logins/work-2");
    write(&login.join("auth.json"), &codex_auth(UNICORN));
    let env = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("CODEX_AUTH_PATH", login.join("auth.json").to_str().unwrap()),
    ]);
    let who = codex_who(&env);
    let got = pooled(Cli::Codex, &process(&env, None), home.path(), &who, None);
    assert_eq!(seen(&got).email, UNICORN);
    assert_eq!(seen(&got).path, login.join("auth.json"));
}

/// An `auth.json` that names no ChatGPT account (an API-key login) is signed out, a home with
/// no file is unread, and so is a process off its stored login, whatever the file says.
/// NEGATIVE CONTROL: with `codex` reading on an outside route, the file's unicorn is seen: ``
/// left: Seen(…) right: Unread ``.
#[test]
fn codex_without_an_account_is_not_an_account() {
    let home = home("codex-none");
    let env = env(&[("HOME", home.path().to_str().unwrap())]);
    let who = codex_who(&env);
    assert_eq!(
        codex(&process(&env, None), home.path(), &who),
        Observation::Unread
    );
    let path = home.path().join(".codex/auth.json");
    write(&path, r#"{"auth_mode":"apikey"}"#);
    assert_eq!(
        codex(&process(&env, None), home.path(), &who),
        Observation::SignedOut { path: path.clone() }
    );
    write(&path, &codex_auth(UNICORN));
    let outside = ProcessLogin {
        route: AuthRoute::Outside,
        ..who
    };
    assert_eq!(
        codex(&process(&env, None), home.path(), &outside),
        Observation::Unread
    );
}

fn agy_log(email: &str, method: &str) -> String {
    format!("I1109 12:00:00 auth.go:1] applyAuthResult: email={email}, authMethod={method}\n")
}

/// agy's login is the consumer account its CLI log's latest sign-in names, in the
/// configuration directory its own environment selects.
/// NEGATIVE CONTROL: with `agy` reading the first record of the log, the earlier bronto is
/// read: `` left: "brontosaurus…" right: "unicorn…" ``.
#[test]
fn agy_log_names_the_consumer_account() {
    let home = home("agy-log");
    let config = home.path().join("agy-config");
    let log = config.join("cli.log");
    write(
        &log,
        &format!(
            "{}{}",
            agy_log(BRONTO, "consumer"),
            agy_log(UNICORN, "consumer")
        ),
    );
    let env = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("AGY_CLI_CONFIG_DIR", config.to_str().unwrap()),
    ]);
    let got = agy(&process(&env, None), home.path());
    let got = seen(&got);
    assert_eq!(got.cli, ObservedCli::Pool(Cli::Agy));
    assert_eq!(got.email, UNICORN);
    assert_eq!(got.key, None);
    assert_eq!(got.src, ObservedSrc::CliLog);
    assert_eq!(got.path, log);
    // A latest sign-in by another method names no consumer account; no log names none.
    write(&log, &agy_log(UNICORN, "gemini_api_key"));
    assert_eq!(agy(&process(&env, None), home.path()), Observation::Unread);
    std::fs::remove_file(&log).unwrap();
    assert_eq!(agy(&process(&env, None), home.path()), Observation::Unread);
}

/// Behind a gateway agy runs on no account of its own: the observation says the route and
/// names no account, whatever its log's last consumer sign-in said.
/// NEGATIVE CONTROL: with `agy_read` not checking the gateway variable, the log's unicorn is
/// seen: `` left: Seen(…) right: Routed { route: Gateway } ``.
#[test]
fn agy_behind_a_gateway_names_no_account() {
    let home = home("agy-gateway");
    let config = home.path().join("agy-config");
    write(&config.join("cli.log"), &agy_log(UNICORN, "consumer"));
    let env = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("AGY_CLI_CONFIG_DIR", config.to_str().unwrap()),
        ("AGY_LLM_GATEWAY_URL", "http://127.0.0.1:1/v1"),
    ]);
    let got = agy(&process(&env, None), home.path());
    assert_eq!(
        got,
        Observation::Routed {
            route: ObservedRoute::Gateway
        }
    );
    assert_eq!(got.email(), None);
    assert_eq!(ObservedRoute::Gateway.word(), "gateway");
}

fn muse_auth(email: &str, mechanism: &str) -> String {
    format!(r#"{{"providers":{{"meta":{{"mechanism":"{mechanism}","user_email":"{email}"}}}}}}"#)
}

/// Muse Code's login is Meta's OAuth account its `auth.json` names, under `XDG_CONFIG_HOME`
/// where its environment sets one; with `META_API_KEY` set the process runs on no stored login
/// and is unread.
/// NEGATIVE CONTROL: with `muse_login_for` not checking `META_API_KEY`, the file's unicorn is
/// seen under the key: `` left: Seen(…) right: Unread ``.
#[test]
fn muse_with_and_without_an_api_key() {
    let home = home("muse");
    let path = home.path().join(".config/muse/auth.json");
    write(&path, &muse_auth(UNICORN, "oauth"));
    let stored = env(&[("HOME", home.path().to_str().unwrap())]);
    let got = muse(&process(&stored, None), home.path());
    let got = seen(&got);
    assert_eq!(got.cli, ObservedCli::Muse);
    assert_eq!(got.email, UNICORN);
    assert_eq!(got.src, ObservedSrc::AuthFile);
    assert_eq!(got.path, path);
    let keyed = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("META_API_KEY", "fixture"),
    ]);
    assert_eq!(
        muse(&process(&keyed, None), home.path()),
        Observation::Unread
    );
    let xdg = home.path().join("xdg");
    write(&xdg.join("muse/auth.json"), &muse_auth(BRONTO, "oauth"));
    let moved = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
    ]);
    assert_eq!(
        seen(&muse(&process(&moved, None), home.path())).email,
        BRONTO
    );
}

/// A Muse `auth.json` that names no OAuth account is signed out; no file is unread.
/// NEGATIVE CONTROL: with `muse_login_for` answering `Unread` for a file with no account, the
/// signed-out file is unread: `` left: Unread right: SignedOut { … } ``.
#[test]
fn muse_without_an_account_is_not_an_account() {
    let home = home("muse-none");
    let env = env(&[("HOME", home.path().to_str().unwrap())]);
    assert_eq!(muse(&process(&env, None), home.path()), Observation::Unread);
    let path = home.path().join(".config/muse/auth.json");
    write(&path, &muse_auth(UNICORN, "api_key"));
    assert_eq!(
        muse(&process(&env, None), home.path()),
        Observation::SignedOut { path }
    );
}

fn realm_keyer(realm: &str, email: &str) -> Option<String> {
    match (realm, email) {
        ("codex", UNICORN) => Some("ac-00000000000000c1".to_owned()),
        ("codex", BRONTO) => Some("ac-00000000000000c2".to_owned()),
        ("google", UNICORN) => Some("ac-00000000000000d1".to_owned()),
        ("google", BRONTO) => Some("ac-00000000000000d2".to_owned()),
        ("meta", UNICORN) => Some("ac-00000000000000e1".to_owned()),
        ("kimi:global", UNICORN) => Some("ac-00000000000000f1".to_owned()),
        ("kimi:mainland-cn", UNICORN) => Some("ac-00000000000000f2".to_owned()),
        _ => None,
    }
}

fn observed_of(cli: ObservedCli, email: &str, src: ObservedSrc) -> Observation {
    Observation::Seen(Observed {
        cli,
        email: email.to_owned(),
        key: None,
        region: None,
        src,
        path: PathBuf::from("/home/someone/login-file"),
        mtime: Some(1),
    })
}

fn pool_m22(cli: Cli) -> Roster {
    Roster::parse_for(
        cli,
        &format!("[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n"),
    )
    .unwrap()
}

/// The same marks for a pooled Codex tab as for Claude Code's: its sighting keyed by the
/// observed account under Codex's realm, one `wrong-account` against the believed row, and the
/// login handed to the registry under that realm.
/// NEGATIVE CONTROL: with `sighting` keying by `claims_of(Cli::Claude, …)`, no `codex` key is
/// found and no note is made: `` left: [] right: ["seen", "wrong-account", "session-start"] ``.
#[test]
fn a_codex_tab_makes_the_same_marks() {
    crate::harness::journal::with_keyer(realm_keyer, || {
        let roster = pool_m22(Cli::Codex);
        let tab = Tab::of("machine", "s-c0de");
        let mut kept = Sightings::default();
        let seen = observed_of(Cli::Codex.into(), UNICORN, ObservedSrc::AuthFile);
        let first = sighting(
            Some(&roster),
            &stored(),
            &seen,
            &mut kept,
            tab,
            "codex",
            1_000,
        );
        assert_eq!(kinds(&first), ["seen", "wrong-account", "session-start"]);
        assert_eq!(first.notes[0].cli, "codex");
        assert_eq!(first.notes[0].src, "auth-file");
        assert_eq!(
            first.notes[1].account.unwrap().as_str(),
            "ac-00000000000000c1"
        );
        assert_eq!(first.notes[1].to.unwrap().as_str(), "ac-00000000000000c2");
        let login = first.login.unwrap();
        assert_eq!(
            (login.cli.id(), login.realm.as_str(), login.email.as_str()),
            ("codex", "codex", UNICORN)
        );
        // The row's own account agrees: its sighting, and nothing against the row.
        let mut kept = Sightings::default();
        let agrees = observed_of(Cli::Codex.into(), BRONTO, ObservedSrc::AuthFile);
        let first = sighting(
            Some(&roster),
            &stored(),
            &agrees,
            &mut kept,
            tab,
            "codex",
            1_000,
        );
        assert_eq!(kinds(&first), ["seen"]);
    });
}

/// agy's account is keyed under its provider's realm, `google`, as its provider reads key it:
/// the claims ring's `agy` is no realm the registry keys an account by. A disagreement names
/// the believed row under that realm too.
/// NEGATIVE CONTROL: with `observed_claims` answering `claims_of` for agy, the realm is `agy`
/// and no key is found: `` left: [] right: ["seen", "wrong-account", "session-start"] ``.
#[test]
fn an_agy_tab_is_keyed_by_its_providers_realm() {
    crate::harness::journal::with_keyer(realm_keyer, || {
        let roster = pool_m22(Cli::Agy);
        let tab = Tab::of("machine", "s-a61a");
        let mut kept = Sightings::default();
        let seen = observed_of(Cli::Agy.into(), UNICORN, ObservedSrc::CliLog);
        let first = sighting(
            Some(&roster),
            &stored(),
            &seen,
            &mut kept,
            tab,
            "agy",
            1_000,
        );
        assert_eq!(kinds(&first), ["seen", "wrong-account", "session-start"]);
        assert_eq!(first.notes[0].cli, "agy");
        assert_eq!(first.notes[0].src, "cli-log");
        assert_eq!(
            first.notes[0].account.unwrap().as_str(),
            "ac-00000000000000d1"
        );
        assert_eq!(first.notes[1].to.unwrap().as_str(), "ac-00000000000000d2");
        assert_eq!(first.login.unwrap().realm, "google");
    });
}

/// A Muse Code tab has no roster and so no belief: its sighting and its session's start are
/// keyed by the observed account under `meta`, nothing is called a wrong account, and its end
/// closes the session and hands the login back.
/// NEGATIVE CONTROL: with `sighting_unpooled` passing a roster that lists another account, a
/// `wrong-account` is written: `` left: ["seen", "wrong-account", "session-start"] right:
/// ["seen", "session-start"] ``.
#[test]
fn a_muse_tab_is_its_observed_account_alone() {
    crate::harness::journal::with_keyer(realm_keyer, || {
        let tab = Tab::of("machine", "s-0b5e");
        let mut kept = Sightings::default();
        let seen = observed_of(ObservedCli::Muse, UNICORN, ObservedSrc::AuthFile);
        let first = sighting_unpooled(&seen, &mut kept, tab, "muse", 1_000);
        assert_eq!(kinds(&first), ["seen", "session-start"]);
        assert_eq!(first.notes[0].cli, "muse");
        assert_eq!(
            first.notes[0].account.unwrap().as_str(),
            "ac-00000000000000e1"
        );
        let login = first.login.unwrap();
        assert_eq!(
            (login.cli.id(), login.realm.as_str(), login.email.as_str()),
            ("muse", "meta", UNICORN)
        );
        assert_eq!(
            kinds(&sighting_unpooled(&seen, &mut kept, tab, "muse", 1_060)),
            Vec::<&str>::new()
        );
        let ended = kept.end(2_000);
        assert_eq!(kinds(&ended), ["session-end"]);
        assert_eq!(ended.login.unwrap().state, LoginState::Idle);
    });
}

/// A process routed past any account changes no belief and makes no mark.
/// NEGATIVE CONTROL: with `Observation::seen` answering a routed observation's account, the
/// reconciliation is `Unlisted`: `` left: Unlisted(…) right: Unread(Some(…)) ``.
#[test]
fn a_routed_process_keeps_the_belief_unverified() {
    let roster = pool_m22(Cli::Agy);
    let routed = Observation::Routed {
        route: ObservedRoute::Gateway,
    };
    assert_eq!(
        crate::harness::accounts::reconcile(&roster, &stored(), &routed),
        Reconciled::Unread(roster.row("m22"))
    );
    let mut kept = Sightings::default();
    let tab = Tab::of("machine", "s-a61a");
    assert_eq!(
        sighting(
            Some(&roster),
            &stored(),
            &routed,
            &mut kept,
            tab,
            "agy",
            1_000
        ),
        Sighted::default()
    );
}

const GROK_USER: &str = "grok-user-fixture-0001";
const KIMI_USER: &str = "kimi-user-fixture-0001";

/// A Grok Build `auth.json` with one login record under xAI's sign-in scope: its `key` is a
/// made-up word that authenticates nothing.
fn grok_auth(user: &str, email: Option<&str>) -> String {
    let email = email.map_or_else(String::new, |email| format!(r#","email":"{email}""#));
    format!(
        r#"{{"https://accounts.x.ai/sign-in":{{"key":"fixture-not-a-token","user_id":"{user}"{email}}}}}"#
    )
}

fn who_of(cli: Cli, env: &[String]) -> ProcessLogin {
    ProcessLogin::of_env_for(cli, env.iter().filter_map(|kv| kv.split_once('=')))
}

fn grok_at(env: &[String], home: &Path) -> Observation {
    grok(&process(env, None), home, &who_of(Cli::Grok, env))
}

/// Grok Build's login is the xAI account its `auth.json` names, keyed by its user id: the
/// default home's, the one `GROK_HOME` names, or the login directory `GROK_AUTH_PATH` names,
/// as the process's own environment selects.
/// NEGATIVE CONTROL: with `grok` joining `auth.json` to the default home whatever `who.login`
/// says, the pooled row's process reads the default's bronto: `` left: "brontosaurus…" right:
/// "unicorn…" ``.
#[test]
fn grok_file_names_the_xai_account() {
    let home = home("grok");
    let path = home.path().join(".grok/auth.json");
    write(&path, &grok_auth(GROK_USER, Some(BRONTO)));
    let plain = env(&[("HOME", home.path().to_str().unwrap())]);
    let got = grok_at(&plain, home.path());
    let got = seen(&got);
    assert_eq!(got.cli, ObservedCli::Pool(Cli::Grok));
    assert_eq!(got.email, BRONTO);
    assert_eq!(got.key, AccountKey::grok(GROK_USER));
    assert!(got.key.is_some());
    assert_eq!(got.region, None);
    assert_eq!(got.src, ObservedSrc::AuthFile);
    assert_eq!(got.path, path);
    // The row's own login file, as a pooled launch names it.
    let own = home.path().join("logins/work-2/auth.json");
    write(&own, &grok_auth("grok-user-fixture-0002", Some(UNICORN)));
    let pooled = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("GROK_AUTH_PATH", own.to_str().unwrap()),
    ]);
    let got = grok_at(&pooled, home.path());
    assert_eq!(seen(&got).email, UNICORN);
    assert_eq!(seen(&got).path, own);
    assert_eq!(seen(&got).key, AccountKey::grok("grok-user-fixture-0002"));
    // Another home.
    let moved = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("GROK_HOME", own.parent().unwrap().to_str().unwrap()),
    ]);
    assert_eq!(seen(&grok_at(&moved, home.path())).email, UNICORN);
}

/// A credential variable signs Grok Build in ahead of its login file: the process runs on no
/// account of its own, whatever the file names, and the route says so.
/// NEGATIVE CONTROL: with `grok` reading the file before the variables, the file's account is
/// seen under the key: `` left: Seen(…) right: Routed { route: Env } ``.
#[test]
fn grok_with_a_credential_variable_is_routed_by_env() {
    let home = home("grok-env");
    write(
        &home.path().join(".grok/auth.json"),
        &grok_auth(GROK_USER, Some(BRONTO)),
    );
    for name in ["XAI_API_KEY", "GROK_AUTH"] {
        let keyed = env(&[("HOME", home.path().to_str().unwrap()), (name, "fixture")]);
        let got = grok_at(&keyed, home.path());
        assert_eq!(
            got,
            Observation::Routed {
                route: ObservedRoute::Env
            },
            "{name}"
        );
        assert_eq!(ObservedRoute::Env.word(), "env");
        assert_eq!(got.seen(), None);
    }
}

/// What names no Grok account: no file is unread; a file with no login record, an empty file
/// and one grok refused are signed out; records of two users leave the account unsaid; and a
/// record with a user id and no address is a key-only sighting, which names no address.
/// NEGATIVE CONTROL: with `grok_named` taking the first record's user, the two-user file names
/// the first: `` left: KeyOnly { … } right: Unread ``.
#[test]
fn grok_without_an_addressed_account_is_not_one() {
    let home = home("grok-none");
    let plain = env(&[("HOME", home.path().to_str().unwrap())]);
    assert_eq!(grok_at(&plain, home.path()), Observation::Unread);
    let path = home.path().join(".grok/auth.json");
    write(&path, "{}");
    assert_eq!(
        grok_at(&plain, home.path()),
        Observation::SignedOut { path: path.clone() }
    );
    write(&path, "");
    assert_eq!(
        grok_at(&plain, home.path()),
        Observation::SignedOut { path: path.clone() }
    );
    write(
        &path,
        r#"{"a":{"key":"fixture","user_id":"one"},"b":{"key":"fixture","user_id":"two"}}"#,
    );
    assert_eq!(grok_at(&plain, home.path()), Observation::Unread);
    write(&path, &grok_auth(GROK_USER, None));
    assert_eq!(
        grok_at(&plain, home.path()),
        Observation::KeyOnly {
            key: AccountKey::grok(GROK_USER).unwrap(),
            path: path.clone()
        }
    );
    // A login grok refused is one it will not start on: no account runs there.
    write(&path, &grok_auth(GROK_USER, Some(BRONTO)));
    assert_eq!(seen(&grok_at(&plain, home.path())).email, BRONTO);
    write(
        &crate::harness::grok_signin::refused_marker(&path),
        "refused",
    );
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(crate::harness::grok_signin::refused_marker(&path))
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert_eq!(
        grok_at(&plain, home.path()),
        Observation::SignedOut { path }
    );
}

/// An unsigned stand-in for a token that is a set of claims: a header, `claims` and a signature
/// word. It authenticates nothing.
fn token_of(claims: &str) -> String {
    let part = |bytes: &[u8]| {
        aterm_codec::base64::encode(bytes)
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_")
    };
    format!(
        "{}.{}.fixture",
        part(br#"{"alg":"none"}"#),
        part(claims.as_bytes())
    )
}

/// A Kimi Code credential file whose access token is `token`.
fn kimi_credential(token: &str) -> String {
    format!(
        r#"{{"access_token":"{token}","refresh_token":"fixture-not-a-token","expires_at":1,"scope":"fixture","token_type":"Bearer","expires_in":1}}"#
    )
}

/// A Kimi Code `config.toml` whose default model runs on the managed provider, which keeps its
/// login in the slot `key` on that slot's region's endpoints (the mainland's for its fixed slot,
/// the global ones for any other), beside a provider of someone's own and a model on it.
fn kimi_config(key: &str) -> String {
    let endpoints = if key == "oauth/kimi-code" {
        "base_url = \"https://api.kimi.com/coding/v1\"\n"
    } else {
        "base_url = \"https://api.kimi.ai/coding/v1\"\n"
    };
    let host = if key == "oauth/kimi-code" {
        ""
    } else {
        "oauth_host = \"https://auth.kimi.ai\"\n"
    };
    format!(
        "default_model = \"model\"\n\
         [providers.home]\ntype = \"openai\"\nbase_url = \"http://127.0.0.1:1/v1\"\n\
         api_key = \"fixture\"\n\
         [providers.\"managed:kimi-code\"]\ntype = \"kimi\"\n{endpoints}api_key = \"\"\n\
         [providers.\"managed:kimi-code\".oauth]\nstorage = \"file\"\nkey = \"{key}\"\n{host}\
         [models.model]\nprovider = \"managed:kimi-code\"\n\
         [models.own]\nprovider = \"home\"\n"
    )
}

/// The slot and credential file of the global account system's managed login
/// (`cli_shared_kimi_tests.rs` holds the hash to Kimi's own).
const KIMI_GLOBAL_SLOT: &str = "oauth/kimi-code-env-0e4f99c69cc27850";
const KIMI_GLOBAL_FILE: &str = "credentials/kimi-code-env-0e4f99c69cc27850.json";

fn kimi_at(env: &[String], home: &Path) -> Observation {
    kimi(&process(env, None), home, &who_of(Cli::Kimi, env))
}

/// Kimi Code's login is the account its managed login's token names, in the account system its
/// home is set up for: one user id in the two regions is two accounts, each with its own
/// credential file and its own key. The home is the one `KIMI_CODE_HOME` names, else
/// `~/.kimi-code`.
/// NEGATIVE CONTROL: with `kimi_slot` answering the mainland file for every slot, the global
/// home's login is the other region's file's: `` left: "brontosaurus…" right: "unicorn…" ``.
#[test]
fn kimi_in_both_regions() {
    let home = home("kimi");
    let claims = |email: &str| format!(r#"{{"user_id":"{KIMI_USER}","email":"{email}"}}"#);
    let mainland = home.path().join(".kimi-code");
    write(
        &mainland.join("config.toml"),
        &kimi_config("oauth/kimi-code"),
    );
    write(
        &mainland.join("credentials/kimi-code.json"),
        &kimi_credential(&token_of(&claims(BRONTO))),
    );
    let plain = env(&[("HOME", home.path().to_str().unwrap())]);
    let got = kimi_at(&plain, home.path());
    let got = seen(&got);
    assert_eq!(got.cli, ObservedCli::Pool(Cli::Kimi));
    assert_eq!(got.email, BRONTO);
    assert_eq!(got.region, Some(Region::MainlandCn));
    assert_eq!(got.key, AccountKey::kimi(Region::MainlandCn, KIMI_USER));
    assert_eq!(got.src, ObservedSrc::AuthFile);
    assert_eq!(got.path, mainland.join("credentials/kimi-code.json"));

    let global = home.path().join("logins/work-2");
    write(&global.join("config.toml"), &kimi_config(KIMI_GLOBAL_SLOT));
    write(
        &global.join(KIMI_GLOBAL_FILE),
        &kimi_credential(&token_of(&claims(UNICORN))),
    );
    // The other region's file in the same home is not the one this home reads.
    write(
        &global.join("credentials/kimi-code.json"),
        &kimi_credential(&token_of(&claims(BRONTO))),
    );
    let pooled = env(&[
        ("HOME", home.path().to_str().unwrap()),
        ("KIMI_CODE_HOME", global.to_str().unwrap()),
    ]);
    let got = kimi_at(&pooled, home.path());
    let got = seen(&got);
    assert_eq!(got.email, UNICORN);
    assert_eq!(got.region, Some(Region::Global));
    assert_eq!(got.key, AccountKey::kimi(Region::Global, KIMI_USER));
    assert_ne!(got.key, AccountKey::kimi(Region::MainlandCn, KIMI_USER));
    assert_eq!(got.path, global.join(KIMI_GLOBAL_FILE));
}

/// What names no addressed Kimi account. A token whose claims name a user and no address is a
/// key-only sighting (by `sub` where there is no `user_id`); a token that is no set of claims
/// says nothing of its account, which only Kimi's profile request would, and is unread; a
/// credential file with no token is signed out; and a home with no managed login, a slot of
/// someone's own endpoints, endpoints moved by the environment, or an environment model, is
/// unread.
/// NEGATIVE CONTROL: with `kimi_named` answering `Nobody` for a token that is no set of claims,
/// a signed-in home reads as signed out: `` left: SignedOut { … } right: Unread ``.
#[test]
fn kimi_key_only_and_unread() {
    let home = home("kimi-none");
    let plain = env(&[("HOME", home.path().to_str().unwrap())]);
    let dir = home.path().join(".kimi-code");
    let path = dir.join("credentials/kimi-code.json");
    // A credential and no config: the home's account system is unknown.
    write(
        &path,
        &kimi_credential(&token_of(&format!(r#"{{"sub":"{KIMI_USER}"}}"#))),
    );
    assert_eq!(kimi_at(&plain, home.path()), Observation::Unread);
    write(&dir.join("config.toml"), &kimi_config("oauth/kimi-code"));
    assert_eq!(
        kimi_at(&plain, home.path()),
        Observation::KeyOnly {
            key: AccountKey::kimi(Region::MainlandCn, KIMI_USER).unwrap(),
            path: path.clone()
        }
    );
    write(&path, &kimi_credential("fixture-opaque-word"));
    assert_eq!(kimi_at(&plain, home.path()), Observation::Unread);
    write(&path, "{}");
    assert_eq!(
        kimi_at(&plain, home.path()),
        Observation::SignedOut { path: path.clone() }
    );
    // A readable account, then each environment that moves the process off it.
    write(
        &path,
        &kimi_credential(&token_of(&format!(
            r#"{{"user_id":"{KIMI_USER}","email":"{BRONTO}"}}"#
        ))),
    );
    assert_eq!(seen(&kimi_at(&plain, home.path())).email, BRONTO);
    for (name, value) in [
        ("KIMI_CODE_BASE_URL", "http://127.0.0.1:1/v1"),
        ("KIMI_CODE_OAUTH_HOST", "http://127.0.0.1:1"),
        ("KIMI_MODEL_NAME", "fixture"),
    ] {
        let moved = env(&[("HOME", home.path().to_str().unwrap()), (name, value)]);
        assert_eq!(kimi_at(&moved, home.path()), Observation::Unread, "{name}");
    }
    write(
        &dir.join("config.toml"),
        &kimi_config("oauth/kimi-code-env-0000000000000000"),
    );
    assert_eq!(kimi_at(&plain, home.path()), Observation::Unread);
    std::fs::remove_file(&path).unwrap();
    write(&dir.join("config.toml"), &kimi_config("oauth/kimi-code"));
    assert_eq!(kimi_at(&plain, home.path()), Observation::Unread);
}

/// THE WRONG-ACCOUNT BUG: a home that keeps a managed provider's table and its credential file
/// from an earlier login, while the model its turns run is on a provider of its own
/// (`docs/measured/kimi-credentials-2026-10-24.md` §1), uses none of that account's quota and
/// is unread: by its default model, and by the model its command line names. The same holds
/// for a managed provider that is no region's native route: another type, another endpoint, a
/// key of its own, or an issuer that is not the region's.
/// NEGATIVE CONTROL: with `kimi_slot` not asking which provider the model runs on, the process
/// is sighted on the earlier login's account:
/// `` ["kimi", "-m", "own"] left: Seen(Observed { … }) right: Unread ``.
#[test]
fn kimi_off_its_managed_login_is_unread() {
    let home = home("kimi-own");
    let plain = env(&[("HOME", home.path().to_str().unwrap())]);
    let dir = home.path().join(".kimi-code");
    let config = dir.join("config.toml");
    let managed = kimi_config("oauth/kimi-code");
    write(
        &dir.join("credentials/kimi-code.json"),
        &kimi_credential(&token_of(&format!(
            r#"{{"user_id":"{KIMI_USER}","email":"{BRONTO}"}}"#
        ))),
    );
    let at = |argv: &[&str]| {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        kimi(
            &Process {
                argv: &argv,
                env: &plain,
                cwd: None,
            },
            home.path(),
            &who_of(Cli::Kimi, &plain),
        )
    };
    write(&config, &managed);
    assert_eq!(
        at(&[]).email(),
        Some(BRONTO),
        "CONTROL: on its managed login"
    );
    assert_eq!(at(&["kimi", "-m", "model"]).email(), Some(BRONTO));
    assert_eq!(
        at(&["kimi", "-p", "-m", "--", "-m", "own"]).email(),
        Some(BRONTO),
        "a prompt and what follows `--` name no model"
    );
    for argv in [
        &["kimi", "-m", "own"][..],
        &["kimi", "--model", "own"],
        &["kimi", "--model=own"],
        &["kimi", "-mown"],
        &["kimi", "-m", "model", "-m", "own"],
        &["kimi", "-m", "unlisted"],
    ] {
        assert_eq!(at(argv), Observation::Unread, "{argv:?}");
    }
    for (what, from, to) in [
        (
            "default model on another provider",
            "default_model = \"model\"",
            "default_model = \"own\"",
        ),
        (
            "no default model",
            "default_model = \"model\"",
            "other = \"model\"",
        ),
        ("another type", "type = \"kimi\"", "type = \"openai\""),
        (
            "another endpoint",
            "https://api.kimi.com/coding/v1",
            "http://127.0.0.1:1/v1",
        ),
        (
            "a key of its own",
            "api_key = \"\"",
            "api_key = \"fixture\"",
        ),
        (
            "another issuer",
            "storage = \"file\"",
            "storage = \"file\"\noauth_host = \"http://127.0.0.1:1\"",
        ),
        (
            "a login not in a file",
            "storage = \"file\"",
            "storage = \"keyring\"",
        ),
    ] {
        assert!(managed.contains(from), "{what}");
        write(&config, &managed.replace(from, to));
        assert_eq!(at(&[]), Observation::Unread, "{what}");
    }
    // Its command line can put it back on the managed login.
    write(
        &config,
        &managed.replace("default_model = \"model\"", "default_model = \"own\""),
    );
    assert_eq!(at(&["kimi", "--model", "model"]).email(), Some(BRONTO));
}

fn kimi_seen(region: Region, email: &str) -> Observation {
    Observation::Seen(Observed {
        cli: Cli::Kimi.into(),
        email: email.to_owned(),
        key: AccountKey::kimi(region, KIMI_USER),
        region: Some(region),
        src: ObservedSrc::AuthFile,
        path: PathBuf::from("/home/someone/.kimi-code/credentials/kimi-code.json"),
        mtime: Some(1),
    })
}

/// A Kimi Code tab's account is keyed under its region's realm, and a row of the other region
/// does not answer to its address: the mainland login of an address the roster lists under the
/// global system is an account no row lists, noted against the believed row.
/// NEGATIVE CONTROL: with `observed_claims` passing no region, the mainland login is keyed
/// under `kimi:global`: `` left: "ac-…f1" right: "ac-…f2" ``; with `reconcile` matching by
/// address alone, the mainland login agrees with the global row and is not `Unlisted`.
#[test]
fn a_kimi_tab_is_keyed_and_matched_by_its_region() {
    crate::harness::journal::with_keyer(realm_keyer, || {
        let roster = Roster::parse_for(
            Cli::Kimi,
            &format!(
                "[[account]]\nlabel = \"k1\"\nclass = \"dedicated\"\nemail = \"{UNICORN}\"\n\
                 region = \"global\"\n"
            ),
        )
        .unwrap();
        let global = kimi_seen(Region::Global, UNICORN);
        assert_eq!(
            crate::harness::accounts::reconcile(&roster, &stored(), &global),
            Reconciled::Agrees(roster.row("k1").unwrap())
        );
        let mainland = kimi_seen(Region::MainlandCn, UNICORN);
        assert!(matches!(
            crate::harness::accounts::reconcile(&roster, &stored(), &mainland),
            Reconciled::Unlisted(_)
        ));
        let tab = Tab::of("machine", "s-71a1");
        let mut kept = Sightings::default();
        let first = sighting(
            Some(&roster),
            &stored(),
            &mainland,
            &mut kept,
            tab,
            "kimi",
            1_000,
        );
        assert_eq!(kinds(&first), ["seen", "wrong-account", "session-start"]);
        assert_eq!(first.notes[0].cli, "kimi");
        assert_eq!(
            first.notes[0].account.unwrap().as_str(),
            "ac-00000000000000f2"
        );
        assert_eq!(first.notes[1].to.unwrap().as_str(), "ac-00000000000000f1");
        assert_eq!(first.login.unwrap().realm, "kimi:mainland-cn");
    });
}

/// A key-only sighting names no address: it answers to no row, changes no belief and makes no
/// mark, so nothing is born of it.
#[test]
fn a_key_only_login_keeps_the_belief_unverified() {
    let roster = pool_m22(Cli::Grok);
    let keyed = Observation::KeyOnly {
        key: AccountKey::grok(GROK_USER).unwrap(),
        path: PathBuf::from("/home/someone/.grok/auth.json"),
    };
    assert_eq!(
        crate::harness::accounts::reconcile(&roster, &stored(), &keyed),
        Reconciled::Unread(roster.row("m22"))
    );
    let mut kept = Sightings::default();
    let tab = Tab::of("machine", "s-960c");
    assert_eq!(
        sighting(
            Some(&roster),
            &stored(),
            &keyed,
            &mut kept,
            tab,
            "grok",
            1_000
        ),
        Sighted::default()
    );
}
