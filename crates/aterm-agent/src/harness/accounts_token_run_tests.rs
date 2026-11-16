// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts token-run` (DESIGN-claude-setup-token S8): the verb's words, each
//! fallback the wrapper reaches over a scratch home, roster, store and store build, the claims
//! snapshot an unlabelled run reads without `accounts.lock`, a scratch seated vault's token
//! decided and a busy one passed over at once, and, on macOS, a stand-in the wrapper execs that
//! reads the token from its descriptor. The stand-in's token is the sentinel no reason holds.

use super::*;
use crate::harness::accounts_tokens::tests::Lab;
use crate::harness::setup_token::tests::token as sentinel;
use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Meta};
use aterm_secrets::client::{MemberWritten, OnBusy};
use std::time::Instant;

/// The run's clock (2027-01-15).
const NOW: i64 = 1_800_000_000;

/// The lab key every test account is keyed by (the lab vault's).
const KEY: &str = "ac-fedcba9876543210";

/// `work` is the default login's row; `lent`, a login of its own, is held by m7; `away`, a login
/// of its own that no Mac holds, was never made here (its directory does not exist).
const ROSTER: &str = "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \
                      \"person@example.com\"\n\n[[account]]\nlabel = \"lent\"\nclass = \
                      \"dedicated\"\ndir = \"/logins/lent\"\nemail = \"lent@example.com\"\n\
                      held_by = \"m7\"\n\n[[account]]\nlabel = \"away\"\nclass = \"shared\"\n\
                      dir = \"/logins/away\"\nemail = \"away@example.com\"\n";

fn keyer(_realm: &str, _email: &str) -> Option<String> {
    Some(KEY.to_owned())
}

fn entry() -> TokenEntry {
    TokenEntry::parse(&format!("aterm-claude-token-{KEY}")).expect("the entry")
}

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// A scratch world: a home, a launch directory, the store build and the accounts store.
struct World {
    dir: aterm_tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-token-run-")
            .suffix(".noindex")
            .tempdir()
            .expect("scratch");
        let world = Self { dir };
        for sub in ["home", "work/.claude", "state"] {
            std::fs::create_dir_all(world.path().join(sub)).expect("a folder");
        }
        let build = world.build();
        std::fs::create_dir_all(build.parent().unwrap()).expect("the build's folder");
        std::fs::write(&build, "#!/bin/sh\n").expect("the build");
        world
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn build(&self) -> PathBuf {
        self.path().join("store/claude/7/bin/claude")
    }

    fn store(&self) -> Store {
        Store::at(&self.path().join("state"))
    }

    /// The context of a launch with environment `env` (`HOME` added) over `vault`.
    fn ctx(&self, env: &[(&str, &str)], vault: Option<&Lab>) -> Ctx {
        let home = self.path().join("home");
        Ctx {
            env: std::iter::once(("HOME".to_owned(), home.display().to_string()))
                .chain(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
                .collect(),
            home: Some(home),
            cwd: self.path().join("work"),
            roster: Some(Roster::parse(ROSTER).expect("the roster")),
            store: Some(self.store()),
            builds: Some(self.path().join("store/claude")),
            vault: vault.map(|lab| lab.paths.clone()),
            now: NOW,
            macos: true,
        }
    }

    /// A run of the store build with `args`, named `named` (`None`: `--auto`).
    fn run(&self, named: Option<&str>, args: &[&str]) -> Run {
        Run {
            named: named.map(str::to_owned),
            program: self.build().into_os_string(),
            args: os(args),
        }
    }
}

/// The lab vault's token for `work`'s account, minted on this Mac (m22) a day ago.
fn mint(lab: &Lab) {
    mint_for(lab, "person@example.com");
}

/// [`mint`] of a token whose record names `email`.
fn mint_for(lab: &Lab, email: &str) {
    let meta = Meta {
        email: email.to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at: 1_800_000_000 - 86_400,
        expires_at: 1_800_000_000 + 300 * 86_400,
        expiry_from: ExpiryFrom::Printed,
        minted_on: "m22".to_owned(),
        cli: "2.1.318".to_owned(),
        minted_by: None,
        verified: None,
    };
    let written = claude_token::write(
        &lab.paths,
        &Key::parse(KEY).expect("a key"),
        &aterm_secrets::SecretBytes::from(sentinel()),
        &meta,
        OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("the write");
    assert!(
        matches!(written, MemberWritten::Posted | MemberWritten::SignedHere),
        "{written:?}"
    );
}

/// What the wrapper decided for `run` in `ctx`, with the lab keyer: the judgment, the read and
/// the fallback it journals ([`resolved`] adds the build it serves and why it starts nothing).
fn decided(run: &Run, ctx: &Ctx) -> (Decision, Option<Read>, Option<(TokenEntry, &'static str)>) {
    let Decided {
        decision,
        read,
        noted,
        ..
    } = resolved(run, ctx);
    (decision, read, noted)
}

/// [`decide_with`] with the lab keyer.
fn resolved(run: &Run, ctx: &Ctx) -> Decided {
    crate::harness::journal::with_keyer(keyer, || decide_with(run, ctx))
}

fn word(decision: &Decision) -> Option<&'static str> {
    decision.fallback().map(Fallback::word)
}

/// THE VERB'S WORDS: a label, an email or `--auto`, then `--`, the program and its arguments.
#[test]
fn the_verb_takes_a_name_or_auto_then_the_program() {
    let run = parse(&os(&["work", "--", "/s/claude", "--resume", "x"])).unwrap();
    assert_eq!(run.named.as_deref(), Some("work"));
    assert_eq!(run.program, OsString::from("/s/claude"));
    assert_eq!(run.args, os(&["--resume", "x"]));
    assert_eq!(parse(&os(&["--auto", "--", "claude"])).unwrap().named, None);
    for refused in [
        &["work", "claude"][..],
        &["work", "--"],
        &["--", "claude"],
        &["--work", "--", "claude"],
        &["", "--", "claude"],
    ] {
        assert!(parse(&os(refused)).is_err(), "{refused:?}");
    }
}

/// THE CHEAP STEPS FALL BACK FIRST: platform, no-harness and shape run the line as typed with
/// nothing asked; a program outside the store is `program`.
#[test]
fn the_platform_harness_shape_and_program_fall_back_first() {
    let world = World::new();
    let run = world.run(None, &[]);
    let ctx = world.ctx(&[], None);
    let other_os = Ctx {
        macos: false,
        ..world.ctx(&[], None)
    };
    assert_eq!(
        decided(&run, &other_os).0,
        Decision::AsTyped(Fallback::Platform)
    );
    let unharnessed = world.ctx(&[("ATERM_NO_HARNESS", "1")], None);
    assert_eq!(
        decided(&run, &unharnessed).0,
        Decision::AsTyped(Fallback::NoHarness)
    );
    for typed in [
        &["mcp", "list"][..],
        &["doctor"],
        &["setup-token"],
        &["auth", "status"],
        &["hello"],
    ] {
        let (decision, read, noted) = decided(&world.run(None, typed), &ctx);
        assert_eq!(word(&decision), Some("shape"), "{typed:?}");
        assert!(matches!(decision, Decision::AsTyped(_)));
        assert!(read.is_none() && noted.is_none());
    }
    let outside_store = Run {
        program: world.path().join("bin/claude").into_os_string(),
        ..world.run(None, &[])
    };
    std::fs::create_dir_all(world.path().join("bin")).unwrap();
    std::fs::write(world.path().join("bin/claude"), "").unwrap();
    assert_eq!(
        decided(&outside_store, &ctx).0,
        Decision::Stored(Fallback::Program)
    );
}

/// THE WRAPPER SERVES ONLY A BUILD IN ATPKG'S OWN STORE, AND THE FILE IT JUDGED (§5.3): a program
/// with a store build's shape anywhere else is `program`, and a link to the store build resolves
/// to the build, which is what the wrapper execs. CONTROLS: the store build itself resolves to its
/// own real path; with no store known, nothing is a store build.
#[test]
fn the_wrapper_serves_only_a_build_in_atpkgs_own_store_by_its_real_path() {
    let world = World::new();
    let ctx = world.ctx(&[], None);
    let real = std::fs::canonicalize(world.build()).unwrap();
    let build = resolved(&world.run(None, &[]), &ctx).build;
    assert_eq!(build.as_deref(), Some(real.as_path()));
    let elsewhere = world.path().join("x/store/claude/1/bin/claude");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    std::fs::write(&elsewhere, "#!/bin/sh\n").unwrap();
    let planted = Run {
        program: elsewhere.into_os_string(),
        ..world.run(None, &[])
    };
    let planted = resolved(&planted, &ctx);
    assert_eq!(planted.decision, Decision::Stored(Fallback::Program));
    assert!(planted.read.is_none() && planted.build.is_none());
    #[cfg(unix)]
    {
        let link = world.path().join("claude-link");
        std::os::unix::fs::symlink(world.build(), &link).unwrap();
        let linked = Run {
            program: link.into_os_string(),
            ..world.run(None, &[])
        };
        let build = resolved(&linked, &ctx).build;
        assert_eq!(build.as_deref(), Some(real.as_path()), "the link's build");
    }
    let unknown = Ctx {
        builds: None,
        ..world.ctx(&[], None)
    };
    let unknown = resolved(&world.run(None, &[]), &unknown);
    assert_eq!(unknown.decision, Decision::Stored(Fallback::Program));
    assert!(unknown.build.is_none());
}

/// TODAY'S JUDGMENT, NEVER OVERRIDDEN: a gateway in the environment, in the project's settings
/// or behind a `--settings` argument, an `apiKeyHelper` and a proxy each keep the descriptor
/// away, and the reason names the variable or the document, never its value.
#[test]
fn a_launch_off_its_stored_login_gets_no_descriptor_and_a_named_reason() {
    const VALUE: &str = "https://sentinel.invalid/gateway";
    let world = World::new();
    let project = world.path().join("work/.claude/settings.json");
    let check = |decision: &Decision, says: &str| {
        let why = decision.fallback().expect("a fallback").why();
        assert!(why.contains(says), "{why}");
        assert!(!why.contains("sentinel"), "{why}");
        assert!(!matches!(decision, Decision::Token { .. }));
    };
    let ctx = world.ctx(&[("ANTHROPIC_BASE_URL", VALUE)], None);
    let (decision, read, _) = decided(&world.run(None, &[]), &ctx);
    check(
        &decision,
        "outside: `ANTHROPIC_BASE_URL` in the environment",
    );
    assert!(read.is_none());
    let ctx = world.ctx(&[("HTTPS_PROXY", VALUE)], None);
    check(
        &decided(&world.run(Some("work"), &[]), &ctx).0,
        "outside: `HTTPS_PROXY` in the environment",
    );

    let ctx = world.ctx(&[], None);
    std::fs::write(
        &project,
        format!(r#"{{"env":{{"ANTHROPIC_BASE_URL":"{VALUE}"}}}}"#),
    )
    .unwrap();
    check(
        &decided(&world.run(None, &[]), &ctx).0,
        &format!("outside: `ANTHROPIC_BASE_URL` in `{}`", project.display()),
    );
    std::fs::write(&project, format!(r#"{{"apiKeyHelper":"{VALUE}"}}"#)).unwrap();
    check(
        &decided(&world.run(None, &[]), &ctx).0,
        &format!("outside: `apiKeyHelper` in `{}`", project.display()),
    );
    std::fs::remove_file(&project).unwrap();

    let given = world.path().join("gateway.json");
    std::fs::write(
        &given,
        format!(r#"{{"env":{{"ANTHROPIC_BASE_URL":"{VALUE}"}}}}"#),
    )
    .unwrap();
    let decision = decided(
        &world.run(None, &["--settings", &given.display().to_string()]),
        &ctx,
    )
    .0;
    assert_eq!(word(&decision), Some("shape"));
    check(&decision, "shape: its own `--settings` or `--bare`");

    // A login of another row than the one the line names.
    let ctx = world.ctx(&[(accounts::LOGIN_ENV, "/logins/lent")], None);
    check(&decided(&world.run(Some("work"), &[]), &ctx).0, "outside: ");
}

/// THE CLAIMS SNAPSHOT (an unlabelled run only): a held row is `held`, another Mac's lease or an
/// off clock `claimed`, read without `accounts.lock`; a labelled line was admitted under it. A
/// held row's run starts nothing, so it journals no fallback.
#[test]
fn an_unlabelled_run_reads_the_claims_ring_from_a_snapshot() {
    let world = World::new();
    let lent = world.ctx(&[(accounts::LOGIN_ENV, "/logins/lent")], None);
    let held = resolved(&world.run(None, &[]), &lent);
    assert_eq!(
        held.decision,
        Decision::Stored(Fallback::Held("lent is held by m7".to_owned()))
    );
    assert_eq!(held.noted, None);
    assert!(held.unstored.is_some());

    let ctx = world.ctx(&[], None);
    let store = world.store();
    store
        .transact(NOW, &mut None, |state| {
            state.note_lease("work", "m7", NOW + 3600, NOW);
            Ok(Vec::new())
        })
        .expect("the lease");
    let (decision, _, noted) = decided(&world.run(None, &[]), &ctx);
    assert_eq!(word(&decision), Some("claimed"), "{decision:?}");
    assert!(matches!(decision, Decision::Stored(_)));
    assert_eq!(noted, Some((entry(), "claimed")));
    // A labelled line reads no claims: it reaches the vault (none here: absent).
    let (decision, _, _) = decided(&world.run(Some("work"), &[]), &ctx);
    assert_eq!(decision, Decision::Stored(Fallback::Absent));

    let world = World::new();
    world
        .store()
        .transact(NOW, &mut None, |state| {
            state.clock = Some(accounts::Clock {
                standing: accounts::ClockStanding::Off { ahead: true },
                by_s: 600,
                against: 2,
                of: 3,
                peers: Vec::new(),
                until: NOW + 3600,
            });
            state.home_accounts.remove("work");
            Ok(Vec::new())
        })
        .expect("the clock");
    let (decision, _, _) = decided(&world.run(None, &[]), &world.ctx(&[], None));
    assert_eq!(word(&decision), Some("claimed"), "{decision:?}");

    // accounts.lock held across the whole run: the run still completes.
    let world = World::new();
    let held = world.store().acquire().expect("accounts.lock");
    let (decision, _, _) = decided(&world.run(None, &[]), &world.ctx(&[], None));
    assert_eq!(decision, Decision::Stored(Fallback::Absent));
    drop(held);
}

/// NO SESSION STARTS ON A STORED LOGIN THIS MAC MUST NOT RUN (FL-135): where the judgment falls
/// back to the row's stored login and that row has none made here (`away`), or another Mac's warm
/// set holds it (`lent`), the wrapper starts nothing ([`Decided::unstored`], [`NOT_STARTED`]) and
/// journals no fallback: a vault writer holding the state (`busy`, for a labelled line and for
/// `--auto` alike), another Mac's lease (`claimed`), the hold (`held`), no token entry
/// (`absent`), and a program outside atpkg's store (`program`, whose account it resolves after
/// the judgment). A run the judgment leaves as typed (a subcommand) is no such fallback. A run on
/// `away` whose token is in use here is served, and one whose token could not then be served
/// starts nothing too ([`refusal`]). NEGATIVE CONTROLS: the default login's row, enrolled here,
/// still falls back to its stored login on the same lease and the same busy vault, and runs as
/// typed where its token could not be served.
#[cfg(unix)]
#[test]
fn a_run_starts_nothing_where_its_fallback_is_a_login_this_mac_must_not_run() {
    let lab = Lab::new();
    let world = World::new();
    let away = world.ctx(&[(accounts::LOGIN_ENV, "/logins/away")], Some(&lab));
    let lent = world.ctx(&[(accounts::LOGIN_ENV, "/logins/lent")], Some(&lab));
    let work = world.ctx(&[], Some(&lab));
    let refused = |run: &Run, ctx: &Ctx, word: &str| {
        let decided = resolved(run, ctx);
        assert_eq!(
            decided.decision.fallback().map(Fallback::word),
            Some(word),
            "{:?}",
            decided.decision
        );
        assert!(matches!(decided.decision, Decision::Stored(_)));
        assert_eq!(decided.noted, None, "nothing started, so no fallback");
        let why = decided
            .unstored
            .clone()
            .expect("the wrapper starts nothing");
        assert_eq!(refusal(&decided, false).as_deref(), Some(why.as_str()));
        why
    };
    let falls_back = |run: &Run, ctx: &Ctx, word: &str| {
        let decided = resolved(run, ctx);
        assert_eq!(decided.decision.fallback().map(Fallback::word), Some(word));
        assert_eq!(decided.unstored, None, "CONTROL: it falls back");
        assert_eq!(refusal(&decided, false), None);
    };

    // No token entry here: `absent`.
    let why = refused(&world.run(None, &[]), &away, "absent");
    assert!(why.contains("away has no login on this Mac"), "{why}");
    falls_back(&world.run(None, &[]), &work, "absent");
    // The hold refuses every route, so `--auto` never reaches the vault on `lent`.
    let why = refused(&world.run(None, &[]), &lent, "held");
    assert!(why.contains("lent is held by m7"), "{why}");
    let why = refused(&world.run(Some("lent"), &[]), &lent, "absent");
    assert!(why.contains("lent is held by m7"), "{why}");

    // A program outside the store: its account is still resolved.
    std::fs::create_dir_all(world.path().join("bin")).unwrap();
    std::fs::write(world.path().join("bin/claude"), "").unwrap();
    let outside_store = Run {
        program: world.path().join("bin/claude").into_os_string(),
        ..world.run(None, &[])
    };
    assert!(refused(&outside_store, &away, "program").contains("away has no login"));
    falls_back(&outside_store, &work, "program");
    // A subcommand runs as typed: no session starts on the login.
    let typed = resolved(&world.run(None, &["mcp", "list"]), &away);
    assert!(matches!(typed.decision, Decision::AsTyped(_)));
    assert_eq!(typed.unstored, None);

    // Another Mac's lease on each row: `claimed`.
    world
        .store()
        .transact(NOW, &mut None, |state| {
            state.note_lease("away", "m7", NOW + 3600, NOW);
            state.note_lease("work", "m7", NOW + 3600, NOW);
            Ok(Vec::new())
        })
        .expect("m7's leases");
    refused(&world.run(None, &[]), &away, "claimed");
    falls_back(&world.run(None, &[]), &work, "claimed");
    world
        .store()
        .transact(NOW, &mut None, |state| {
            state.clear_lease("away");
            state.clear_lease("work");
            Ok(Vec::new())
        })
        .expect("the leases end");

    // `away`'s token in use here: the run on `away` is served, and one whose token could not be
    // served starts nothing either.
    crate::harness::journal::with_keyer(keyer, || mint_for(&lab, "away@example.com"));
    let token = resolved(&world.run(None, &[]), &away);
    assert!(
        matches!(token.decision, Decision::Token { .. }),
        "{:?}",
        token.decision
    );
    assert_eq!(refusal(&token, true), None, "its token is served");
    let why = refusal(&token, false).expect("an unserved token starts nothing");
    assert!(
        why.contains("away has no login on this Mac") && why.contains("token could not be read"),
        "{why}"
    );
    crate::harness::journal::with_keyer(keyer, || mint(&lab));
    let work_token = resolved(&world.run(None, &[]), &work);
    assert!(matches!(work_token.decision, Decision::Token { .. }));
    assert_eq!(
        refusal(&work_token, false),
        None,
        "CONTROL: an unserved token on a login made here runs as typed"
    );

    // A vault writer holds the state: `busy`, labelled and unlabelled.
    let held =
        aterm_secrets::client::test_hold_state_lock(&lab.paths).expect("a pass holds the state");
    for named in [None, Some("away")] {
        refused(&world.run(named, &[]), &away, "busy");
    }
    falls_back(&world.run(None, &[]), &work, "busy");
    drop(held);
}

/// THE VAULT: the lab's token decides `Token` for the account its record names, for `--auto`
/// and the row's label alike; a vault apply holding the state makes `--auto` fall back with
/// `busy` at once.
#[cfg(unix)]
#[test]
fn a_token_in_use_here_is_served_and_a_busy_vault_is_passed_over_at_once() {
    let lab = Lab::new();
    crate::harness::journal::with_keyer(keyer, || mint(&lab));
    let world = World::new();
    let ctx = world.ctx(&[], Some(&lab));
    for named in [None, Some("work"), Some("person@example.com")] {
        let (decision, read, noted) = decided(&world.run(named, &["--continue"]), &ctx);
        let Decision::Token {
            entry: served,
            account,
        } = &decision
        else {
            panic!("{named:?}: {decision:?}");
        };
        assert_eq!((*served, account.named()), (entry(), "work"));
        let read = read.expect("the read");
        assert_eq!(
            read.token().map(|t| t.as_slice().to_vec()),
            Some(sentinel().into_bytes())
        );
        assert_eq!(noted, None);
    }
    let held =
        aterm_secrets::client::test_hold_state_lock(&lab.paths).expect("a pass holds the state");
    let began = Instant::now();
    let (decision, read, noted) = decided(&world.run(None, &[]), &ctx);
    assert_eq!(decision, Decision::Stored(Fallback::Busy));
    assert!(read.is_none());
    assert_eq!(noted, Some((entry(), "busy")));
    assert!(
        began.elapsed() < LABELLED_WAIT,
        "--auto waits for no vault writer"
    );
    drop(held);
}

/// THE HOLD AT THE WRAPPER (§5.6): the lab's token, minted here with nothing compared yet, is held
/// while the row's stored login works here, the roster saying which login that is: the launch
/// stays on it (`awaiting-basis`, journaled), and nothing is read to serve. CONTROL: with no login
/// record of the row's account here, the same token is served.
#[cfg(unix)]
#[test]
fn a_token_awaiting_its_basis_is_held_at_the_wrapper_while_the_stored_login_works() {
    let lab = Lab::new();
    crate::harness::journal::with_keyer(keyer, || mint(&lab));
    let world = World::new();
    let ctx = world.ctx(&[], Some(&lab));
    let (decision, _, _) = decided(&world.run(Some("work"), &[]), &ctx);
    assert!(
        matches!(decision, Decision::Token { .. }),
        "CONTROL: {decision:?}"
    );
    // The vault is the user home's, so the row's login record is read beside it.
    std::fs::write(
        lab.paths.home.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"person@example.com"}}"#,
    )
    .expect("the login record");
    let (decision, read, noted) = decided(&world.run(Some("work"), &[]), &ctx);
    assert_eq!(decision, Decision::Stored(Fallback::AwaitingBasis));
    assert!(read.is_none(), "nothing read to serve");
    assert_eq!(noted, Some((entry(), "awaiting-basis")));
}

/// The variable the wrapper stand-in's parent sets: where the reader writes what it saw.
#[cfg(target_os = "macos")]
const REPORT: &str = "ATERM_TEST_TOKEN_RUN_REPORT";

/// The `minted_at` of the token the wrapper stand-in serves.
#[cfg(target_os = "macos")]
const SERVED_MINTED: u64 = 1_790_000_000;

/// The stand-in for the wrapper: serves the sentinel to the reader stand-in (this test binary
/// again), as `token-run` does once the judgment said `Token`.
#[cfg(target_os = "macos")]
#[test]
fn token_wrapper_stand_in() {
    if std::env::var_os(REPORT).is_none() || std::env::var_os("ATERM_TEST_TOKEN_RUN_WRAP").is_none()
    {
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let args = os(&[
        "harness::accounts_token_run::tests::token_reader_stand_in",
        "--exact",
        "--nocapture",
        "--test-threads=1",
        "--quiet",
    ]);
    let token = aterm_secrets::SecretBytes::from(sentinel());
    let error = serve(exe.as_os_str(), &args, &token, (entry(), SERVED_MINTED));
    panic!("the exec failed: {error}");
}

/// The stand-in Claude Code: reads its descriptor to the newline, and writes whether it held
/// the sentinel, the marker, the token it names and every credential variable it saw.
#[cfg(target_os = "macos")]
#[test]
fn token_reader_stand_in() {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;
    let Some(report) = std::env::var_os(REPORT) else {
        return;
    };
    let number: i32 = std::env::var(token_route::DESCRIPTOR_ENV)
        .expect("the descriptor variable")
        .parse()
        .expect("a descriptor number");
    // SAFETY: the wrapper placed this descriptor in this process for it alone (its number is in
    // DESCRIPTOR_ENV); nothing else here opened, owns or closes it, and the File takes it.
    let mut held = unsafe { std::fs::File::from_raw_fd(number) };
    let mut line = Vec::new();
    held.read_to_end(&mut line)
        .expect("the token's descriptor reads");
    let token = if line == format!("{}\n", sentinel()).into_bytes() {
        "same"
    } else {
        "other"
    };
    let seen: Vec<&str> = accounts::CREDENTIAL_KEYS
        .iter()
        .copied()
        .filter(|name| *name != token_route::DESCRIPTOR_ENV && std::env::var_os(name).is_some())
        .collect();
    let marker = std::env::var(token_route::MARKER_ENV).unwrap_or_default();
    let minted = std::env::var(token_route::MINTED_ENV).unwrap_or_default();
    std::fs::write(
        report,
        format!(
            "token={token}\nmarker={marker}\nminted={minted}\ncredentials={}\n",
            seen.join(",")
        ),
    )
    .expect("the report");
}

/// SERVING (§5.3): the program the wrapper execs reads the token from the descriptor its
/// variable numbers, to the newline; it sees the marker naming the entry and the served token's
/// `minted_at` beside it, and no credential variable (the empty ones, and a stale marker and
/// `minted_at` inherited, are gone).
#[cfg(target_os = "macos")]
#[test]
fn the_exec_d_program_reads_the_token_from_its_descriptor() {
    let world = World::new();
    let report = world.path().join("report");
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "harness::accounts_token_run::tests::token_wrapper_stand_in",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "--quiet",
        ])
        .env(REPORT, &report)
        .env("ATERM_TEST_TOKEN_RUN_WRAP", "1")
        .env("ANTHROPIC_API_KEY", "")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "")
        .env(
            token_route::MARKER_ENV,
            "aterm-claude-token-ac-0000000000000000",
        )
        .env(token_route::MINTED_ENV, "1");
    let out = aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
        .expect("the wrapper ran");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for stream in [&out.stdout, &out.stderr] {
        assert!(!String::from_utf8_lossy(stream).contains(&sentinel()[..24]));
    }
    let said = std::fs::read_to_string(&report).expect("the reader's report");
    assert_eq!(
        said,
        format!(
            "token=same\nmarker={}\nminted={SERVED_MINTED}\ncredentials=\n",
            entry().name()
        )
    );
}
