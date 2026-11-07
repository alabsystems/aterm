// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one judgment (DESIGN-claude-setup-token S8): the token route's shape in a process's own
//! environment, the steps of [`judge`] in order, the session-starting shapes, and reasons that
//! name a variable or a document, never a value.

use super::*;
use crate::harness::accounts::{AuthRoute, LaunchEnv, Roster};
use std::cell::Cell;

/// The lab key of each login email the roster names.
fn keyer(_realm: &str, email: &str) -> Option<String> {
    match email {
        "main@example.com" => Some("ac-00000000000000a1".to_owned()),
        "work@example.com" => Some("ac-00000000000000b2".to_owned()),
        _ => None,
    }
}

/// `main` is the default login's row, `work` a login of its own.
fn roster() -> Roster {
    Roster::parse_for(
        accounts::Cli::Claude,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.com\"\n\n\
         [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"/logins/work\"\n\
         email = \"work@example.com\"\n",
    )
    .expect("the roster")
}

const MAIN: &str = "aterm-claude-token-ac-00000000000000a1";
const WORK: &str = "aterm-claude-token-ac-00000000000000b2";

fn entry(name: &str) -> TokenEntry {
    TokenEntry::parse(name).expect("a token entry")
}

fn exec_env(pairs: &[(&str, &str)]) -> Vec<String> {
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect()
}

fn of(pairs: &[(&str, &str)]) -> ProcessLogin {
    ProcessLogin::of_exec_env(&exec_env(pairs)).expect("a read environment")
}

/// THE TOKEN ROUTE'S SHAPE IN A PROCESS'S OWN ENVIRONMENT (§5.2): the descriptor and the marker
/// together, nothing else; and the gate keeps it only for the account whose entry the marker
/// names, an absent login being the default row's.
///
/// NEGATIVE CONTROL: read the marker alone as the token (drop the descriptor from `of_env`'s
/// match), and the "marker alone" case reads `VaultToken`.
#[test]
fn a_process_is_on_the_token_only_by_the_descriptor_and_the_marker_together() {
    crate::harness::journal::with_keyer(keyer, || {
        let r = roster();
        let token = |entry_name| AuthRoute::VaultToken {
            entry: entry(entry_name),
        };
        let full = [
            (DESCRIPTOR_ENV, "3"),
            (MARKER_ENV, WORK),
            (accounts::LOGIN_ENV, "/logins/work"),
        ];
        let who = of(&full);
        assert_eq!(who.route, token(WORK), "the full token shape");
        assert_eq!(who.login.as_deref(), Some("/logins/work"));
        assert_eq!(who.me(&r).route, token(WORK));
        assert_eq!(accounts::judged_label(&r, &who), Some("work"));

        let with_auth = [full.as_slice(), &[("ANTHROPIC_AUTH_TOKEN", "x")]].concat();
        assert_eq!(
            of(&with_auth).route,
            AuthRoute::Outside,
            "ANTHROPIC_AUTH_TOKEN"
        );
        assert_eq!(
            of(&[(MARKER_ENV, WORK)]).route,
            AuthRoute::Outside,
            "the marker alone"
        );
        assert_eq!(
            of(&[(DESCRIPTOR_ENV, "3")]).route,
            AuthRoute::Outside,
            "the descriptor alone"
        );
        assert_eq!(
            of(&[(DESCRIPTOR_ENV, "3"), (MARKER_ENV, &format!("{WORK}-meta"))]).route,
            AuthRoute::Outside,
            "a marker naming no token entry"
        );
        assert_eq!(
            of(&[
                (DESCRIPTOR_ENV, "3"),
                (MARKER_ENV, WORK),
                (MARKER_ENV, MAIN)
            ])
            .route,
            AuthRoute::Outside,
            "two markers"
        );

        // A foreign login: the marker names main's entry, the login is work's row.
        let foreign = of(&[
            (DESCRIPTOR_ENV, "3"),
            (MARKER_ENV, MAIN),
            (accounts::LOGIN_ENV, "/logins/work"),
        ]);
        assert_eq!(
            foreign.route,
            token(MAIN),
            "the shape alone, with no roster"
        );
        assert_eq!(
            foreign.me(&r).route,
            AuthRoute::Outside,
            "outside at the gate"
        );
        assert_eq!(accounts::judged_label(&r, &foreign), None);

        // An absent login is the default row's.
        let default = of(&[(DESCRIPTOR_ENV, "3"), (MARKER_ENV, MAIN)]);
        assert_eq!(default.me(&r).route, token(MAIN));
        assert_eq!(accounts::judged_label(&r, &default), Some("main"));
        // A marker naming another account's entry is outside at the gate.
        let other = of(&[(DESCRIPTOR_ENV, "3"), (MARKER_ENV, WORK)]);
        assert_eq!(other.me(&r).route, AuthRoute::Outside);
        assert_eq!(accounts::judged_label(&r, &other), None);

        // A token session's launch record sets the wrapper's state aside, and reads as a stored
        // login's: the relaunch's wrapper makes both afresh.
        let record = LaunchEnv::of_exec_env(&exec_env(&full)).expect("admitted");
        assert!(
            record
                .entries()
                .iter()
                .all(|e| !e.starts_with(DESCRIPTOR_ENV) && !e.starts_with(MARKER_ENV)),
            "{record:?}"
        );
        assert_eq!(
            ProcessLogin::of_exec_env(record.entries()).unwrap().route,
            AuthRoute::Stored
        );
        assert_eq!(
            LaunchEnv::of_exec_env(&exec_env(&[(DESCRIPTOR_ENV, "3")])),
            None,
            "CONTROL: a stray descriptor is no token session's"
        );
    });
}

/// A stand-in for what [`judge`] asks, counting each question.
struct Fake {
    account: Result<(Account, Option<TokenEntry>), String>,
    claims: Option<Fallback>,
    token: TokenFact,
    asked: Cell<u32>,
}

impl Fake {
    fn on_work(token: TokenFact) -> Self {
        Self {
            account: Ok((
                Account {
                    label: Some("work".to_owned()),
                    email: "work@example.com".to_owned(),
                },
                Some(entry(WORK)),
            )),
            claims: None,
            token,
            asked: Cell::new(0),
        }
    }
}

impl Ask for Fake {
    fn account(&self) -> Result<(Account, Option<TokenEntry>), String> {
        self.asked.set(self.asked.get() + 1);
        self.account.clone()
    }
    fn claims(&self, _account: &Account) -> Option<Fallback> {
        self.asked.set(self.asked.get() + 1);
        self.claims.clone()
    }
    fn token(&self, _entry: TokenEntry, _account: &Account) -> TokenFact {
        self.asked.set(self.asked.get() + 1);
        self.token
    }
}

fn words(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|w| (*w).to_owned()).collect()
}

/// THE STEPS RUN IN ORDER (§5.3): each fallback has its word, the cheap steps ask nothing, and
/// only a token in use decides `Token`.
#[test]
fn the_judgment_runs_its_steps_in_order() {
    let argv = words(&["claude"]);
    let facts = Facts {
        macos: true,
        no_harness: false,
        argv: &argv,
        store_build: true,
    };
    let fake = Fake::on_work(TokenFact::InUse);
    assert!(matches!(
        judge(&facts, &fake),
        Decision::Token { entry: e, ref account } if e == entry(WORK) && account.named() == "work"
    ));
    for (facts, word) in [
        (
            Facts {
                macos: false,
                ..facts
            },
            "platform",
        ),
        (
            Facts {
                no_harness: true,
                ..facts
            },
            "no-harness",
        ),
    ] {
        let fake = Fake::on_work(TokenFact::InUse);
        let decided = judge(&facts, &fake);
        assert!(matches!(decided, Decision::AsTyped(_)), "{decided:?}");
        assert_eq!(decided.fallback().map(Fallback::word), Some(word));
        assert_eq!(fake.asked.get(), 0, "{word}: nothing is asked");
    }
    let fake = Fake::on_work(TokenFact::InUse);
    assert_eq!(
        judge(
            &Facts {
                store_build: false,
                ..facts
            },
            &fake
        ),
        Decision::Stored(Fallback::Program)
    );
    assert_eq!(fake.asked.get(), 0);

    let outside = Fake {
        account: Err("`HTTPS_PROXY` in the environment".to_owned()),
        ..Fake::on_work(TokenFact::InUse)
    };
    assert_eq!(
        judge(&facts, &outside),
        Decision::AsTyped(Fallback::Outside(
            "`HTTPS_PROXY` in the environment".to_owned()
        ))
    );
    let unkeyed = Fake {
        account: Ok((
            Account {
                label: Some("bare".to_owned()),
                email: String::new(),
            },
            None,
        )),
        ..Fake::on_work(TokenFact::InUse)
    };
    assert_eq!(judge(&facts, &unkeyed), Decision::Stored(Fallback::Absent));
    let claimed = Fake {
        claims: Some(Fallback::Claimed("leased elsewhere")),
        ..Fake::on_work(TokenFact::InUse)
    };
    assert_eq!(
        judge(&facts, &claimed),
        Decision::Stored(Fallback::Claimed("leased elsewhere"))
    );
    assert_eq!(
        claimed.asked.get(),
        2,
        "the token is not read past the claims"
    );
    for (fact, word) in [
        (TokenFact::Absent, "absent"),
        (TokenFact::Busy, "busy"),
        (TokenFact::Unverified, "unverified"),
        (TokenFact::Ended, "ended"),
        (TokenFact::Rejected, "rejected"),
        (TokenFact::Mismatched, "mismatched"),
        (TokenFact::Unreadable, "unreadable"),
    ] {
        let decided = judge(&facts, &Fake::on_work(fact));
        assert!(matches!(decided, Decision::Stored(_)), "{decided:?}");
        assert_eq!(decided.fallback().map(Fallback::word), Some(word));
        // The wrapper journals each but `absent` (an account with no token says nothing).
        assert!(
            word == "absent" || crate::harness::journal::WHYS.contains(&word),
            "{word}"
        );
    }
}

/// SESSION-STARTING SHAPES ONLY (§5.3, step 2): a subcommand or a one-word prompt runs as typed,
/// and so does a line with a `--settings` or `--bare` of its own; an interactive launch, `-p`,
/// `--resume` and `--continue` are served, and the tap's own `--settings` is set aside.
///
/// NEGATIVE CONTROL: judge the argv without [`super::super::tap::without_own_tap`], and the
/// tapped launches read `shape`.
#[test]
fn only_session_starting_shapes_are_served() {
    let tap = crate::harness::tap::settings_value(Path::new("/opt/aterm/bin/aterm"))
        .expect("a tap value");
    for typed in [
        &["claude", "mcp", "list"][..],
        &["claude", "doctor"],
        &["claude", "setup-token"],
        &["claude", "auth", "status"],
        &["claude", "plugin"],
        &["claude", "update"],
        &["claude", "hello"],
        &["claude", "--settings", "/tmp/mine.json"],
        &["claude", "--bare"],
    ] {
        assert!(shape_refusal(&words(typed)).is_some(), "{typed:?}");
    }
    for served in [
        &["claude"][..],
        &["claude", "-p", "say hello"],
        &["claude", "--print", "say hello"],
        &["claude", "--resume", "0b6a1c1e-7d2f-4c55-9a8e-111111111111"],
        &["claude", "--continue"],
        &["claude", "fix the failing test"],
        &["claude", "--model", "opus"],
    ] {
        assert_eq!(shape_refusal(&words(served)), None, "{served:?}");
    }
    let tapped = words(&["claude", "--settings", &tap, "--continue"]);
    assert_eq!(shape_refusal(&tapped), None, "the tap's own --settings");
    let inline = words(&["claude", &format!("--settings={tap}")]);
    assert_eq!(shape_refusal(&inline), None, "the tap's own --settings=");
    let both = words(&["claude", "--settings", &tap, "--settings", "/tmp/mine.json"]);
    assert_eq!(
        shape_refusal(&both),
        Some("its own `--settings` or `--bare`"),
        "a --settings of the line's own stays"
    );
}

/// A launch's environment, its settings chain, whether the vendor's profile store holds a
/// configuration, and the reason [`outside_why`] gives.
type OutsideCase<'a> = (
    Vec<(&'a str, &'a str)>,
    Vec<(String, Source)>,
    bool,
    &'a str,
);

/// EACH REASON NAMES THE VARIABLE OR THE DOCUMENT, NEVER A VALUE (§5.3, step 4).
#[test]
fn a_reason_names_the_variable_or_the_document_never_a_value() {
    const SECRET: &str = "https://sentinel.invalid/never-shown";
    let project = "/work/repo/.claude/settings.json".to_owned();
    let cases: Vec<OutsideCase<'_>> = vec![
        (
            vec![("ANTHROPIC_BASE_URL", SECRET)],
            vec![],
            false,
            "`ANTHROPIC_BASE_URL` in the environment",
        ),
        (
            vec![("HTTPS_PROXY", SECRET)],
            vec![],
            false,
            "`HTTPS_PROXY` in the environment",
        ),
        (
            vec![],
            vec![(
                project.clone(),
                Source::Text(format!(r#"{{"env":{{"ANTHROPIC_BASE_URL":"{SECRET}"}}}}"#)),
            )],
            false,
            "`ANTHROPIC_BASE_URL` in `/work/repo/.claude/settings.json`",
        ),
        (
            vec![],
            vec![(
                project.clone(),
                Source::Text(format!(r#"{{"apiKeyHelper":"{SECRET}"}}"#)),
            )],
            false,
            "`apiKeyHelper` in `/work/repo/.claude/settings.json`",
        ),
        (
            vec![],
            vec![(project.clone(), Source::Unreadable)],
            false,
            "`/work/repo/.claude/settings.json` cannot be read",
        ),
        (
            vec![],
            vec![],
            true,
            "the vendor's profile store holds a configuration",
        ),
    ];
    for (env, settings, profile, says) in cases {
        let who = ProcessLogin::of_env(env.iter().copied())
            .with_settings(&settings)
            .with_profile_store(profile);
        assert_eq!(who.route, AuthRoute::Outside, "{says}");
        let why = outside_why(&env, &settings, profile).expect("a reason");
        assert_eq!(why, says);
        assert!(!why.contains("sentinel"), "{why}");
        let fallback = Fallback::Outside(why);
        assert!(!fallback.why().contains("sentinel"));
        assert!(fallback.why().starts_with("outside: "));
    }
    assert_eq!(
        outside_why(&[("HOME", "/Users//person")], &[], false),
        None,
        "CONTROL: a stored login's environment names nothing"
    );
}

/// TODAY'S JUDGMENT OF THE ACCOUNT (§5.3, step 4): the row the login resolves, a label naming
/// that row and an email that account; a row-less login's account by its login record.
#[test]
fn the_account_is_the_one_the_launchs_login_resolves() {
    crate::harness::journal::with_keyer(keyer, || {
        let r = roster();
        let none = |_: Option<&str>| None;
        let nothing = || None;
        let stored = |login: Option<&str>| ProcessLogin {
            login: login.map(str::to_owned),
            default_home: true,
            route: AuthRoute::Stored,
        };
        let (account, found) =
            account_of(&stored(None), Some(&r), Named::Auto, &none, &nothing).unwrap();
        assert_eq!(account.named(), "main");
        assert_eq!(found, Some(entry(MAIN)));
        let work = stored(Some("/logins/work"));
        assert!(account_of(&work, Some(&r), Named::Label("work"), &none, &nothing).is_ok());
        assert!(
            account_of(
                &work,
                Some(&r),
                Named::Label("work@example.com"),
                &none,
                &nothing
            )
            .is_ok()
        );
        let refused = account_of(&work, Some(&r), Named::Label("main"), &none, &nothing)
            .expect_err("work's login is not main's");
        assert!(refused.contains("another row's login"), "{refused}");
        // A row-less login: the login record names the account.
        let record = |_: Option<&str>| Some("work@example.com".to_owned());
        let (account, found) = account_of(
            &stored(Some("/logins/elsewhere")),
            Some(&r),
            Named::Auto,
            &record,
            &nothing,
        )
        .unwrap();
        assert_eq!(account.label, None);
        assert_eq!(account.named(), "work@example.com");
        assert_eq!(found, Some(entry(WORK)));
        // A token session whose marker names its row's entry is on its account's own route.
        let token = ProcessLogin {
            route: AuthRoute::VaultToken { entry: entry(WORK) },
            ..work.clone()
        };
        assert!(account_of(&token, Some(&r), Named::Auto, &none, &nothing).is_ok());
        let foreign = ProcessLogin {
            route: AuthRoute::VaultToken { entry: entry(MAIN) },
            ..work.clone()
        };
        assert!(account_of(&foreign, Some(&r), Named::Auto, &none, &nothing).is_err());
        let elsewhere = ProcessLogin {
            default_home: false,
            ..work
        };
        assert!(account_of(&elsewhere, Some(&r), Named::Auto, &none, &nothing).is_err());
    });
}

/// THE PROGRAM (§5.3, step 3): a Claude Code build in atpkg's store, an exec entry resolved;
/// nothing else.
#[cfg(unix)]
#[test]
fn only_a_claude_build_in_the_store_is_a_program_the_token_serves() {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-token-route-store-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let build = root.path().join("store/claude/7/bin/claude");
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();
    std::fs::write(&build, "#!/bin/sh\n").unwrap();
    assert!(store_build(&build).is_some());
    let entry = root.path().join("store/claude/exec/claude");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&build, &entry).unwrap();
    assert!(
        store_build(&entry).is_some(),
        "an exec entry resolves to the build"
    );
    let other = root.path().join("store/codex/7/bin/claude");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::write(&other, "").unwrap();
    for refused in [
        other,
        root.path().join("bin/claude"),
        root.path().join("store/claude/07/bin/claude"),
        root.path().join("store/claude/8/bin/claude"),
    ] {
        assert_eq!(store_build(&refused), None, "{}", refused.display());
    }
}

/// THE ROUTE MOVE A RUNNING SESSION IS OFFERED (§5.5): a stored session moves onto a token in use
/// here; a token session moves back only on its token's refusal, removal, end or mismatch, and
/// only where a stored login is here; an exposed token (in use) and an unsettled read move
/// nothing.
///
/// NEGATIVE CONTROL: drop `if stored_here` from `route_move`, and the "no stored login" rows move
/// back onto a login that is not here.
#[test]
fn a_running_session_moves_onto_its_token_and_back_only_where_the_other_route_is_here() {
    let stored = AuthRoute::Stored;
    let token = AuthRoute::VaultToken { entry: entry(MAIN) };
    assert_eq!(
        route_move(stored, TokenFact::InUse, true),
        Some(RouteTo::Token)
    );
    assert_eq!(
        route_move(stored, TokenFact::InUse, false),
        Some(RouteTo::Token),
        "a token in use serves a row with no stored login too"
    );
    for fact in [
        TokenFact::Absent,
        TokenFact::Busy,
        TokenFact::Unverified,
        TokenFact::Ended,
        TokenFact::Rejected,
        TokenFact::Mismatched,
        TokenFact::Unreadable,
    ] {
        assert_eq!(route_move(stored, fact, true), None, "{fact:?}");
    }
    for fact in [
        TokenFact::Absent,
        TokenFact::Ended,
        TokenFact::Rejected,
        TokenFact::Mismatched,
    ] {
        assert_eq!(
            route_move(token, fact, true),
            Some(RouteTo::Stored),
            "{fact:?}"
        );
        assert_eq!(
            route_move(token, fact, false),
            None,
            "{fact:?}: no stored login here, so it stays"
        );
    }
    for fact in [
        TokenFact::InUse,
        TokenFact::Busy,
        TokenFact::Unverified,
        TokenFact::Unreadable,
    ] {
        assert_eq!(route_move(token, fact, true), None, "{fact:?}");
    }
    // An exposed token is still in use here (§4.3): it moves nothing.
    #[cfg(unix)]
    {
        let exposed = super::super::accounts_tokens::TokenState::Exposed(Vec::new());
        assert_eq!(fact_of(&exposed), TokenFact::InUse);
        assert_eq!(route_move(token, fact_of(&exposed), true), None);
    }
    for fact in [TokenFact::InUse, TokenFact::Absent, TokenFact::Rejected] {
        assert_eq!(route_move(AuthRoute::Outside, fact, true), None, "{fact:?}");
    }
}

/// A RUNNING SESSION'S ACCOUNT (§5.2): its login's row, else the account its login record names.
#[test]
fn a_running_sessions_account_is_its_logins_row_or_its_records() {
    crate::harness::journal::with_keyer(keyer, || {
        let r = roster();
        let who = |login: Option<&str>| ProcessLogin {
            login: login.map(str::to_owned),
            default_home: true,
            route: AuthRoute::Stored,
        };
        let (account, found) = session_account(&who(None), Some(&r), None).expect("main");
        assert_eq!(account.named(), "main");
        assert_eq!(found, entry(MAIN));
        let (account, found) =
            session_account(&who(Some("/logins/work")), Some(&r), None).expect("work");
        assert_eq!(account.named(), "work");
        assert_eq!(found, entry(WORK));
        let (account, found) = session_account(
            &who(Some("/logins/elsewhere")),
            Some(&r),
            Some("work@example.com".to_owned()),
        )
        .expect("the record's account");
        assert_eq!(account.label, None);
        assert_eq!(found, entry(WORK));
        assert!(session_account(&who(Some("/logins/elsewhere")), Some(&r), None).is_none());
        assert!(
            session_account(
                &who(Some("/logins/elsewhere")),
                None,
                Some("nobody@example.com".to_owned())
            )
            .is_none(),
            "an account the lab keys no entry for"
        );
    });
}

/// A LINE ANOTHER LAUNCHER TYPES (§5.4): a store build with session-starting flags is typed
/// behind the wrapper, `--auto`, with the build and its flags after `--`; anything else is typed
/// as it was.
#[cfg(unix)]
#[test]
fn a_line_for_a_store_build_types_the_wrapper_ahead_of_it() {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-token-route-line-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let build = root.path().join("store/claude/7/bin/claude");
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();
    std::fs::write(&build, "#!/bin/sh\n").unwrap();
    let aterm = PathBuf::from("/checked/aterm");
    let flags = vec!["--dangerously-skip-permissions".to_owned()];
    with_wrapper(aterm.clone(), || {
        let line = auto_line(&build, &flags);
        if cfg!(target_os = "macos") {
            let (exe, args) = line.expect("the wrapper");
            assert_eq!(exe, aterm);
            let mut want: Vec<String> = VERB.iter().map(|w| (*w).to_owned()).collect();
            want.extend([
                "--auto".to_owned(),
                "--".to_owned(),
                build.to_str().unwrap().to_owned(),
                "--dangerously-skip-permissions".to_owned(),
            ]);
            assert_eq!(args, want);
        } else {
            assert_eq!(line, None, "off macOS");
        }
        let other = root.path().join("bin/claude");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "").unwrap();
        assert_eq!(auto_line(&other, &flags), None);
        let own = vec!["--settings".to_owned(), "{}".to_owned()];
        assert_eq!(
            auto_line(&build, &own),
            None,
            "a line with its own settings is no shape the token serves"
        );
    });
}
