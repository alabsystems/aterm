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
        at!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.com\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"",
            "/logins/work",
            "\"\nemail = \"work@example.com\"\n"
        ),
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

/// THE WRAPPER NAMES WHICH TOKEN OF THE ENTRY IT SERVED (§5.2, §5.6): [`ServedToken::of`] reads
/// the `minted_at` beside the marker; where the variable is unset, the session is an earlier
/// wrapper's, which named none, and its birth is kept; a value that is not a number, or two
/// different values, name nothing. What the session is to the vault's token ([`ServedToken::on`]):
/// the named token alone; for an earlier wrapper's session, a token made before it began, and no
/// later one. It leaves the route's shape as it is: alone, beside no descriptor or marker, the
/// variable names nothing, so it is not part of the token state a relaunch line unsets
/// ([`TOKEN_STATE_ENV`]).
#[test]
fn the_wrapper_names_which_token_of_the_entry_it_served() {
    let read = |pairs: &[(&str, &str)]| ServedToken::of(pairs.iter().copied(), Some(1_000));
    let served = [
        (DESCRIPTOR_ENV, "3"),
        (MARKER_ENV, WORK),
        (MINTED_ENV, "1790000000"),
    ];
    assert_eq!(read(&served), ServedToken::Named(1_790_000_000));
    assert_eq!(
        of(&served).route,
        AuthRoute::VaultToken { entry: entry(WORK) }
    );
    assert_eq!(
        read(&[(DESCRIPTOR_ENV, "3"), (MARKER_ENV, WORK)]),
        ServedToken::Unnamed(Some(1_000)),
        "a wrapper that named no token"
    );
    for (why, value) in [("empty", ""), ("a word", "soon"), ("negative", "-5")] {
        assert_eq!(read(&[(MINTED_ENV, value)]), ServedToken::Garbled, "{why}");
    }
    assert_eq!(
        read(&[(MINTED_ENV, "5"), (MINTED_ENV, "6")]),
        ServedToken::Garbled,
        "two tokens named"
    );
    assert_eq!(
        read(&[(MINTED_ENV, "5"), (MINTED_ENV, "5")]),
        ServedToken::Named(5)
    );
    assert_eq!(ServedToken::Named(5).on(5), OnToken::Named);
    assert_eq!(
        ServedToken::Named(5).on(6),
        OnToken::Other,
        "a replaced token"
    );
    assert_eq!(
        ServedToken::Unnamed(Some(1_000)).on(999),
        OnToken::Unnamed,
        "made before the session began"
    );
    for (why, served, minted_at) in [
        (
            "made after it began",
            ServedToken::Unnamed(Some(1_000)),
            1_001,
        ),
        ("made as it began", ServedToken::Unnamed(Some(1_000)), 1_000),
        ("no birth", ServedToken::Unnamed(None), 1),
        ("garbled", ServedToken::Garbled, 1),
    ] {
        assert_eq!(served.on(minted_at), OnToken::Other, "{why}");
    }
    assert_eq!(
        of(&[(MINTED_ENV, "1790000000")]).route,
        AuthRoute::Stored,
        "alone it names no token"
    );
    assert!(!TOKEN_STATE_ENV.contains(&MINTED_ENV));
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
            (accounts::LOGIN_ENV, at!("/logins/work")),
        ];
        let who = of(&full);
        assert_eq!(who.route, token(WORK), "the full token shape");
        assert_eq!(who.login.as_deref(), Some(at!("/logins/work")));
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
            (accounts::LOGIN_ENV, at!("/logins/work")),
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
        Decision::Token { entry: e, ref account } if e == entry(WORK) && account.label.as_deref() == Some("work")
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
        (TokenFact::AwaitingBasis, "awaiting-basis"),
        (TokenFact::Differ, "differ"),
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
    let tap = crate::harness::tap::settings_value(Path::new(at!("/opt/aterm/bin/aterm")))
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

/// TODAY'S JUDGMENT OF THE ACCOUNT (§5.3, step 4): the row the login resolves, with its token
/// entry; a row-less login's account by its login record. (A wrapper line that names its account
/// holds this account to its word: `accounts_token_run`'s tests.)
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
        let (account, found) = account_of(&stored(None), Some(&r), &none, &nothing).unwrap();
        assert_eq!(account.label.as_deref(), Some("main"));
        assert_eq!(found, Some(entry(MAIN)));
        let work = stored(Some(at!("/logins/work")));
        let (account, found) = account_of(&work, Some(&r), &none, &nothing).unwrap();
        assert_eq!(account.label.as_deref(), Some("work"));
        assert_eq!(found, Some(entry(WORK)));
        // A row-less login: the login record names the account.
        let record = |_: Option<&str>| Some("work@example.com".to_owned());
        let (account, found) = account_of(
            &stored(Some(at!("/logins/elsewhere"))),
            Some(&r),
            &record,
            &nothing,
        )
        .unwrap();
        assert_eq!(account.label, None);
        assert_eq!(account.email, "work@example.com");
        assert_eq!(found, Some(entry(WORK)));
        // A token session whose marker names its row's entry is on its account's own route.
        let token = ProcessLogin {
            route: AuthRoute::VaultToken { entry: entry(WORK) },
            ..work.clone()
        };
        assert!(account_of(&token, Some(&r), &none, &nothing).is_ok());
        let foreign = ProcessLogin {
            route: AuthRoute::VaultToken { entry: entry(MAIN) },
            ..work.clone()
        };
        assert!(account_of(&foreign, Some(&r), &none, &nothing).is_err());
        let elsewhere = ProcessLogin {
            default_home: false,
            ..work
        };
        assert!(account_of(&elsewhere, Some(&r), &none, &nothing).is_err());
    });
}

/// THE PROGRAM (§5.3, step 3): a Claude Code build in atpkg's store (`…/store/claude/<build>/
/// bin/` and the executable atpkg names there, `claude`, or `claude.exe` on Windows), an exec entry
/// resolved; nothing else: another program's store, a folder that is no build, a build with no
/// such file, and a file in the build's `bin` under another name (the other host's spelling, a
/// `.cmd`). NEGATIVE CONTROL (run on Windows): the shape naming the bare `claude` on every host,
/// the Windows build is refused.
#[test]
fn only_a_claude_build_in_the_store_is_a_program_the_token_serves() {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-token-route-store-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let (exe, other_spelling) = if cfg!(windows) {
        ("claude.exe", "claude")
    } else {
        ("claude", "claude.exe")
    };
    let at = |parts: &[&str]| {
        parts
            .iter()
            .fold(root.path().to_path_buf(), |p, s| p.join(s))
    };
    let lay = |path: &Path| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
    };
    let build = at(&["store", "claude", "7", "bin", exe]);
    lay(&build);
    assert_eq!(
        store_build(&build),
        Some(std::fs::canonicalize(&build).unwrap())
    );
    #[cfg(unix)]
    {
        // An exec entry is a symbolic link atpkg lays on Unix alone.
        let entry = at(&["store", "claude", "exec", "claude"]);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&build, &entry).unwrap();
        assert!(
            store_build(&entry).is_some(),
            "an exec entry resolves to the build"
        );
    }
    let other = at(&["store", "codex", "7", "bin", exe]);
    let misspelled = at(&["store", "claude", "7", "bin", other_spelling]);
    let cmd = at(&["store", "claude", "7", "bin", "claude.cmd"]);
    for file in [&other, &misspelled, &cmd] {
        lay(file);
    }
    for refused in [
        other,
        misspelled,
        cmd,
        at(&["bin", exe]),
        at(&["store", "claude", "07", "bin", exe]),
        at(&["store", "claude", "8", "bin", exe]),
    ] {
        assert_eq!(store_build(&refused), None, "{}", refused.display());
    }
}

/// THE ROUTE MOVE A RUNNING SESSION IS OFFERED (§5.5): a stored session moves onto a token in use
/// here; a token session moves back only on its token's refusal, removal, end, mismatch,
/// differing windows or, on a Mac that did not mint it, its not being in use there before its
/// verification (`unverified`: a token made at a terminal or by a meta that names no route,
/// `MintedBy::serves_unverified`), and only where a stored login is here; an exposed token (in
/// use), an unsettled read and a token held for its basis move nothing.
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
        TokenFact::AwaitingBasis,
        TokenFact::Differ,
        TokenFact::Ended,
        TokenFact::Rejected,
        TokenFact::Mismatched,
        TokenFact::Unreadable,
    ] {
        assert_eq!(route_move(stored, fact, true), None, "{fact:?}");
    }
    for fact in [
        TokenFact::Absent,
        TokenFact::Unverified,
        TokenFact::Differ,
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
    // Held for its basis, a session on the stored login stays to give it, and one already on
    // the token stays on it.
    for fact in [
        TokenFact::InUse,
        TokenFact::Busy,
        TokenFact::AwaitingBasis,
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

/// A SESSION ON A TOKEN THE VAULT NO LONGER HOLDS MOVES (§5.5, §5.6): its own token serves
/// nowhere now, so [`stale_move`] takes it onto the vault's token where that is in use here, else
/// onto the stored login where it is here, whatever kept the vault's token from use (refused,
/// windows different, held for its basis, not verified for this Mac); with no stored login here
/// it stays, and a vault not read just now moves nothing. Such a session is one its wrapper
/// served an earlier token, one whose wrapper named nothing it could read, and one an earlier
/// wrapper served before the vault's token was made ([`ServedToken::off`]); one on the vault's
/// token is none of them. CONTROL: [`route_move`], which judges the vault's token as the
/// session's, moves none of them off a token whose replacement is in use or held for its basis.
#[test]
fn a_session_on_a_token_the_vault_no_longer_holds_moves() {
    let token = AuthRoute::VaultToken { entry: entry(WORK) };
    for served in [
        ServedToken::Named(5),
        ServedToken::Garbled,
        ServedToken::Unnamed(Some(6)),
        ServedToken::Unnamed(Some(5)),
    ] {
        assert!(served.off(6), "{served:?}");
    }
    for served in [ServedToken::Named(6), ServedToken::Unnamed(Some(7))] {
        assert!(!served.off(6), "{served:?}: on the vault's token");
    }
    assert_eq!(stale_move(TokenFact::InUse, true), Some(RouteTo::Token));
    assert_eq!(stale_move(TokenFact::InUse, false), Some(RouteTo::Token));
    assert_eq!(
        route_move(token, TokenFact::InUse, true),
        None,
        "CONTROL: judged as the session's own token"
    );
    for fact in [
        TokenFact::Rejected,
        TokenFact::Differ,
        TokenFact::AwaitingBasis,
        TokenFact::Unverified,
        TokenFact::Absent,
    ] {
        assert_eq!(stale_move(fact, true), Some(RouteTo::Stored), "{fact:?}");
        assert_eq!(
            stale_move(fact, false),
            None,
            "{fact:?}: no stored login here"
        );
    }
    assert_eq!(
        route_move(token, TokenFact::AwaitingBasis, true),
        None,
        "CONTROL"
    );
    assert_eq!(stale_move(TokenFact::Busy, true), None, "a vault not read");
}

/// A SESSION OF UNKNOWN BIRTH MOVES ONLY WHERE BOTH READINGS AGREE (§5.5, §5.6): an earlier
/// wrapper served it, naming no token, and its process birth cannot be read, so it may run on the
/// vault's token as well as on an earlier one. Its readings count for neither
/// ([`ServedToken::on`] says [`OnToken::Other`]), but it is not known to be off the vault's token
/// ([`ServedToken::off`]), so it is judged as a session on it ([`route_move`]). Where the vault's
/// token was refused, found `differ`, ended, mismatched or is unverified for this Mac, a session
/// on it and one on an earlier token both move back to the stored login where it is here, and so
/// does this one. Where the vault's token is in use or held for its basis, a session on an earlier
/// token would move ([`stale_move`]) and one on the vault's token would stay; this one stays: a
/// relaunch onto the vault's token might only serve it again. The offered move over a scratch
/// vault is `the_offered_move_reads_the_vault_and_the_roster`'s.
#[test]
fn a_session_of_unknown_birth_moves_only_where_both_readings_agree() {
    let unknown = ServedToken::Unnamed(None);
    assert_eq!(unknown.on(1), OnToken::Other, "its readings stay apart");
    assert!(!unknown.off(1), "it may run on the vault's token");
    let token = AuthRoute::VaultToken { entry: entry(WORK) };
    for fact in [
        TokenFact::Rejected,
        TokenFact::Differ,
        TokenFact::Unverified,
        TokenFact::Ended,
        TokenFact::Mismatched,
    ] {
        assert_eq!(
            stale_move(fact, true),
            route_move(token, fact, true),
            "{fact:?}: the readings agree"
        );
        assert_eq!(
            route_move(token, fact, true),
            Some(RouteTo::Stored),
            "{fact:?}"
        );
        assert_eq!(route_move(token, fact, false), None, "{fact:?}");
    }
    for fact in [TokenFact::InUse, TokenFact::AwaitingBasis] {
        assert_ne!(
            stale_move(fact, true),
            route_move(token, fact, true),
            "{fact:?}: the readings disagree"
        );
        assert_eq!(route_move(token, fact, true), None, "{fact:?}: it stays");
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
        assert_eq!(account.label.as_deref(), Some("main"));
        assert_eq!(found, entry(MAIN));
        let (account, found) =
            session_account(&who(Some(at!("/logins/work"))), Some(&r), None).expect("work");
        assert_eq!(account.label.as_deref(), Some("work"));
        assert_eq!(found, entry(WORK));
        let (account, found) = session_account(
            &who(Some(at!("/logins/elsewhere"))),
            Some(&r),
            Some("work@example.com".to_owned()),
        )
        .expect("the record's account");
        assert_eq!(account.label, None);
        assert_eq!(found, entry(WORK));
        assert!(session_account(&who(Some(at!("/logins/elsewhere"))), Some(&r), None).is_none());
        assert!(
            session_account(
                &who(Some(at!("/logins/elsewhere"))),
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

/// THE MOVE A RUNNING SESSION IS OFFERED, READ FROM THE VAULT (§5.5, §5.6): over a scratch seated
/// vault holding a token minted here with nothing compared yet, a session on the row's stored
/// login is offered the token while no login record of the row's account is here, and nothing
/// once one is (the token is held for its basis, the roster saying which login is the row's). A
/// session on a token the vault no longer holds (served an earlier token, or named none and began
/// before the vault's token was made) moves onto the vault's token while it is in use, and onto
/// the stored login while it is held; one on the vault's token stays, and so does one an earlier
/// wrapper served whose birth is not known. Once this Mac records the vault's token refused, every
/// session on a token moves back to the stored login, that one included. NEGATIVE CONTROLS:
/// judged with no roster, the held token reads in use and the stored-login session is offered it;
/// judged as on the vault's token, the stale session is offered nothing; and (applied to
/// [`offered_move`], then reverted) a session of unknown birth judged as one on an earlier token,
/// `served.on(..) == OnToken::Other`, is offered the vault's token while it is in use,
/// `left: Some(Token)`, while one offered nothing whatever the token's standing, as before, stays
/// on the refused token, `left: None`.
#[cfg(target_os = "macos")]
#[test]
fn the_offered_move_reads_the_vault_and_the_roster() {
    use crate::harness::accounts::ProcessLogin;
    use crate::harness::accounts_tokens::tests::Lab;
    use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Meta};
    use aterm_secrets::client::{MemberWritten, OnBusy};
    const KEY: &str = "ac-fedcba9876543210";
    const NOW: u64 = 1_800_000_000;
    fn keyer(_realm: &str, _email: &str) -> Option<String> {
        Some(KEY.to_owned())
    }
    let lab = Lab::new();
    let minted_at = NOW - 86_400;
    let meta = Meta {
        email: "person@example.com".to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at,
        expires_at: NOW + 300 * 86_400,
        expiry_from: ExpiryFrom::Printed,
        minted_on: "m22".to_owned(),
        cli: "2.1.318".to_owned(),
        minted_by: None,
        verified: None,
    };
    let written = claude_token::write(
        &lab.paths,
        &Key::parse(KEY).expect("a key"),
        &aterm_secrets::SecretBytes::from(crate::harness::setup_token::tests::token().as_bytes()),
        &meta,
        OnBusy::Refuse(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("the write");
    assert!(
        matches!(written, MemberWritten::Posted | MemberWritten::SignedHere),
        "{written:?}"
    );
    let state = aterm_tempfile::Builder::new()
        .prefix("aterm-offered-move-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let store = accounts::Store::at(state.path());
    let roster = Roster::parse_for(
        accounts::Cli::Claude,
        "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \"person@example.com\"\n",
    )
    .expect("the roster");
    let home = lab.paths.home.clone();
    let on = |route: AuthRoute, served: ServedToken| {
        let who = ProcessLogin {
            login: None,
            default_home: true,
            route,
        };
        crate::harness::journal::with_keyer(keyer, || {
            offered_move(
                &home,
                &lab.paths,
                &store,
                Some(&roster),
                (&who, served),
                NOW,
            )
        })
    };
    let token = AuthRoute::VaultToken {
        entry: entry(&format!("aterm-claude-token-{KEY}")),
    };
    let earlier = ServedToken::Named(minted_at - 600);
    let before_it = ServedToken::Unnamed(Some(minted_at - 1));
    let on_it = [
        ServedToken::Named(minted_at),
        ServedToken::Unnamed(Some(minted_at + 1)),
    ];
    assert_eq!(
        on(AuthRoute::Stored, ServedToken::Unnamed(None)),
        Some(RouteTo::Token)
    );
    for stale in [earlier, before_it] {
        assert_eq!(on(token, stale), Some(RouteTo::Token), "{stale:?}");
    }
    for served in on_it {
        assert_eq!(on(token, served), None, "{served:?}");
    }
    assert_eq!(
        on(token, ServedToken::Unnamed(None)),
        None,
        "a session of unknown birth may be on the vault's token: it is not relaunched onto it"
    );
    std::fs::write(
        home.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"person@example.com"}}"#,
    )
    .expect("the login record");
    assert_eq!(
        on(AuthRoute::Stored, ServedToken::Unnamed(None)),
        None,
        "held for its basis"
    );
    for stale in [earlier, before_it] {
        assert_eq!(on(token, stale), Some(RouteTo::Stored), "{stale:?}");
    }
    for served in on_it {
        assert_eq!(on(token, served), None, "{served:?}");
    }
    assert_eq!(
        on(token, ServedToken::Unnamed(None)),
        None,
        "a session of unknown birth is not moved off a token held for its basis"
    );
    let Ok(claude_token::Lookup::Found(standing)) =
        claude_token::standing(&lab.paths, &Key::parse(KEY).expect("a key"))
    else {
        panic!("the vault holds the token");
    };
    assert!(
        crate::harness::accounts_tokens::note_rejected(&lab.paths, &store, &standing, 401, NOW)
            .expect("recorded"),
        "this Mac saw the vault's token refused"
    );
    for served in [earlier, before_it, ServedToken::Unnamed(None)]
        .into_iter()
        .chain(on_it)
    {
        assert_eq!(
            on(token, served),
            Some(RouteTo::Stored),
            "{served:?}: off a refused token, whichever it runs on"
        );
    }
}

/// One Mac of the fleet for [`judge`]: its own token table and accounts state in `scratch`, the
/// roster beside it, and the vault's standing of the account's token as it reads it.
#[cfg(unix)]
struct MacOf<'a> {
    scratch: &'a crate::harness::accounts_tokens::tests::Scratch,
    found: aterm_secrets::claude_token::Lookup<aterm_secrets::claude_token::Standing>,
    now: u64,
}

#[cfg(unix)]
impl MacOf<'_> {
    fn roster(&self) -> Roster {
        Roster::load_for(
            accounts::Cli::Claude,
            &self.scratch.path().join("accounts.toml"),
        )
        .expect("the roster")
    }

    /// The one judgment of a session-starting launch on `work`'s stored login on this Mac.
    fn decision(&self) -> Decision {
        let argv = words(&["claude"]);
        let facts = Facts {
            macos: true,
            no_harness: false,
            argv: &argv,
            store_build: true,
        };
        judge(&facts, self)
    }
}

#[cfg(unix)]
impl Ask for MacOf<'_> {
    fn account(&self) -> Result<(Account, Option<TokenEntry>), String> {
        use crate::harness::accounts_tokens::tests::{EMAIL, key};
        Ok((
            Account {
                label: Some("work".to_owned()),
                email: EMAIL.to_owned(),
            },
            TokenEntry::parse(&key().entry()),
        ))
    }
    fn claims(&self, _account: &Account) -> Option<Fallback> {
        None
    }
    fn token(&self, _entry: TokenEntry, account: &Account) -> TokenFact {
        standing_judged(
            &self.found,
            &aterm_secrets::client::Paths::locate_for_home(&self.scratch.path().join("home")),
            &self.scratch.store(),
            Some(&self.roster()),
            &crate::harness::accounts_tokens::tests::key(),
            account,
            self.now,
        )
    }
}

/// A WINDOW-MINTED TOKEN FROM ANOTHER MAC SERVES HERE FROM MINT, AND A TERMINAL-MINTED ONE ONLY
/// ONCE VERIFIED (§5.1, §5.3): m7 minted `work`'s token in aterm's window, which signed in as the
/// account and refused a token for any other, and nobody has verified it; every other Mac reading
/// the same vault starts a launch on it. A token m7 made at its terminal, or one whose meta names
/// no route (an earlier build's), starts launches on m7 alone until it is verified, then on every
/// Mac. CONTROLS, in one fixture: refused here, the window token is `Stored (rejected)` here while
/// a second Mac reading the same vault decides `Token`; this Mac's comparison finding its windows
/// different stops it here alone (`differ`); ended, it is `Stored (ended)`; with this Mac's stored
/// login of the account signed in and no basis yet it is `Stored (awaiting-basis)` and the row the
/// stored login's reading records the basis on is made here, and with that login signed out it is
/// `Token`; a meta with `minted_by` stripped serves only its minting Mac.
#[cfg(unix)]
#[test]
fn a_window_minted_token_from_another_mac_serves_here_from_mint_and_a_terminal_minted_one_only_once_verified()
 {
    use crate::harness::accounts_tokens::tests::{
        A_YEAR, Answer, EMAIL, NOW, Scratch, key, standing as standing_of,
    };
    use crate::harness::accounts_tokens::{Served, Windows, note_basis, note_served};
    use aterm_secrets::claude_token::{Check, Lookup, MintedBy, Standing};
    // The account's token, made on m7 (or here, `here`) by `by`, verified or not, ending at
    // `expires_at`.
    let token = |by: Option<MintedBy>, verified: bool, here: bool, expires_at: u64| {
        let Answer::Found(mut token) =
            standing_of(Check::Readable, expires_at, verified, here, &[])
        else {
            panic!("a standing");
        };
        token.meta.as_mut().expect("a meta").minted_by = by;
        *token
    };
    fn mac<'a>(scratch: &'a Scratch, standing: &Standing) -> MacOf<'a> {
        MacOf {
            scratch,
            found: Lookup::Found(standing.clone()),
            now: NOW,
        }
    }
    let fell_to = |decided: Decision| decided.fallback().map(Fallback::word);
    let window = token(Some(MintedBy::Window), false, false, NOW + A_YEAR);
    let (here, there) = (Scratch::new("route-here"), Scratch::new("route-there"));
    assert!(
        matches!(mac(&here, &window).decision(), Decision::Token { .. }),
        "the window token serves this Mac, which did not mint it, unverified"
    );
    assert!(matches!(
        mac(&there, &window).decision(),
        Decision::Token { .. }
    ));

    // This Mac's refusal is read into its standing (`claude_token::note_refused`, beside its
    // vault state); the second Mac's standing of the same vault carries none.
    let refused_here = Standing {
        refused_at: Some(NOW),
        ..window.clone()
    };
    assert_eq!(
        mac(&here, &refused_here).decision(),
        Decision::Stored(Fallback::Rejected),
        "CONTROL: refused here"
    );
    assert!(
        matches!(mac(&there, &window).decision(), Decision::Token { .. }),
        "CONTROL: a second Mac reading the same vault still starts on it"
    );

    let compared = Scratch::new("route-differ");
    let (stored, theirs) = (
        Windows {
            week_reset: Some(i64::try_from(NOW).unwrap() + 3 * 86_400),
            session_reset: Some(i64::try_from(NOW).unwrap() + 7_200),
        },
        Windows {
            week_reset: Some(i64::try_from(NOW).unwrap() + 5 * 86_400),
            session_reset: Some(i64::try_from(NOW).unwrap() + 3_600),
        },
    );
    assert!(
        note_basis(&compared.store(), &window, "work", &stored, &[], NOW)
            .unwrap()
            .recorded()
    );
    let served = Served {
        label: "work".to_owned(),
        reading: theirs,
        others: Vec::new(),
        named: true,
    };
    note_served(&compared.store(), &window, &served, NOW).unwrap();
    assert_eq!(
        mac(&compared, &window).decision(),
        Decision::Stored(Fallback::Differ),
        "CONTROL: windows that differ here stop it here"
    );
    assert!(matches!(
        mac(&there, &window).decision(),
        Decision::Token { .. }
    ));

    let ended = token(Some(MintedBy::Window), false, false, NOW - 1);
    assert_eq!(
        fell_to(mac(&there, &ended).decision()),
        Some("ended"),
        "CONTROL: ended"
    );

    let signed_in = Scratch::new("route-hold");
    std::fs::write(
        signed_in.path().join("home/.claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}"}}}}"#),
    )
    .expect("the login record");
    assert!(
        signed_in.table().rows.is_empty(),
        "PRECONDITION: no row here"
    );
    assert_eq!(
        mac(&signed_in, &window).decision(),
        Decision::Stored(Fallback::AwaitingBasis),
        "CONTROL: this Mac's stored login signed in, with no basis yet"
    );
    let row = signed_in
        .table()
        .rows
        .remove(key().as_str())
        .expect("CONTROL: the row the basis goes on, made here");
    assert_eq!(
        (row.minted_at, row.label.as_deref()),
        (
            window.meta.as_ref().map(|meta| meta.minted_at),
            Some("work")
        )
    );
    let roster = mac(&signed_in, &window).roster();
    let work = roster.row("work").expect("the row").clone();
    let mut note = None;
    signed_in
        .store()
        .transact_logins(i64::try_from(NOW).unwrap(), &mut note, |state, logins| {
            Ok(accounts::note_login_end(
                state,
                logins,
                &work.label,
                i64::try_from(NOW).unwrap(),
            ))
        })
        .expect("the login's end");
    assert!(
        matches!(mac(&signed_in, &window).decision(), Decision::Token { .. }),
        "CONTROL: that login signed out, the token serves"
    );

    for (case, by) in [
        ("made at m7's terminal", Some(MintedBy::Terminal)),
        ("its meta's minted_by stripped", None),
    ] {
        let elsewhere = Scratch::new("route-terminal");
        let unverified = token(by, false, false, NOW + A_YEAR);
        assert_eq!(
            fell_to(mac(&elsewhere, &unverified).decision()),
            Some("unverified"),
            "{case}: not on a Mac that did not mint it, unverified"
        );
        assert!(
            matches!(
                mac(&elsewhere, &token(by, false, true, NOW + A_YEAR)).decision(),
                Decision::Token { .. }
            ),
            "{case}: on the Mac that minted it"
        );
        assert!(
            matches!(
                mac(&elsewhere, &token(by, true, false, NOW + A_YEAR)).decision(),
                Decision::Token { .. }
            ),
            "{case}: on every Mac once verified"
        );
    }
}

/// A LOST LOGIN IS A TOKEN'S REFUSAL ONLY WHERE CLAUDE CODE'S NEWEST API ERROR ANSWERED `401`
/// (§5.6): the status read off a session's transcript is its newest main-chain API error's, so a
/// `403`, Cloudflare's block page that Claude Code writes as `authentication_failed`, is read as
/// `403`, an earlier `401` behind it counting for nothing, and a sidechain's error and a reply
/// count for nothing either. NEGATIVE CONTROL: the newest error's status reads `401` where it
/// answered `401`, a notice that names no status reads none, and a tail with no API error none.
#[test]
fn the_newest_api_errors_status_is_what_judges_a_refusal() {
    let notice = |status: Option<u16>, sidechain: bool| {
        let status = status.map_or(String::new(), |s| format!(r#","apiErrorStatus":{s}"#));
        format!(
            r#"{{"type":"assistant","isSidechain":{sidechain},"isApiErrorMessage":true,"error":"authentication_failed"{status},"message":{{"model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":"Please run /login"}}]}}}}"#
        )
    };
    let reply = r#"{"type":"assistant","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#;
    let tail = |lines: &[String]| lines.join("\n") + "\n";
    let cloudflare = tail(&[notice(Some(401), false), notice(Some(403), false)]);
    assert_eq!(newest_api_error_status(&cloudflare), Some(403));
    assert_ne!(
        newest_api_error_status(&cloudflare),
        Some(REFUSED_STATUS),
        "an earlier 401 behind it is not the newest"
    );
    let refused = tail(&[
        notice(Some(403), false),
        notice(Some(401), false),
        notice(Some(403), true),
        reply.to_owned(),
    ]);
    assert_eq!(
        newest_api_error_status(&refused),
        Some(REFUSED_STATUS),
        "CONTROL: a sidechain's error and a reply after it count for nothing"
    );
    assert_eq!(newest_api_error_status(&tail(&[notice(None, false)])), None);
    assert_eq!(newest_api_error_status(&tail(&[reply.to_owned()])), None);
    assert_eq!(newest_api_error_status(""), None);
}
