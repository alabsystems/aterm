// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `accounts --cli codex`'s `next:` says what a launch would take: on a Codex program that keeps
//! no row's login its own, only the row on this machine's own login; and under
//! `[harness] accounts = "off"`, for every CLI, what rotation off leaves a launch. The report,
//! `accounts login|renew` and `accounts use` judge the program a launch runs; `accounts use` of
//! every CLI but Claude Code says what a launch does under the pin, or once it ends.

use super::*;
use std::time::Duration;

#[path = "accounts_step_law_tests.rs"]
mod step_law;
#[path = "accounts_walk_tests.rs"]
mod walk;

/// A host whose one answer is the Codex program a worker aterm starts types here.
struct CodexProgram(Option<PathBuf>);

impl AccountsHost for CodexProgram {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no Claude probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn codex_launch_program(&self) -> Option<PathBuf> {
        self.0.clone()
    }
}

/// A pool of `cli` of `main` (this machine's own login, signed in) and `work` (a login of its
/// own, made), both usable, beside a codex-alab from before `CODEX_AUTH_PATH`'s login (`old`)
/// and one that keeps it (`new`).
struct Pool {
    cli: Cli,
    tmp: Tmp,
    env: Env,
    old: PathBuf,
    new: PathBuf,
    laid: std::cell::RefCell<Option<LaidRecords>>,
}

/// The records as the last [`Pool::lay`]'s commit left them, for laying that world again. One slot:
/// another world, or another `now`, commits for itself.
struct LaidRecords {
    world: LawWorld,
    now: i64,
    state: Vec<u8>,
    logins: Vec<u8>,
}

impl Pool {
    /// A Codex pool.
    fn new(tag: &str) -> Pool {
        Pool::of(Cli::Codex, tag)
    }

    fn of(cli: Cli, tag: &str) -> Pool {
        let tmp = tmp_dir(tag);
        let env = env_at(&tmp);
        let home = env.home.clone().expect("a home");
        let store = cli.connector().stores.first().copied().unwrap_or("login");
        let default = home.join(cli.accounts().default_home).join(store);
        std::fs::create_dir_all(default.parent().expect("parent")).expect("default home");
        std::fs::write(&default, "{}").expect("stand-in default login");
        let work = tmp.path().join("logins").join(cli.id()).join("work");
        std::fs::create_dir_all(work.join(store).parent().expect("parent")).expect("work home");
        std::fs::write(work.join(store), "{}").expect("stand-in work login");
        // Kimi Code's rows name the account system their email belongs to.
        let vendor = accounts::Vendor {
            region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
            ..accounts::Vendor::default()
        };
        let text = accounts::append_rows_for(
            cli,
            "",
            &[
                accounts::Row {
                    label: "main".into(),
                    email: Some("owner@example.com".into()),
                    vendor,
                    ..accounts::Row::default()
                },
                accounts::Row {
                    label: "work".into(),
                    dir: Some(work.display().to_string()),
                    vendor,
                    ..accounts::Row::default()
                },
            ],
        )
        .expect("the rows admit");
        std::fs::write(env.accounts_roster_for(cli).expect("a roster"), text).expect("roster");
        let old = crate::harness::codex_alab::lay_package(&tmp.path().join("store/old"), false);
        let new = crate::harness::codex_alab::lay_package(&tmp.path().join("store/new"), true);
        Pool {
            cli,
            tmp,
            env,
            old,
            new,
            laid: std::cell::RefCell::default(),
        }
    }

    /// `[harness] accounts = "<mode>"` in the `aterm.toml` beside the rosters.
    fn mode(&self, mode: &str) {
        self.config(&format!("[harness]\naccounts = \"{mode}\"\n"));
    }

    /// `text` as the `aterm.toml` beside the rosters.
    fn config(&self, text: &str) {
        aterm_uds::atomic_file::write(
            self.env.config.as_deref().expect("aterm.toml"),
            text.as_bytes(),
            0o600,
        )
        .expect("aterm.toml");
    }

    /// Codex's login kept in the OS keyring (`cli_auth_credentials_store` in its default home's
    /// `config.toml`), which only a Codex pool reads.
    fn keyring(&self) {
        let codex = self
            .env
            .home
            .as_deref()
            .expect("a home")
            .join(Cli::Codex.accounts().default_home);
        std::fs::create_dir_all(&codex).expect("Codex's default home");
        std::fs::write(
            codex.join("config.toml"),
            "cli_auth_credentials_store = \"keyring\"\n",
        )
        .expect("config.toml");
    }

    /// `label` at its five-hour limit until `reset`; the instant the pool takes it again.
    fn wall(&self, label: &str, reset: i64) -> i64 {
        let now = self.env.now;
        let store = self.env.accounts_store_for(self.cli).expect("a store");
        store
            .transact(now, &mut None, |state| {
                Ok(accounts::note_wall(
                    state,
                    label,
                    "five_hour",
                    Some(reset),
                    now - 60,
                    now,
                ))
            })
            .expect("a wall");
        store.load(now).expect("state").0.accounts[label]
            .limited_until(now)
            .expect("walled")
    }

    fn pin(&self, label: &str) {
        let store = self.env.accounts_store_for(self.cli).expect("a store");
        store
            .transact(self.env.now, &mut None, |state| {
                state.active = Some(label.to_string());
                state.pinned = true;
                Ok(Vec::new())
            })
            .expect("a pin");
    }

    /// The text report and its JSON twin, with `program` as the Codex a worker aterm starts types.
    fn report(&self, program: Option<&Path>) -> (String, Value) {
        self.report_on(&CodexProgram(program.map(Path::to_path_buf)))
    }

    /// The text report and its JSON twin, as `host` answers.
    fn report_on(&self, host: &dyn AccountsHost) -> (String, Value) {
        let run = |line: &str| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = match parsed(line).expect(line) {
                Cmd::Accounts(form) => run_accounts(&form, &self.env, host, &mut out, &mut err),
                Cmd::AccountsOf(of) => run_accounts_of(&of, &self.env, host, &mut out, &mut err),
                _ => panic!("an accounts report"),
            };
            assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
            String::from_utf8(out).expect("utf-8")
        };
        let line = format!("accounts --cli {}", self.cli.id());
        let text = run(&line);
        let json = aterm_json::from_str(run(&format!("{line} --json")).trim()).expect("JSON");
        (text, json)
    }

    /// The pools' seat a launch reads.
    fn seat(&self) -> crate::harness::accounts_pool::Seat<'_> {
        crate::harness::accounts_pool::Seat {
            config_dir: self.tmp.path(),
            state: &self.env.state,
            home: self.env.home.as_deref().expect("a home"),
            now: self.env.now,
        }
    }

    /// When a launch on `program` refused for its pool may start
    /// (`accounts_pool::launch_opening`).
    fn opening(&self, program: &Path) -> Option<i64> {
        self.opening_on(Some(program))
    }

    /// [`Pool::opening`] on `program`, `None` where it is not known.
    fn opening_on(&self, program: Option<&Path>) -> Option<i64> {
        crate::harness::accounts_pool::launch_opening(&self.seat(), self.cli, program)
    }

    /// The row a launch on `program` takes, reading no usage (`accounts_pool::launch_env_for`).
    fn launch(&self, program: &Path) -> Result<Option<String>, String> {
        self.launch_on(Some(program))
    }

    /// [`Pool::launch`] on `program`, `None` where it is not known.
    fn launch_on(&self, program: Option<&Path>) -> Result<Option<String>, String> {
        let unread = |_: Cli, _: &Path, _: Option<&str>, _: i64| None;
        crate::harness::accounts_pool::launch_env_for(&self.seat(), self.cli, &unread, program)
            .map(|env| env.map(|env| env.label))
    }

    /// The roster replaced by `rows`.
    fn rows(&self, rows: &[accounts::Row]) {
        let text = accounts::append_rows_for(self.cli, "", rows).expect("the rows admit");
        std::fs::write(
            self.env.accounts_roster_for(self.cli).expect("a roster"),
            text,
        )
        .expect("roster");
    }

    /// Record an explicit current rejection, with real synthetic credentials for Codex.
    fn signed_out(&self, label: &str) {
        if self.cli == Cli::Codex {
            crate::ai::testing::current_codex_auth(
                &self.env.accounts_store_for(self.cli).unwrap(),
                &self.env.accounts_roster_for(self.cli).unwrap(),
                label,
                accounts::Auth::SignedOut,
                self.env.now,
            );
        } else {
            self.state(|state| {
                accounts::note_auth(state, label, accounts::Auth::SignedOut, self.env.now);
            });
        }
    }

    /// `change` applied to the pool's state.
    fn state(&self, change: impl FnOnce(&mut accounts::State)) {
        let store = self.env.accounts_store_for(self.cli).expect("a store");
        store
            .transact(self.env.now, &mut None, |state| {
                change(state);
                Ok(Vec::new())
            })
            .expect("state");
    }
}

/// A named state of a [`Pool`], and how to put the pool in it.
type Case = (&'static str, fn(&Pool));

/// The text's `next:` line.
fn next_line(text: &str) -> &str {
    text.lines()
        .find(|line| line.starts_with("next: "))
        .unwrap_or_else(|| panic!("a next line: {text}"))
}

/// The text's `not next:` rows, label to reason.
fn not_next(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .find_map(|line| line.strip_prefix("not next: "))
        .map(|rows| {
            rows.split("; ")
                .map(|row| {
                    let (label, why) = row.split_once(": ").expect("label: why");
                    (label.to_string(), why.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `rows` with each reason cut to the word it opens with: what follows it in `not next:` is for a
/// person, and `next.not[].why` is that word alone.
fn reason_words(rows: BTreeMap<String, String>) -> BTreeMap<String, String> {
    rows.into_iter()
        .map(|(label, why)| {
            let word = why.split(" (").next().unwrap_or(&why).to_string();
            (label, word)
        })
        .collect()
}

/// The JSON's `next.not` rows, label to reason.
fn not_next_json(doc: &Value) -> BTreeMap<String, String> {
    doc["next"]["not"]
        .as_array()
        .expect("next.not")
        .iter()
        .map(|row| {
            (
                row["label"].as_str().expect("label").to_string(),
                row["why"].as_str().expect("why").to_string(),
            )
        })
        .collect()
}

/// ON A CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN, `next:` IS THE ROW ON THIS MACHINE'S OWN LOGIN:
/// with `main` (the account in use) and `work` both usable, plain rotation would name `work`,
/// which that build would run on `main`'s login; the report names `main`, and `work` is not next
/// for the build, with `aterm pkg update codex` as its way back. The JSON says the same, and
/// names the program. A pin on `work`, which such a build refuses, leaves no launch to run:
/// `next: none`, its reason the JSON's.
#[test]
fn an_old_codex_reports_the_default_row_next_and_every_row_of_its_own_not_next() {
    let pool = Pool::new("codex-next-old");
    let (text, doc) = pool.report(Some(&pool.old));
    assert_eq!(next_line(&text), "next: main", "{text}");
    let not = not_next(&text);
    assert_eq!(not.keys().collect::<Vec<_>>(), ["work"], "{text}");
    assert!(not["work"].contains("`aterm pkg update codex`"), "{text}");
    assert_eq!(doc["next"]["to"], Value::from("main"), "{doc}");
    assert_eq!(doc["next"]["wait_until"], Value::Null);
    assert_eq!(not_next_json(&doc), not, "JSON matches text");
    let build = doc["next"]["only_default"]
        .as_str()
        .expect("the build's reason");
    assert!(build.contains(&pool.old.display().to_string()), "{build}");
    assert!(text.contains(build), "{text}");

    pool.pin("work");
    let (text, doc) = pool.report(Some(&pool.old));
    let none = doc["next"]["none"].as_str().expect("why none runs");
    assert_eq!(next_line(&text), format!("next: none ({none})"), "{text}");
    assert!(
        !text.contains("list another"),
        "another row is no help: {text}"
    );
    assert!(
        none.contains("aterm harness accounts use main --cli codex"),
        "{none}"
    );
    assert_eq!(doc["next"]["to"], Value::Null);
    assert_eq!(not_next_json(&doc), not_next(&text), "JSON matches text");
}

/// WITH `main` WALLED, `next:` WAITS FOR `main`'S WALL, NOT FOR `work`: with `work` usable
/// (plain rotation would name it) and with `work` walled to an earlier reset (plain rotation
/// would wait for it), the wait is `main`'s, the instant a refused launch waits for
/// (`accounts_pool::launch_opening`); `main` is not next for its own wall.
#[test]
fn an_old_codex_with_its_default_row_walled_waits_for_that_rows_reset() {
    let pool = Pool::new("codex-next-walled");
    let now = pool.env.now;
    let until = pool.wall("main", now + 2 * 3600);
    for work_walled in [false, true] {
        if work_walled {
            pool.wall("work", now + 3600);
        }
        let (text, doc) = pool.report(Some(&pool.old));
        let wait = format!("next: wait until {} (in ", rfc3339_utc(until));
        assert!(next_line(&text).starts_with(&wait), "{work_walled}: {text}");
        let not = not_next(&text);
        assert!(
            not["main"].starts_with(&format!("limited until {}", rfc3339_utc(until))),
            "{work_walled}: {text}"
        );
        assert!(not["work"].contains("`aterm pkg update codex`"), "{text}");
        assert_eq!(doc["next"]["to"], Value::Null, "{doc}");
        assert_eq!(doc["next"]["wait_until"].as_i64(), Some(until), "{doc}");
        assert_eq!(not_next_json(&doc), not, "JSON matches text");
        assert_eq!(
            pool.opening(&pool.old),
            Some(until),
            "a launch waits as long"
        );
    }
}

/// CONTROL: ON A CODEX THAT KEEPS `CODEX_AUTH_PATH`'S LOGIN, THE REPORT IS PLAIN ROTATION'S, BYTE
/// FOR BYTE AS WITH NO PROGRAM KNOWN: with no row active, `next: main`, the first usable row, and
/// `work` once `main` is walled, with no word of the build in text or JSON.
#[test]
fn a_codex_that_keeps_codex_auth_path_reports_plain_rotation() {
    let pool = Pool::new("codex-next-new");
    for (walled, want) in [(false, "main"), (true, "work")] {
        if walled {
            pool.wall("main", pool.env.now + 2 * 3600);
        }
        let (text, doc) = pool.report(Some(&pool.new));
        assert_eq!(
            next_line(&text),
            format!("next: {want}"),
            "{walled}: {text}"
        );
        assert_eq!(doc["next"]["to"], Value::from(want));
        assert!(doc["next"].get("only_default").is_none(), "{doc}");
        assert_eq!(not_next_json(&doc), not_next(&text), "JSON matches text");
        let (plain_text, plain_doc) = pool.report(None);
        assert_eq!(text, plain_text);
        assert_eq!(doc, plain_doc);
    }
}

/// ON A CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN, THE REPORT SAYS WHAT A LAUNCH DOES IN EVERY
/// CONDITION OF THE ROW ON THIS MACHINE'S OWN LOGIN: where a launch takes a row, `next:` names
/// it; where it is refused, `next:` waits as long as `accounts_pool::launch_opening` says a
/// refused launch waits, else says `none` with the JSON's reason, and offers no other row to
/// list. Rotation off is [`off_report_agrees_with_a_launch`]'s, a keyring login
/// [`a_keyring_login_takes_no_row_in_any_mode_and_the_report_says_so`]'s. Left out, since the
/// report does not say it yet: with rotation on, a pin that names no row of the roster, which
/// refuses every launch.
#[test]
fn an_old_codex_report_agrees_with_its_launch_in_every_default_row_condition() {
    fn main() -> accounts::Row {
        accounts::Row {
            label: "main".into(),
            email: Some("owner@example.com".into()),
            ..accounts::Row::default()
        }
    }
    fn work(pool: &Pool) -> accounts::Row {
        accounts::Row {
            label: "work".into(),
            dir: Some(
                pool.tmp
                    .path()
                    .join("logins/codex/work")
                    .display()
                    .to_string(),
            ),
            ..accounts::Row::default()
        }
    }
    let cases: [Case; 10] = [
        ("walled", |pool| {
            pool.wall("main", pool.env.now + 7200);
        }),
        ("signed out", |pool| {
            pool.signed_out("main");
        }),
        ("not signed in here", |pool| {
            let home = pool.env.home.as_deref().expect("a home");
            std::fs::remove_file(
                home.join(Cli::Codex.accounts().default_home)
                    .join("auth.json"),
            )
            .expect("the default login");
        }),
        ("held for another Mac", |pool| {
            pool.rows(&[
                accounts::Row {
                    held_by: Some("other-mac".into()),
                    ..main()
                },
                work(pool),
            ]);
        }),
        ("claimed elsewhere", |pool| {
            let now = pool.env.now;
            pool.state(|state| state.note_lease("main", "other-mac", now + 3600, now));
        }),
        ("claims unrecorded", |pool| {
            pool.state(|state| state.unrecorded = true);
        }),
        ("unlisted", |pool| pool.rows(&[work(pool)])),
        ("moved", |pool| {
            let now = pool.env.now;
            let staging = pool.tmp.path().join("logins/codex/main+1");
            std::fs::create_dir_all(&staging).expect("staging");
            std::fs::write(staging.join("auth.json"), "{}").expect("stand-in login");
            pool.env
                .accounts_store_for(Cli::Codex)
                .expect("a store")
                .transact_logins(now, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "main",
                        None,
                        &staging.display().to_string(),
                        None,
                        now,
                    ))
                })
                .expect("moved onto a login of its own");
        }),
        ("pin on a row of its own while walled", |pool| {
            pool.wall("main", pool.env.now + 7200);
            pool.pin("work");
        }),
        ("pinned while walled", |pool| {
            pool.wall("main", pool.env.now + 7200);
            pool.pin("main");
        }),
    ];
    for (case, set) in cases {
        let pool = Pool::new("codex-next-agree");
        set(&pool);
        let (text, doc) = pool.report(Some(&pool.old));
        let opening = pool.opening(&pool.old);
        let launch = pool.launch(&pool.old);
        let next = next_line(&text);
        assert_eq!(
            not_next_json(&doc),
            not_next(&text),
            "{case}: JSON matches text"
        );
        match launch {
            Ok(Some(row)) => {
                assert_eq!(next, format!("next: {row}"), "{case}: {text}");
                assert_eq!(doc["next"]["to"], Value::from(row), "{case}: {doc}");
            }
            Ok(None) => panic!("{case}: a launch that takes no row: {text}"),
            Err(refused) => {
                assert_eq!(doc["next"]["to"], Value::Null, "{case} ({refused}): {doc}");
                assert_eq!(
                    doc["next"]["wait_until"].as_i64(),
                    opening,
                    "{case} ({refused}): {doc}"
                );
                match opening {
                    Some(until) => assert!(
                        next.starts_with(&format!("next: wait until {}", rfc3339_utc(until))),
                        "{case} ({refused}): {text}"
                    ),
                    None => {
                        let none = doc["next"]["none"].as_str().expect("why none runs");
                        assert_eq!(next, format!("next: none ({none})"), "{case}: {text}");
                    }
                }
                assert!(
                    !text.contains("list another"),
                    "{case}: another row is no help: {text}"
                );
            }
        }
    }
}

/// The text's `rotation off:` line, where it has one.
fn off_line(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.strip_prefix("rotation off: "))
}

/// The states of the pool under rotation off a launch is judged in: `main` (this machine's own
/// login) and `work` (a login of its own) usable with no pin, `main` walled with none (rotation
/// off reads no wall), the pin on either row, on `work` with no login of it here, and on a label
/// the roster does not list; and the pin on `work` or on a label not listed with Codex's login
/// kept in the OS keyring ([`Pool::keyring`]).
fn off_cases() -> [Case; 8] {
    [
        ("no pin", |_| {}),
        ("no pin, main walled", |pool| {
            pool.wall("main", pool.env.now + 7200);
        }),
        ("pin on work", |pool| pool.pin("work")),
        ("pin on main", |pool| pool.pin("main")),
        ("pin on work, not signed in here", |pool| {
            pool.pin("work");
            let work = pool
                .tmp
                .path()
                .join("logins")
                .join(pool.cli.id())
                .join("work");
            std::fs::remove_dir_all(&work).expect("work's login");
            std::fs::create_dir_all(&work).expect("an empty home");
        }),
        ("pin on a label not listed", |pool| pool.pin("gone")),
        ("keyring, pin on work", |pool| {
            pool.keyring();
            pool.pin("work");
        }),
        ("keyring, pin on a label not listed", |pool| {
            pool.keyring();
            pool.pin("gone");
        }),
    ]
}

/// UNDER `[harness] accounts = "off"`, THE REPORT SAYS WHAT A LAUNCH DOES, FOR CODEX, GROK AND
/// KIMI, CODEX ON EVERY PROGRAM: in each of [`off_cases`], with `work` usable (plain rotation would name it, or
/// on a Codex that keeps no row's login its own, `main`), `next:` names the row
/// `accounts_pool::launch_env_for` takes (the pin), else says `none` with the JSON's reason: the
/// launch takes no row and runs on its starter's login, which names `main` (so too with Codex's
/// login in the OS keyring, the pin included), or the launch's own refusal. No launch waits on the pool then, nor does the report; no row is weighed, so none is
/// `not next:`; no other row is offered to list; and a `rotation off:` line, the JSON's `off`,
/// says so. Claude Code's is [`off_report_of_claude_code_moves_no_session`].
#[test]
fn off_report_agrees_with_a_launch() {
    let mut seen = BTreeSet::new();
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        // No program known, then (Codex) one that keeps no row's login its own, and one that does.
        let programs: &[Option<bool>] = match cli {
            Cli::Codex => &[None, Some(true), Some(false)],
            _ => &[None],
        };
        for (case, set) in off_cases() {
            for old in programs {
                let pool = Pool::of(cli, "off-agree");
                let program = old.map(|old| {
                    if old {
                        pool.old.clone()
                    } else {
                        pool.new.clone()
                    }
                });
                pool.mode("off");
                set(&pool);
                let at = format!("{} on {program:?}, {case}", cli.id());
                let (text, doc) = pool.report(program.as_deref());
                let launch = pool.launch_on(program.as_deref());
                let next = next_line(&text);
                let none = doc["next"]["none"].as_str();
                seen.insert(match &launch {
                    Ok(Some(_)) => "a row",
                    Ok(None) => "no row",
                    Err(_) => "refused",
                });
                match &launch {
                    Ok(Some(row)) => {
                        assert_eq!(next, format!("next: {row}"), "{at}: {text}");
                        assert_eq!(doc["next"]["to"], Value::from(row.clone()), "{at}: {doc}");
                        assert_eq!(doc["next"]["none"], Value::Null, "{at}: {doc}");
                    }
                    Ok(None) => {
                        let none = none.expect("why no row is next");
                        assert_eq!(doc["next"]["to"], Value::Null, "{at}: {doc}");
                        assert_eq!(next, format!("next: none ({none})"), "{at}: {text}");
                        assert!(
                            none.contains("`main`"),
                            "{at}: the login it runs on: {none}"
                        );
                    }
                    Err(refused) => {
                        let none = none.expect("why no row is next");
                        assert_eq!(doc["next"]["to"], Value::Null, "{at}: {doc}");
                        assert_eq!(next, format!("next: none ({none})"), "{at}: {text}");
                        assert!(none.contains(refused.as_str()), "{at}: {none}");
                    }
                }
                assert_eq!(doc["next"]["wait_until"], Value::Null, "{at}: {doc}");
                assert_eq!(
                    pool.opening_on(program.as_deref()),
                    None,
                    "{at}: no launch waits"
                );
                assert!(not_next(&text).is_empty(), "{at}: {text}");
                assert!(not_next_json(&doc).is_empty(), "{at}: {doc}");
                let off = doc["next"]["off"].as_str().expect("rotation off's words");
                assert_eq!(off_line(&text), Some(off), "{at}: {text}");
                assert!(!text.contains("list another"), "{at}: {text}");
                assert!(doc["next"].get("only_default").is_none(), "{at}: {doc}");
            }
        }
    }
    assert_eq!(
        seen.len(),
        3,
        "a launch took a row, took none and was refused"
    );
}

/// UNDER `[harness] accounts = "off"`, CLAUDE CODE'S REPORT NAMES NO ROW NEXT: the supervisor
/// moves no session then, and a window's Claude Code worker takes no pick of the pool's (its
/// starter's login is kept), so in each of [`off_cases`] `next:` is `none`, its reason the
/// JSON's, naming the login a plain start runs on (`main`'s) and, where a pin stands, the pin it
/// moves nothing onto. With no pin that is what `accounts_pool::launch_env_for` says too: it takes
/// no row. No row is `not next:`, none is offered to list, and the `rotation off:` line is the
/// JSON's `off`.
#[test]
fn off_report_of_claude_code_moves_no_session() {
    for (case, set) in off_cases() {
        let pool = Pool::of(Cli::Claude, "off-claude");
        pool.mode("off");
        set(&pool);
        let (text, doc) = pool.report(None);
        let none = doc["next"]["none"].as_str().expect("why no row is next");
        assert_eq!(doc["next"]["to"], Value::Null, "{case}: {doc}");
        assert_eq!(
            next_line(&text),
            format!("next: none ({none})"),
            "{case}: {text}"
        );
        assert!(
            none.contains("`main`"),
            "{case}: the login it runs on: {none}"
        );
        let state = pool
            .env
            .accounts_store()
            .load(pool.env.now)
            .expect("state")
            .0;
        match state.active.as_deref().filter(|_| state.pinned) {
            Some(pin) => assert!(none.contains(&format!("`{pin}`")), "{case}: {none}"),
            None => assert_eq!(pool.launch_on(None), Ok(None), "{case}: no row taken"),
        }
        assert_eq!(doc["next"]["wait_until"], Value::Null, "{case}: {doc}");
        assert!(not_next(&text).is_empty(), "{case}: {text}");
        assert!(not_next_json(&doc).is_empty(), "{case}: {doc}");
        let off = doc["next"]["off"].as_str().expect("rotation off's words");
        assert_eq!(off_line(&text), Some(off), "{case}: {text}");
        assert!(!text.contains("list another"), "{case}: {text}");
    }
}

/// CONTROL: WITH ROTATION ON, THE REPORT IS AS IT WAS WITH NO `aterm.toml`: under `early` and
/// `wall`, `next: work` for Claude Code (whose session on this machine's own login, `main`,
/// rotates off it), and `next: main` for a Codex that keeps `CODEX_AUTH_PATH`'s login (no row is
/// active, so a launch takes the first usable row) and for one that keeps none; the text byte for
/// byte up to Claude Code's outlook (which replays the rotation under the mode it names, so
/// `wall`'s says `rotation wall`), the JSON's `next` too (and, under
/// `early`, the default, all of the JSON); no `rotation off:` line, and the JSON's `next` holds
/// the keys it held before rotation off had words (`to`, `wait_until` with its RFC 3339 twin,
/// `not`, and on a Codex that keeps no row's login its own `only_default` and `none`), no `off`.
/// Claude Code's also holds `worker`, since its `to` is a rotation's.
#[test]
fn rotation_on_reports_as_before() {
    for (cli, program, want) in [
        (Cli::Claude, None, "next: work"),
        (Cli::Codex, Some(false), "next: main"),
        (Cli::Codex, Some(true), "next: main"),
    ] {
        for mode in ["early", "wall"] {
            let pool = Pool::of(cli, "off-control");
            let program = program.map(|old| {
                if old {
                    pool.old.clone()
                } else {
                    pool.new.clone()
                }
            });
            let (plain_text, plain_doc) = pool.report(program.as_deref());
            pool.mode(mode);
            let (text, doc) = pool.report(program.as_deref());
            let at = format!("{} under {mode} on {program:?}", cli.id());
            assert_eq!(next_line(&text), want, "{at}: {text}");
            assert_eq!(off_line(&text), None, "{at}: {text}");
            let mut keys: Vec<&str> = doc["next"]
                .as_object()
                .expect("next")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            // What a weekly limit moves is its own law's ([`Pool::law_wall`]).
            keys.retain(|key| *key != "wall_moves");
            let only_default = matches!(program, Some(ref p) if *p == pool.old);
            let want_keys: &[&str] = if only_default {
                &[
                    "none",
                    "not",
                    "only_default",
                    "to",
                    "wait_until",
                    "wait_until_rfc3339",
                ]
            } else if cli == Cli::Claude {
                // Claude Code's `to` is a rotation's, so what a worker takes is said apart.
                &["not", "to", "wait_until", "wait_until_rfc3339", "worker"]
            } else {
                &["not", "to", "wait_until", "wait_until_rfc3339"]
            };
            assert_eq!(keys, want_keys, "{at}: {doc}");
            // The outlook replays under the mode it names (the outlook's design, §9).
            let rows = |text: &str| {
                text.split("\noutlook (")
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            };
            assert_eq!(rows(&text), rows(&plain_text), "{at}");
            if mode == "early" {
                assert_eq!(text, plain_text, "{at}");
            }
            assert_eq!(doc["next"], plain_doc["next"], "{at}");
            if mode == "early" {
                assert_eq!(doc, plain_doc, "{at}");
            }
        }
    }
}

/// THE `rotation off:` LINE NAMES WHAT IN `aterm.toml` TURNS ROTATION OFF, AND ONLY THAT:
/// `[harness] accounts` as TOML reads it (`"off"`, or `false`, which reads as off), and only
/// then that `"early"` there turns it back on; `[swarm] enabled = false`, which turns it off over
/// `[harness] accounts = "early"`; or a `harness.accounts` filed below another table's header,
/// which limits all the same. Beside `[harness] accounts = "off"`, a `[swarm] enabled = true` and
/// a `harness.accounts = "early"` filed below another table's header turn nothing off, and the
/// line names neither. The JSON's `off` is the line.
#[test]
fn the_rotation_off_line_names_what_turns_rotation_off() {
    for (toml, named, root, unnamed) in [
        (
            "[harness]\naccounts = \"early\"\n[swarm]\nenabled = false\n",
            "`[swarm] enabled = false`",
            false,
            None,
        ),
        (
            "[theme]\nharness.accounts = \"off\"\n",
            "`theme.harness.accounts = \"off\"`",
            false,
            None,
        ),
        (
            "[harness]\naccounts = \"off\"\n",
            "`[harness] accounts = \"off\"`",
            true,
            None,
        ),
        (
            "[harness]\naccounts = false\n",
            "`[harness] accounts = false`",
            true,
            None,
        ),
        (
            "[harness]\naccounts = \"off\"\n[swarm]\nenabled = true\n",
            "`[harness] accounts = \"off\"`",
            true,
            Some("[swarm]"),
        ),
        (
            "[harness]\naccounts = \"off\"\n[theme]\nharness.accounts = \"early\"\n",
            "`[harness] accounts = \"off\"`",
            true,
            Some("theme.harness.accounts"),
        ),
    ] {
        let pool = Pool::new("off-by");
        pool.config(toml);
        assert_eq!(
            pool.seat().mode_for(pool.cli),
            accounts::Mode::Off,
            "{toml}"
        );
        let (text, doc) = pool.report(None);
        let off = off_line(&text).unwrap_or_else(|| panic!("{toml}: {text}"));
        assert!(off.contains(named), "{toml}: {off}");
        assert_eq!(off.contains("`[harness] accounts"), root, "{toml}: {off}");
        assert_eq!(off.contains("`\"early\"`"), root, "{toml}: {off}");
        if let Some(unnamed) = unnamed {
            assert!(
                !off.contains(unnamed),
                "{toml}: names what turns nothing off: {off}"
            );
        }
        assert_eq!(doc["next"]["off"].as_str(), Some(off), "{toml}: {doc}");
    }
}

/// AN `aterm.toml` THAT IS NOT VALID TOML, WHOSE LINES, READ ONE BY ONE, TURN ROTATION OFF, IS
/// SAID TO BE SO ON THE `rotation off:` LINE, WITH THE PARSER'S REASON, WHERE NO SETTING CAN BE
/// NAMED: `[harness] accounts = "off"` above a broken header, and `[theme] swarm = false` above
/// one (which a valid file reads as turning nothing off). The line offers no `"early"`, since
/// what turns rotation back on is not known until the file is valid. The JSON's `off` is the
/// line. CONTROL: the same `[harness] accounts = "off"` in a valid file is named.
#[test]
fn a_rotation_off_line_says_an_invalid_aterm_toml_is_not_valid_toml() {
    for toml in [
        "[harness]\naccounts = \"off\"\n[broken\n",
        "[theme]\nswarm = false\n[broken\n",
    ] {
        let pool = Pool::new("off-invalid");
        pool.config(toml);
        assert_eq!(
            pool.seat().mode_for(pool.cli),
            accounts::Mode::Off,
            "{toml}"
        );
        let why =
            crate::supervise::config::SupervisorConfig::accounts_off_by(toml, pool.cli.program())
                .expect_err("not valid TOML");
        let (text, doc) = pool.report(None);
        let off = off_line(&text).unwrap_or_else(|| panic!("{toml}: {text}"));
        assert!(
            off.starts_with(&format!("aterm.toml is not valid TOML ({why})")),
            "{toml}: {off}"
        );
        assert!(off.contains("read one by one"), "{toml}: {off}");
        assert!(!off.contains("`[harness] accounts"), "{toml}: {off}");
        assert!(!off.contains("`\"early\"`"), "{toml}: {off}");
        assert_eq!(doc["next"]["off"].as_str(), Some(off), "{toml}: {doc}");
    }
    let pool = Pool::new("off-invalid-control");
    pool.mode("off");
    let (text, _) = pool.report(None);
    let off = off_line(&text).expect("a rotation off line");
    assert!(
        off.starts_with("`[harness] accounts = \"off\"` in aterm.toml"),
        "CONTROL: {off}"
    );
    assert!(!off.contains("not valid TOML"), "CONTROL: {off}");
}

/// AN `aterm.toml` ITS READER REFUSES TURNS THE HARNESS OFF WHATEVER IT SAYS, AND THE
/// `rotation off:` LINE SAYS SO, WITH THE REFUSAL, NOT A SETTING IN IT: a file other users may
/// write that says `[harness] accounts = "off"` is not named by that setting, and the line offers
/// no `"early"`, which would change nothing while the file is refused. The JSON's `off` is the
/// line. NEGATIVE CONTROL: the rule that read the file again without the reader's checks named
/// `[harness] accounts = "off"` and said `"early"` there turns rotation back on. CONTROL: the
/// same file, written for its owner alone, is named.
#[cfg(unix)]
#[test]
fn a_rotation_off_line_says_a_refused_aterm_toml_cannot_be_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let toml = "[harness]\naccounts = \"off\"\n";
    let pool = Pool::new("off-refused");
    pool.config(toml);
    let path = pool.env.config.clone().expect("aterm.toml");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    assert_eq!(
        pool.seat().mode_for(pool.cli),
        accounts::Mode::Off,
        "a refused aterm.toml turns the harness off"
    );
    let (text, doc) = pool.report(None);
    let off = off_line(&text).unwrap_or_else(|| panic!("{text}"));
    assert!(
        off.starts_with(
            "aterm.toml cannot be read (config refused: mode 664 lets other users write it"
        ),
        "{off}"
    );
    assert!(off.contains("which turns the harness off"), "{off}");
    assert!(!off.contains("`[harness] accounts"), "{off}");
    assert!(!off.contains("`\"early\"`"), "{off}");
    assert_eq!(doc["next"]["off"].as_str(), Some(off), "{doc}");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let (text, _) = pool.report(None);
    let off = off_line(&text).expect("a rotation off line");
    assert!(
        off.starts_with("`[harness] accounts = \"off\"` in aterm.toml")
            && off.contains("`\"early\"`"),
        "CONTROL: {off}"
    );
}

/// A LOGIN CODEX KEEPS IN THE OS KEYRING TAKES NO ROW IN ANY MODE, AND THE REPORT SAYS SO: with no
/// `aterm.toml`, under `early`, `wall` and `off`, on every program (none known, one that keeps no
/// row's login its own, one that keeps it), with no pin and with the pin on `work` or `main`, a
/// launch takes no row (`accounts_pool::launch_env_for`), and `next: none` says why, the JSON's
/// `none`, naming the keyring and the login a plain launch runs on (`main`'s); no row is `not
/// next:`, none is offered to list, no build's words stand, and the JSON's `to` is null.
#[test]
fn a_keyring_login_takes_no_row_in_any_mode_and_the_report_says_so() {
    for mode in [None, Some("early"), Some("wall"), Some("off")] {
        for program in [None, Some(true), Some(false)] {
            for pin in [None, Some("work"), Some("main")] {
                let pool = Pool::new("keyring-next");
                pool.keyring();
                if let Some(mode) = mode {
                    pool.mode(mode);
                }
                if let Some(pin) = pin {
                    pool.pin(pin);
                }
                let program = program.map(|old| {
                    if old {
                        pool.old.clone()
                    } else {
                        pool.new.clone()
                    }
                });
                let at = format!("{mode:?} on {program:?}, pin {pin:?}");
                let (text, doc) = pool.report(program.as_deref());
                assert_eq!(
                    pool.launch_on(program.as_deref()),
                    Ok(None),
                    "{at}: PRECONDITION: a launch takes no row"
                );
                let none = doc["next"]["none"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{at}: why no row is next: {doc}"));
                assert_eq!(
                    next_line(&text),
                    format!("next: none ({none})"),
                    "{at}: {text}"
                );
                assert!(
                    none.contains("OS keyring") && none.contains("`main`"),
                    "{at}: {none}"
                );
                assert_eq!(doc["next"]["to"], Value::Null, "{at}: {doc}");
                assert_eq!(doc["next"]["wait_until"], Value::Null, "{at}: {doc}");
                assert!(not_next(&text).is_empty(), "{at}: {text}");
                assert!(not_next_json(&doc).is_empty(), "{at}: {doc}");
                assert!(!text.contains("list another"), "{at}: {text}");
                assert!(doc["next"].get("only_default").is_none(), "{at}: {doc}");
            }
        }
    }
}

/// A LAUNCH ON A LOGIN NO ROW LISTS, AS THE REPORT SAYS IT: on a roster of directories alone, with
/// Codex's login in the OS keyring or with rotation off and no pin, a launch takes no row, so
/// `--host any` passes the member over, and `next:` says which and why. A refused launch says so
/// too: a wall on one row and another Mac's hold on the other leave a wait, each row's reason, and
/// no launch's row.
#[test]
fn the_report_says_where_a_launch_a_survey_passes_over_would_run() {
    let dir_rows = |pool: &Pool| {
        let row = |label: &str| {
            let dir = pool.tmp.path().join("logins").join(label);
            let login = dir.join("auth.json");
            std::fs::create_dir_all(&dir).expect("a login's home");
            std::fs::write(&login, "{}").expect("a stand-in login");
            accounts::Row {
                label: label.into(),
                dir: Some(dir.display().to_string()),
                ..accounts::Row::default()
            }
        };
        pool.rows(&[row("first"), row("second")]);
    };
    let lists_none = "this machine's own, which no row of the roster lists for a plain one";
    let passed_over = |pool: &Pool, at: &str| {
        let config = pool.env.config.clone().expect("aterm.toml");
        let places = (
            config.as_path(),
            pool.env.state.as_path(),
            pool.env.home.as_deref(),
        );
        let rows = survey_accounts(Cli::Codex, places, pool.env.now)
            .expect("the pool reads")
            .rows;
        assert!(rows.iter().any(|row| row.enrolled), "{at}: {rows:?}");
        assert!(rows.iter().all(|row| !row.launch), "{at}: {rows:?}");
    };
    let keyring = Pool::new("passed-over-keyring");
    dir_rows(&keyring);
    keyring.keyring();
    let off = Pool::new("passed-over-off");
    dir_rows(&off);
    off.mode("off");
    for (at, pool, why) in [
        ("the keyring", &keyring, "keeps its login in the OS keyring"),
        ("rotation off", &off, "no account is pinned"),
    ] {
        passed_over(pool, at);
        assert_eq!(pool.launch_on(Some(&pool.new)), Ok(None), "{at}");
        let (text, _) = pool.report(Some(&pool.new));
        let next = next_line(&text);
        for said in [why, "takes no row", lists_none] {
            assert!(next.contains(said), "{at}: {said}: {next}");
        }
    }

    let refused = Pool::new("passed-over-refused");
    let walled = refused.wall("main", refused.env.now + 3_600);
    let mut roster = refused.roster();
    for row in &mut roster.rows {
        if row.label == "work" {
            row.held_by = Some("other-mac".into());
        }
    }
    refused.rows(&roster.rows);
    passed_over(&refused, "refused");
    let launch = refused.launch_on(Some(&refused.new));
    assert!(launch.is_err(), "the launch is refused: {launch:?}");
    let (text, _) = refused.report(Some(&refused.new));
    assert!(
        next_line(&text).contains(&rfc3339_utc(walled)),
        "a wait for the wall: {text}"
    );
    let not = not_next(&text);
    assert!(
        not.contains_key("main") && not.contains_key("work"),
        "{text}"
    );
}

/// UNDER ROTATION OFF WITH CODEX'S LOGIN IN THE OS KEYRING, THE `rotation off:` LINE OFFERS NO
/// `"early"` OR `"wall"`, WHICH CHANGES NO LAUNCH THEN (a launch takes no row in every mode): it
/// names the store setting that does. CONTROL: with the login in a file, it offers `"early"`.
#[test]
fn under_a_keyring_login_the_rotation_off_line_names_the_store_not_the_mode() {
    let pool = Pool::new("keyring-off");
    pool.mode("off");
    let (text, _) = pool.report(None);
    let off = off_line(&text).expect("a rotation off line");
    assert!(off.contains("`\"early\"`"), "CONTROL: {off}");
    assert!(
        !off.contains("cli_auth_credentials_store"),
        "CONTROL: {off}"
    );
    pool.keyring();
    let (text, doc) = pool.report(None);
    let off = off_line(&text).expect("a rotation off line");
    assert!(!off.contains("`\"early\"`"), "{off}");
    assert!(
        off.contains("`cli_auth_credentials_store = \"file\"`"),
        "{off}"
    );
    assert_eq!(doc["next"]["off"].as_str(), Some(off), "{doc}");
    pool.mode("early");
    assert_eq!(
        pool.launch_on(None),
        Ok(None),
        "rotation back on changes no launch"
    );
}

/// WITH NO ROW ACTIVE, `next:` IS THE ROW A LAUNCH TAKES, FOR CODEX (ON A PROGRAM THAT KEEPS
/// `CODEX_AUTH_PATH`'S LOGIN, AND WITH NONE KNOWN), GROK BUILD AND KIMI CODE CLI: a launch weighs
/// the rows against the pool's active row alone (`accounts::launch_row`), so with none active and
/// `main` (this machine's own login, which nothing runs on) usable it takes `main`, and the
/// report names it, never calling `main` the account in use; with `main` walled, `work`; with
/// `work` walled, `main`; with `work` active and walled (not fresh, so a launch takes the next
/// row), `main`. The JSON's `to` is the line's, its `not` the text's. Where aterm makes no
/// overlay home (off Unix, `accounts_overlay::links_here`), a launch that would take Kimi's
/// `work` is refused for it, and `next:` says none with that refusal.
#[test]
fn with_no_row_active_next_is_the_row_a_launch_takes() {
    let cases: [Case; 4] = [
        ("nothing active", |_| {}),
        ("main walled", |pool| {
            pool.wall("main", pool.env.now + 7200);
        }),
        ("work walled", |pool| {
            pool.wall("work", pool.env.now + 7200);
        }),
        ("work active and walled", |pool| {
            pool.wall("work", pool.env.now + 7200);
            pool.state(|state| state.active = Some("work".to_string()));
        }),
    ];
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let programs: &[bool] = if cli == Cli::Codex {
            &[false, true]
        } else {
            &[false]
        };
        for (case, set) in cases {
            for &known in programs {
                let pool = Pool::of(cli, "next-launch");
                set(&pool);
                let program = known.then(|| pool.new.clone());
                let at = format!("{} on {program:?}, {case}", cli.id());
                let (text, doc) = pool.report(program.as_deref());
                let launch = match pool.launch_on(program.as_deref()) {
                    Ok(Some(row)) => row,
                    Err(why) if !links_here(cli) => {
                        assert_eq!(why, accounts_overlay::NO_LINKS, "{at}");
                        let none = doc["next"]["none"].as_str().expect("why no row is next");
                        assert_eq!(next_line(&text), format!("next: none ({none})"), "{at}");
                        assert!(none.contains(&why), "{at}: {none}");
                        continue;
                    }
                    other => panic!("{at}: a launch takes a row: {other:?}"),
                };
                assert_eq!(next_line(&text), format!("next: {launch}"), "{at}: {text}");
                assert_eq!(doc["next"]["to"], Value::from(launch), "{at}: {doc}");
                assert!(
                    !text.contains("the account in use"),
                    "{at}: no row is in use: {text}"
                );
                assert_eq!(
                    not_next_json(&doc),
                    not_next(&text),
                    "{at}: JSON matches text"
                );
            }
        }
    }
}

/// A ROW THAT CANNOT SERVE IS NOT NEXT FOR ITS OWN CONDITION, EVEN WHERE IT IS THE ACTIVE ROW:
/// for Codex (on a program that keeps `CODEX_AUTH_PATH`'s login) with no row active, `main`
/// signed out or walled is not next as such, never as "the account in use" under `active: none`;
/// with `main` the active row, signed out, it is not next as `signed-out`, and while it can serve
/// a launch takes it, so it is next, never "the account in use". With the owner's pin on `work`
/// and no login of it here, the
/// pin refuses every launch, so no row is next, and `next:` says why in text and JSON (CONTROL:
/// with its login here, `next:` names a row). Claude Code, whose session on this machine's own login
/// rotates off it, says `main`'s own condition too, and "the account in use" while it serves.
/// Kimi Code CLI's one row on this machine's own login, with no login made here (as `accounts
/// adopt-default` leaves it), is not next as not signed in here, so no other row is offered to
/// list while that row can be signed in.
#[test]
fn a_row_that_cannot_serve_is_not_next_for_its_own_condition() {
    let signed_out = |pool: &Pool| {
        pool.signed_out("main");
    };
    for cli in [Cli::Codex, Cli::Claude] {
        let program = |pool: &Pool| (cli == Cli::Codex).then(|| pool.new.clone());
        let pool = Pool::of(cli, "own-condition");
        let (text, _) = pool.report(program(&pool).as_deref());
        let usable = not_next(&text);
        assert_eq!(
            usable.get("main").map(String::as_str),
            (cli == Cli::Claude).then_some("the account in use"),
            "{}: CONTROL: {text}",
            cli.id()
        );
        signed_out(&pool);
        let (text, doc) = pool.report(program(&pool).as_deref());
        let words = reason_words(not_next(&text));
        assert_eq!(words["main"], "signed-out", "{}: {text}", cli.id());
        assert_eq!(
            reason_words(not_next_json(&doc)),
            words,
            "{}: JSON matches text",
            cli.id()
        );

        let pool = Pool::of(cli, "own-condition-walled");
        let until = pool.wall("main", pool.env.now + 7200);
        let (text, _) = pool.report(program(&pool).as_deref());
        assert_eq!(
            not_next(&text)["main"],
            format!("limited until {}", rfc3339_utc(until)),
            "{}: {text}",
            cli.id()
        );
    }

    let pool = Pool::new("own-condition-active");
    pool.state(|state| state.active = Some("main".to_string()));
    let (text, _) = pool.report(Some(&pool.new));
    assert_eq!(next_line(&text), "next: main", "CONTROL: {text}");
    assert!(!not_next(&text).contains_key("main"), "CONTROL: {text}");
    signed_out(&pool);
    let (text, _) = pool.report(Some(&pool.new));
    assert_eq!(
        reason_words(not_next(&text))["main"],
        "signed-out",
        "{text}"
    );

    let pool = Pool::new("pin-not-signed-in");
    pool.pin("work");
    let (text, _) = pool.report(Some(&pool.new));
    assert!(
        !text.contains("the pin cannot be taken now"),
        "CONTROL: {text}"
    );
    assert!(!text.contains("next: none"), "CONTROL: {text}");
    let store = Cli::Codex
        .connector()
        .stores
        .first()
        .copied()
        .expect("a login file");
    std::fs::remove_file(pool.tmp.path().join("logins/codex/work").join(store))
        .expect("work's login");
    let (text, doc) = pool.report(Some(&pool.new));
    let none = "the pin cannot be taken now: codex account work is not signed in";
    assert!(text.contains(&format!("next: none ({none})\n")), "{text}");
    assert_eq!(
        doc.get("next")
            .and_then(|n| n.get("none"))
            .and_then(|v| v.as_str()),
        Some(none),
        "JSON matches text: {doc:?}"
    );
    assert!(!text.contains("list another"), "{text}");

    let pool = Pool::of(Cli::Kimi, "own-condition-kimi");
    let roster_path = pool.env.accounts_roster_for(Cli::Kimi).expect("a roster");
    let main: Vec<accounts::Row> = accounts::Roster::load_for(Cli::Kimi, &roster_path)
        .expect("roster")
        .rows
        .into_iter()
        .filter(|row| row.dir.is_none())
        .collect();
    pool.rows(&main);
    let store = Cli::Kimi
        .connector()
        .stores
        .first()
        .copied()
        .expect("a login file");
    let home = pool.env.home.as_deref().expect("a home");
    std::fs::remove_file(home.join(Cli::Kimi.accounts().default_home).join(store))
        .expect("the default login");
    let (text, doc) = pool.report(None);
    assert_eq!(
        doc["next"]["to"].as_str(),
        Some("main"),
        "a launch runs on it: {text}"
    );
    assert_eq!(
        doc["next"]["to_note"].as_str(),
        Some("not-enrolled"),
        "{text}"
    );
    assert!(!not_next(&text).contains_key("main"), "{text}");
    assert!(!text.contains("the account in use"), "{text}");
    assert!(!text.contains("list another"), "{text}");
}

/// A host that answers the Codex program a launch aterm makes runs here (`launch`), and records
/// the staging home of each pool sign-in it is asked to run, signing nothing in.
struct LaunchProgram {
    launch: Option<PathBuf>,
    sign_ins: std::cell::RefCell<Vec<PathBuf>>,
}

impl AccountsHost for LaunchProgram {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no Claude probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no Claude login")
    }
    fn now(&self) -> i64 {
        1_758_412_900
    }
    fn codex_launch_program(&self) -> Option<PathBuf> {
        self.launch.clone()
    }
    fn vendor_login(&self, req: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        self.sign_ins.borrow_mut().push(req.staging.to_path_buf());
        Err("this stand-in signs nothing in".to_string())
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.sign_ins.borrow_mut().push(req.staging.to_path_buf());
        crate::harness::login_drive::Drove {
            outcome: crate::harness::login_drive::Outcome::NotRun(
                "this stand-in signs nothing in".to_string(),
            ),
            host: None,
        }
    }
}

/// EVERY JUDGE OF WHETHER A LAUNCH KEEPS A ROW'S LOGIN ITS OWN READS THE PROGRAM A LAUNCH RUNS
/// (`worker::codex_launch_program`), NOT THE ONE A SIGN-IN RUNS: with no atpkg twin and a
/// codex-alab that keeps `CODEX_AUTH_PATH`'s login in `~/.local/bin` (a stand-in), a sign-in's
/// program (`login_drive::program_of`) is that build, on which a launch would take `work`, the
/// active row; but a launch types the bare word `codex`, which the worker's own shell finds on
/// its `PATH` and which names no build aterm can read, so it takes only `main`, the row on this
/// machine's own login. The report names `main`; `accounts renew main` and `accounts login main`
/// stage no login of its own for `main` (one that launch never runs, which would leave it no row
/// to run): no sign-in runs and `main` stays on the default login.
#[test]
fn every_codex_judge_reads_the_program_a_launch_runs() {
    let pool = Pool::new("codex-launch-program");
    let home = pool.env.home.clone().expect("a home");
    let local = crate::harness::codex_alab::lay_package(&home.join(".local"), true);
    let sign_in = crate::harness::login_drive::program_of(Cli::Codex, &home);
    assert_eq!(
        sign_in, local,
        "PRECONDITION: a sign-in runs ~/.local/bin's codex-alab"
    );
    assert!(crate::harness::codex_alab::auth_path_unread(&sign_in).is_none());
    let launch = crate::harness::worker::codex_launch_program();
    assert_eq!(launch, Path::new("codex"), "the bare word");
    pool.state(|state| state.active = Some("work".to_string()));
    let host = LaunchProgram {
        launch: Some(launch.clone()),
        sign_ins: std::cell::RefCell::default(),
    };
    let (text, doc) = pool.report_on(&host);
    assert_eq!(next_line(&text), "next: main", "{text}");
    assert_eq!(doc["next"]["to"], Value::from("main"), "{doc}");
    let build = doc["next"]["only_default"]
        .as_str()
        .expect("the build's reason");
    assert!(build.contains("`codex`"), "{build}");
    assert_eq!(
        pool.launch(&sign_in),
        Ok(Some("work".to_string())),
        "PRECONDITION: on the sign-in's build a launch takes `work`"
    );
    assert_eq!(pool.launch(&launch), Ok(Some("main".to_string())));
    let store = pool.env.accounts_store_for(Cli::Codex).expect("a store");
    for verb in ["renew", "login"] {
        let line = format!("accounts {verb} main --cli codex");
        let Cmd::AccountsOf(of) = parsed(&line).expect(&line) else {
            panic!("{line}: an accounts command")
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_accounts_of(&of, &pool.env, &host, &mut out, &mut err);
        let err = String::from_utf8(err).expect("utf-8");
        assert_eq!(code, ExitCode::from(1), "{verb}: {err}");
        let why = crate::harness::codex_alab::AuthPathUnread::Unnamed(launch.clone());
        let refusal = crate::harness::codex_alab::default_row_staging("main", &why);
        assert!(err.contains(&refusal), "{verb}: {err}");
        assert!(host.sign_ins.borrow().is_empty(), "{verb}: no sign-in ran");
        let logins = store.load_logins(pool.env.now).expect("logins").0;
        assert_eq!(
            logins.get("main").and_then(|binding| binding.dir.clone()),
            None,
            "{verb}: `main` stays on the default login"
        );
    }
}

/// UNDER THE OWNER'S PIN THAT A LAUNCH TAKES, `next:` IS THE PINNED ROW: with the pin on `work`
/// (signed in here and usable), a launch takes `work` (`accounts_pool::launch_env_for`) and no
/// rotation leaves it, so for Codex on a program that keeps `CODEX_AUTH_PATH`'s login (and with
/// no program known), Grok Build and Kimi Code CLI, `next:` names `work`, as the JSON's `to` does,
/// and `work` is not "the account in use" under `not next:`. CONTROL: with `work` active and
/// walled but no pin, a launch passes it, so `next:` is `main`, and `work` is not next for its
/// wall; with the pin on `work` leased by another Mac, a launch does not take it, `next:` is
/// `main`, and `work` is not next for that claim.
#[test]
fn under_a_pin_a_launch_takes_next_is_the_pinned_row() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let programs: &[bool] = if cli == Cli::Codex {
            &[false, true]
        } else {
            &[false]
        };
        for &known in programs {
            let pool = Pool::of(cli, "next-pin");
            let program = known.then(|| pool.new.clone());
            let at = format!("{} on {program:?}", cli.id());
            let until = pool.wall("work", pool.env.now + 7200);
            pool.state(|state| state.active = Some("work".to_string()));
            let (text, _) = pool.report(program.as_deref());
            assert_eq!(next_line(&text), "next: main", "{at}: CONTROL: {text}");
            assert_eq!(
                not_next(&text)["work"],
                format!("limited until {}", rfc3339_utc(until)),
                "{at}: CONTROL: {text}"
            );
            // The wall's record goes, so `work` is usable again.
            pool.state(|state| {
                state.accounts.remove("work");
            });
            pool.pin("work");
            let (text, doc) = pool.report(program.as_deref());
            if links_here(cli) {
                assert_eq!(
                    pool.launch_on(program.as_deref()),
                    Ok(Some("work".to_string())),
                    "{at}: PRECONDITION: a launch takes the pin"
                );
                assert_eq!(next_line(&text), "next: work", "{at}: {text}");
                assert_eq!(doc["next"]["to"], Value::from("work"), "{at}: {doc}");
                assert!(!not_next(&text).contains_key("work"), "{at}: {text}");
            } else {
                assert_eq!(
                    pool.launch_on(program.as_deref()),
                    Err(accounts_overlay::NO_LINKS.to_string()),
                    "{at}: PRECONDITION: a launch refuses the pin's overlay home"
                );
                let none = doc["next"]["none"].as_str().expect("why no row is next");
                assert_eq!(next_line(&text), format!("next: none ({none})"), "{at}");
                assert!(none.contains(accounts_overlay::NO_LINKS), "{at}: {none}");
            }
            assert_eq!(
                not_next_json(&doc),
                not_next(&text),
                "{at}: JSON matches text"
            );
            let now = pool.env.now;
            pool.state(|state| state.note_lease("work", "other-mac", now + 3600, now));
            assert_ne!(
                pool.launch_on(program.as_deref()),
                Ok(Some("work".to_string())),
                "{at}: CONTROL: a launch leaves a pin leased elsewhere"
            );
            let (text, _) = pool.report(program.as_deref());
            assert_eq!(next_line(&text), "next: main", "{at}: CONTROL: {text}");
            assert_eq!(
                not_next(&text)["work"],
                format!("leased elsewhere until {}", rfc3339_utc(now + 3600)),
                "{at}: CONTROL: {text}"
            );
        }
    }
}

/// A host whose Codex a worker aterm starts types is the bare word `codex` (no atpkg twin), which
/// its shell looks for on `PATH`, the value it holds.
struct BareWord(OsString);

impl AccountsHost for BareWord {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no Claude probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn codex_launch_program(&self) -> Option<PathBuf> {
        Some(PathBuf::from("codex"))
    }
    fn launch_path(&self) -> Option<OsString> {
        Some(self.0.clone())
    }
}

/// WITH NO CODEX A LAUNCH CAN START, NO ROW IS NEXT: a worker aterm starts types the bare word
/// `codex`, and with no `codex` in any directory of the `PATH` it reads (only `~/.local/bin`'s,
/// which is not on it), no worker can start, as the doctor says (`not installed`), so `next:`
/// says none, with the doctor's fix, in text and JSON. CONTROL: with a `codex` on that `PATH`,
/// `next:` names `main`, the row a launch there takes.
#[test]
fn with_no_codex_a_launch_can_start_no_row_is_next() {
    let pool = Pool::new("codex-next-uninstalled");
    let home = pool.env.home.clone().expect("a home");
    let _local = crate::harness::codex_alab::lay_package(&home.join(".local"), true);
    let empty = pool.tmp.path().join("path-empty");
    std::fs::create_dir_all(&empty).expect("an empty PATH directory");
    let path = std::env::join_paths([&empty]).expect("a PATH");
    let (text, doc) = pool.report_on(&BareWord(path));
    let none = doc["next"]["none"].as_str().expect("why none runs");
    assert_eq!(next_line(&text), format!("next: none ({none})"), "{text}");
    assert!(none.contains("`aterm pkg install codex`"), "{none}");
    assert_eq!(doc["next"]["to"], Value::Null, "{doc}");
    assert_eq!(doc["next"]["wait_until"], Value::Null, "{doc}");

    let full = pool.tmp.path().join("path-full");
    std::fs::create_dir_all(&full).expect("a PATH directory");
    std::fs::write(full.join("codex"), "#!/bin/sh\n").expect("a codex on PATH");
    let path = std::env::join_paths([&empty, &full]).expect("a PATH");
    let (text, doc) = pool.report_on(&BareWord(path));
    assert_eq!(next_line(&text), "next: main", "CONTROL: {text}");
    assert_eq!(doc["next"]["to"], Value::from("main"), "CONTROL: {doc}");
}

/// ON A CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN, THE ROW ON THIS MACHINE'S OWN LOGIN WITH NO
/// LOGIN STORED HERE IS NOT CALLED SIGNED OUT: with no login file in the default home
/// (`not-enrolled`), whether none was ever made or one the pool recorded signed out was removed
/// since (as Codex's own logout does), the pool says no login of it is stored here
/// ([`DefaultRow::SignedOut`]'s `stored` is false) and `not next:` gives it that state, with a
/// sign-in that signs it in, never in again. CONTROL: recorded signed out with its login file
/// still here, the words say it is signed out and signs in again.
#[test]
fn a_default_row_with_no_login_stored_is_not_called_signed_out() {
    use crate::harness::codex_alab::{DefaultRow, signed_out_says};
    // (recorded signed out, login file removed, stored)
    for (recorded, removed, stored) in [
        (false, true, false),
        (true, true, false),
        (true, false, true),
    ] {
        let case = format!("recorded={recorded} removed={removed}");
        let pool = Pool::new("codex-next-never");
        if recorded {
            pool.signed_out("main");
        }
        if removed {
            let home = pool.env.home.as_deref().expect("a home");
            std::fs::remove_file(
                home.join(Cli::Codex.accounts().default_home)
                    .join("auth.json"),
            )
            .expect("the default login");
        }
        let records = pool
            .env
            .accounts_store_for(Cli::Codex)
            .expect("a store")
            .read_account_records(
                Some(&pool.env.accounts_roster_for(Cli::Codex).expect("a roster")),
                pool.env.now,
            )
            .expect("current account records");
        let (roster, state) = (records.roster, records.state);
        let enrolled = accounts::enrolled_in(
            Cli::Codex,
            &roster,
            pool.env.home.as_deref().expect("a home"),
        );
        assert_eq!(
            crate::harness::accounts_pool::default_serving(
                &roster,
                &state,
                &enrolled,
                pool.env.now
            )
            .map(|row| row.label.clone()),
            Err(DefaultRow::SignedOut {
                label: "main".into(),
                stored
            }),
            "{case}"
        );
        let (text, _) = pool.report(Some(&pool.old));
        let why = &not_next(&text)["main"];
        let (said, sign_in) = signed_out_says(stored);
        assert!(why.starts_with(said), "{case}: {text}");
        assert!(why.ends_with(&format!("login {sign_in}")), "{case}: {text}");
        assert_eq!(why.contains("in again"), stored, "{case}: {text}");
        assert_eq!(
            why.contains("no login of it is stored here"),
            !stored,
            "{case}: {text}"
        );
    }
}

/// ON A CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN, A PIN ON A ROW OF ITS OWN IS REFUSED FOR THAT
/// FIRST, SIGNED IN OR NOT: with the pin on `work` and no login of it here, a launch's refusal (as
/// the doctor, the review and `aterm ai login` quote it) is the build's refusal of a row of its
/// own, the one the report gives, not `work`'s missing sign-in, which would not end it; under
/// rotation off too. CONTROL: on a Codex that keeps `CODEX_AUTH_PATH`'s login, the same pin is
/// refused for `work`'s sign-in, as the report's `next:` says.
#[test]
fn a_pin_on_a_row_of_its_own_is_refused_for_that_before_its_sign_in() {
    use crate::harness::codex_alab::{DefaultRow, auth_path_unread, only_default_pinned};
    let pool = Pool::new("codex-pin-own-signed-out");
    pool.pin("work");
    std::fs::remove_file(pool.tmp.path().join("logins/codex/work/auth.json"))
        .expect("work's login");
    let why = auth_path_unread(&pool.old).expect("an old build");
    let own = only_default_pinned(&why, "work", &DefaultRow::Serves("main".into()));
    let refused = pool.launch(&pool.old).expect_err("a refused launch");
    assert!(refused.starts_with(&own), "{refused}");
    let (text, _) = pool.report(Some(&pool.old));
    assert!(
        next_line(&text).starts_with("next: none (the pin on `work`"),
        "{text}"
    );
    pool.mode("off");
    let refused_off = pool.launch(&pool.old).expect_err("a refused launch");
    assert!(refused_off.starts_with(&own), "off: {refused_off}");
    pool.config("");

    let roster = accounts::Roster::load_for(
        Cli::Codex,
        &pool.env.accounts_roster_for(Cli::Codex).expect("a roster"),
    )
    .expect("roster");
    let (state, _) = pool
        .env
        .accounts_store_for(Cli::Codex)
        .expect("a store")
        .load(pool.env.now)
        .expect("state");
    let enrolled = accounts::enrolled_in(
        Cli::Codex,
        &roster,
        pool.env.home.as_deref().expect("a home"),
    );
    let sign_in = crate::harness::accounts_pool::pin_refusal(
        Cli::Codex,
        &roster,
        &state,
        &enrolled,
        pool.env.now,
    )
    .expect("CONTROL: a pin refused for its sign-in");
    assert_eq!(pool.launch(&pool.new), Err(sign_in), "CONTROL");
}

impl Pool {
    /// `line`, an `accounts` form, run as `host` answers: its exit, stdout and stderr.
    fn run_on(&self, line: &str, host: &dyn AccountsHost) -> (ExitCode, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = match parsed(line).expect(line) {
            Cmd::Accounts(form) => run_accounts(&form, &self.env, host, &mut out, &mut err),
            Cmd::AccountsOf(of) => run_accounts_of(&of, &self.env, host, &mut out, &mut err),
            _ => panic!("an accounts form"),
        };
        (
            code,
            String::from_utf8(out).expect("utf-8"),
            String::from_utf8(err).expect("utf-8"),
        )
    }

    /// `accounts use <form> --cli <cli>` run as `host` answers, which must succeed: what it
    /// records past its first line.
    fn recorded(&self, form: &str, host: &dyn AccountsHost) -> String {
        let line = format!("accounts use {form} --cli {}", self.cli.id());
        let (code, out, err) = self.run_on(&line, host);
        assert_eq!(code, ExitCode::SUCCESS, "{line}: {err}");
        let mut lines = out.lines();
        assert!(lines.next().is_some(), "{line}: {out}");
        lines.collect::<Vec<_>>().join("\n")
    }

    /// The roster as it is written.
    fn roster(&self) -> Roster {
        accounts_read_roster_for(self.cli, &self.env, &mut Vec::new())
            .expect("a roster")
            .1
    }

    /// The login a launch that takes no row runs on when started plainly, in the report's words.
    fn own(&self) -> String {
        own_login_words(&self.roster())
    }
}

/// What `accounts use` says only of Claude Code's pool, whose supervisor moves sessions.
fn no_claude_words(at: &str, said: &str) {
    for word in ["Claude Code", "turn end", "supervisor"] {
        assert!(!said.contains(word), "{at}: {word:?} in {said}");
    }
}

/// `refused`, a launch's refusal, as a word that is not a launch's ends it.
fn without_no_worker(refused: &str) -> &str {
    refused
        .strip_suffix(crate::harness::accounts_pool::NO_WORKER)
        .unwrap_or(refused)
}

/// ON A CODEX THAT KEEPS NO ROW'S LOGIN ITS OWN, `accounts use` ON A ROW OF ITS OWN RECORDS THE
/// PIN AND THE REFUSAL EVERY LAUNCH UNDER IT GETS: the line past `pinned` carries the launch's
/// own refusal (`accounts_pool::launch_env_for`'s, which the report and doctor quote), with its
/// way back (the pin's end, `aterm harness accounts use --auto --cli codex`, and a build that
/// keeps it, `aterm pkg update codex`), under rotation and rotation off, and none of Claude
/// Code's words. CONTROL: judged with no program known, the line names the pin a worker takes,
/// and no refusal.
#[test]
fn on_an_old_codex_a_pin_on_a_row_of_its_own_records_the_launchs_refusal() {
    use crate::harness::codex_alab::unpin_fix;
    for mode in ["early", "off"] {
        let pool = Pool::new("use-old-own");
        pool.mode(mode);
        let said = pool.recorded("work", &CodexProgram(Some(pool.old.clone())));
        let refused = pool
            .launch(&pool.old)
            .expect_err("PRECONDITION: a launch under the pin is refused");
        let refused = without_no_worker(&refused);
        assert!(said.ends_with(refused), "{mode}: {said}");
        for back in [unpin_fix().as_str(), "aterm pkg update codex"] {
            assert!(
                said.contains(&format!("`{back}`")),
                "{mode}: {back}: {said}"
            );
        }
        no_claude_words(mode, &said);
        let unknown = pool.recorded("work", &CodexProgram(None));
        assert_eq!(
            pool.launch_on(None),
            Ok(Some("work".to_string())),
            "{mode}: CONTROL: PRECONDITION: with no program known a launch takes the pin"
        );
        assert!(!unknown.contains(refused), "{mode}: CONTROL: {unknown}");
        assert!(unknown.contains("`work`"), "{mode}: CONTROL: {unknown}");
    }
}

/// ON A CODEX THAT KEEPS `CODEX_AUTH_PATH`'S LOGIN, `accounts use` ON A ROW OF ITS OWN RECORDS
/// THAT A WORKER TAKES IT, WITH NO REFUSAL: as `accounts_pool::launch_env_for` takes it, under
/// rotation and rotation off, the line names the row and carries none of the refusal the old
/// build's launch gets, nor Claude Code's words. Where no Codex can start (the bare word `codex`
/// with none on `PATH`), the line says so with the doctor's fix, and names no row.
#[test]
fn on_a_codex_that_keeps_codex_auth_path_a_pin_records_that_a_worker_takes_it() {
    for mode in ["early", "off"] {
        let pool = Pool::new("use-new-own");
        pool.mode(mode);
        let said = pool.recorded("work", &CodexProgram(Some(pool.new.clone())));
        assert_eq!(
            pool.launch(&pool.new),
            Ok(Some("work".to_string())),
            "{mode}: PRECONDITION: a launch takes the pin"
        );
        assert!(said.contains("`work`"), "{mode}: {said}");
        let old = pool
            .launch(&pool.old)
            .expect_err("PRECONDITION: the old build's launch under the pin is refused");
        assert!(!said.contains(without_no_worker(&old)), "{mode}: {said}");
        no_claude_words(mode, &said);
    }
    let pool = Pool::new("use-uninstalled");
    let empty = pool.tmp.path().join("path-empty");
    std::fs::create_dir_all(&empty).expect("an empty PATH directory");
    let path = std::env::join_paths([&empty]).expect("a PATH");
    let said = pool.recorded("work", &BareWord(path));
    assert!(said.contains("`aterm pkg install codex`"), "{said}");
    assert!(!said.contains("`work`"), "{said}");
}

/// WITH CODEX'S LOGIN IN THE OS KEYRING, `accounts use` RECORDS THAT A WORKER TAKES NO ROW, THE
/// PIN INCLUDED, AND `use --auto` THAT IT TAKES NONE ONCE THE PIN ENDS: each line carries the
/// report's words for it (`no_row_words`), the pin's only while a pin holds, as
/// `accounts_pool::launch_env_for` takes no row, under rotation and rotation off, on either build.
#[test]
fn under_a_keyring_login_accounts_use_records_that_a_worker_takes_no_row() {
    for mode in ["early", "off"] {
        for old in [false, true] {
            let at = format!("{mode}, old build {old}");
            let pool = Pool::new("use-keyring");
            pool.keyring();
            pool.mode(mode);
            let program = if old {
                pool.old.clone()
            } else {
                pool.new.clone()
            };
            let host = CodexProgram(Some(program.clone()));
            let own = pool.own();
            let said = pool.recorded("work", &host);
            assert_eq!(
                pool.launch(&program),
                Ok(None),
                "{at}: PRECONDITION: a launch under the pin takes no row"
            );
            assert!(
                said.contains(&no_row_words(Cli::Codex, true, &own, true)),
                "{at}: {said}"
            );
            no_claude_words(&at, &said);
            let auto = pool.recorded("--auto", &host);
            assert_eq!(
                pool.launch(&program),
                Ok(None),
                "{at}: PRECONDITION: a launch with no pin takes no row"
            );
            assert!(
                auto.contains(&no_row_words(Cli::Codex, true, &own, false)),
                "{at}: {auto}"
            );
            no_claude_words(&at, &auto);
        }
    }
}

/// FOR GROK BUILD AND KIMI CODE CLI, `accounts use` SAYS WHAT A LAUNCH DOES, NOT WHAT CLAUDE
/// CODE'S SUPERVISOR DOES: the pin, which a launch takes (`accounts_pool::launch_env_for`), under
/// rotation and rotation off; and `use --auto`'s end of it, the row a launch takes then (with
/// rotation, the pool's pick; with rotation off, no row, so the login a plain launch runs on),
/// each with none of Claude Code's words. Where aterm makes no overlay home (off Unix,
/// `accounts_overlay::links_here`), a launch under Kimi's pin is refused for it, and the line
/// carries that refusal.
#[test]
fn for_grok_and_kimi_accounts_use_says_what_a_launch_does() {
    for cli in [Cli::Grok, Cli::Kimi] {
        for mode in ["early", "off"] {
            let at = format!("{} under {mode}", cli.id());
            let pool = Pool::of(cli, "use-grok-kimi");
            pool.mode(mode);
            let host = CodexProgram(None);
            let said = pool.recorded("work", &host);
            if links_here(cli) {
                assert_eq!(
                    pool.launch_on(None),
                    Ok(Some("work".to_string())),
                    "{at}: PRECONDITION: a launch takes the pin"
                );
                assert!(said.contains("`work`"), "{at}: {said}");
                assert!(said.contains(cli.connector().product), "{at}: {said}");
            } else {
                assert_eq!(
                    pool.launch_on(None),
                    Err(accounts_overlay::NO_LINKS.to_string()),
                    "{at}: PRECONDITION: a launch refuses the pin's overlay home"
                );
                assert!(said.contains(accounts_overlay::NO_LINKS), "{at}: {said}");
            }
            no_claude_words(&at, &said);
            let auto = pool.recorded("--auto", &host);
            match pool.launch_on(None) {
                Ok(Some(row)) => {
                    assert!(auto.contains(&format!("`{row}`")), "{at}: {auto}");
                }
                Ok(None) => {
                    assert!(auto.contains(&pool.own()), "{at}: {auto}");
                    assert!(!auto.contains("`work`"), "{at}: {auto}");
                }
                Err(refused) => {
                    assert!(auto.contains(without_no_worker(&refused)), "{at}: {auto}");
                }
            }
            no_claude_words(&at, &auto);
        }
    }
}

/// How a [`Pool`]'s `work`, pinned, is made a row this Mac may not use now, and why
/// (`accounts::WhyNot`).
type NotOurs = (&'static str, fn(&Pool) -> accounts::WhyNot);

/// The states that keep a pinned row from this Mac: another Mac's claim, a claim this Mac cannot
/// record, and a warm set that names another Mac.
fn not_ours_cases() -> [NotOurs; 3] {
    [
        ("claimed elsewhere", |pool| {
            let (now, until) = (pool.env.now, pool.env.now + 3600);
            pool.state(|state| state.note_lease("work", "other-mac", until, now));
            accounts::WhyNot::LeasedElsewhere { until }
        }),
        ("claims unrecorded", |pool| {
            pool.state(|state| state.unrecorded = true);
            accounts::WhyNot::Unclaimed
        }),
        ("held for another Mac", |pool| {
            let mut roster = pool.roster();
            for row in &mut roster.rows {
                if row.label == "work" {
                    row.held_by = Some("other-mac".into());
                }
            }
            pool.rows(&roster.rows);
            accounts::WhyNot::HeldElsewhere
        }),
    ]
}

/// A PIN ON A ROW THIS MAC MAY NOT USE NOW IS RECORDED WITH WHAT A LAUNCH DOES INSTEAD (another
/// Mac's claim holds it, this Mac cannot record a claim on it, or the warm set names another
/// Mac): where a launch takes another row, the line names it and why the pin's is not taken
/// (`why_not_words`); where it takes the pin all the same, the line is the one before; where it
/// is refused, the line carries that refusal, and where the pin's end ends the refusal it names
/// that way back (`accounts_pool::unpin_line`), else it says what `use --auto` says. Codex (on a
/// build that keeps `CODEX_AUTH_PATH`'s login), Grok Build and Kimi Code CLI, under rotation and
/// rotation off. CONTROL: with none of it, a launch takes the pin and the line names it, or,
/// where aterm makes no overlay home (off Unix, `accounts_overlay::links_here`), a launch under
/// Kimi's pin is refused for it and the line carries that refusal.
#[test]
fn a_pin_on_a_row_this_mac_may_not_use_records_what_a_launch_does_instead() {
    use crate::harness::accounts_pool::unpin_line;
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for mode in ["early", "off"] {
            for (case, not_ours) in not_ours_cases() {
                let at = format!("{} under {mode}, {case}", cli.id());
                let pool = Pool::of(cli, "use-held");
                pool.mode(mode);
                let program = (cli == Cli::Codex).then(|| pool.new.clone());
                let host = CodexProgram(program.clone());
                let control = pool.recorded("work", &host);
                if links_here(cli) {
                    assert_eq!(
                        pool.launch_on(program.as_deref()),
                        Ok(Some("work".to_string())),
                        "{at}: CONTROL: PRECONDITION: a launch takes the pin"
                    );
                    assert!(control.contains("`work`"), "{at}: CONTROL: {control}");
                } else {
                    assert_eq!(
                        pool.launch_on(program.as_deref()),
                        Err(accounts_overlay::NO_LINKS.to_string()),
                        "{at}: CONTROL: PRECONDITION: a launch refuses the pin's overlay home"
                    );
                    let no_links = accounts_overlay::NO_LINKS;
                    assert!(control.contains(no_links), "{at}: CONTROL: {control}");
                }
                let why = not_ours(&pool);
                let said = pool.recorded("work", &host);
                let launched = pool.launch_on(program.as_deref());
                let auto = pool.recorded("--auto", &host);
                let unpinned = pool.launch_on(program.as_deref());
                let back = format!("`{}`", unpin_line(cli));
                match launched {
                    Ok(Some(row)) if row == "work" => assert_eq!(said, control, "{at}"),
                    Ok(Some(row)) => {
                        assert!(said.contains(&format!("`{row}`")), "{at}: {said}");
                        assert!(said.contains(&why_not_words(why)), "{at}: {said}");
                    }
                    Err(refused) => {
                        assert!(said.contains(without_no_worker(&refused)), "{at}: {said}");
                        if unpinned.as_ref().err() == Some(&refused) {
                            assert_eq!(said, auto, "{at}: the pin's end changes no refusal");
                            assert!(!said.contains(&back), "{at}: {said}");
                        } else {
                            assert!(said.contains(&back), "{at}: the way back: {said}");
                            assert_ne!(said, auto, "{at}");
                        }
                    }
                    Ok(None) => panic!("{at}: PRECONDITION: a launch takes a row or is refused"),
                }
                no_claude_words(&at, &said);
            }
        }
    }
}

/// CONTROL: CLAUDE CODE'S `accounts use` WORDS ARE ITS SUPERVISOR'S, AS BEFORE: the pin, and
/// `use --auto`'s end of it.
#[test]
fn claude_codes_accounts_use_words_are_its_supervisors() {
    let pool = Pool::of(Cli::Claude, "use-claude");
    let host = CodexProgram(None);
    let (code, out, err) = pool.run_on("accounts use work", &host);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        out,
        format!("work: the active account, pinned\n{ACCOUNTS_USE_RECORDED}\n")
    );
    let (code, out, err) = pool.run_on("accounts use --auto", &host);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        out,
        format!("work: no longer pinned\n{ACCOUNTS_AUTO_RECORDED}\n")
    );
}

/// Off Unix aterm makes no symbolic-link overlay home (`accounts_overlay::make_link`), so a
/// walled Kimi `main` row cannot launch on `work`, and `accounts use work --cli kimi` records
/// that overlay refusal.
#[cfg(not(unix))]
#[test]
fn kimi_walled_default_and_pinned_use_report_the_overlay_refusal_where_aterm_makes_no_links() {
    let pool = Pool::of(Cli::Kimi, "kimi-no-links");
    assert_eq!(
        pool.launch_on(None).expect("main needs no overlay"),
        Some("main".to_string())
    );
    pool.wall("main", pool.env.now + 7200);
    let refused = pool
        .launch_on(None)
        .expect_err("work needs an overlay home");
    assert!(refused.ends_with("only on Unix"), "{refused}");
    let host = CodexProgram(None);
    let said = pool.recorded("work", &host);
    assert!(said.contains(without_no_worker(&refused)), "{said}");
}

/// How many programs a [`Pool`] of `cli` is judged on ([`program_of`]).
fn programs(cli: Cli) -> usize {
    if cli == Cli::Codex { 2 } else { 1 }
}

/// The `n`th program `pool` is judged on: none known, then for Codex a codex-alab that keeps
/// `CODEX_AUTH_PATH`'s login.
fn program_of(pool: &Pool, n: usize) -> Option<PathBuf> {
    (n > 0).then(|| pool.new.clone())
}

/// `label` in `pool`'s roster held for another Mac's warm set (`held_by`), as `accounts sync`
/// records it.
fn hold(pool: &Pool, label: &str) {
    let mut roster = pool.roster();
    for row in &mut roster.rows {
        if row.label == label {
            row.held_by = Some("other-mac".into());
        }
    }
    pool.rows(&roster.rows);
}

/// FOR EVERY CLI BUT CLAUDE CODE, `next:` IS THE ROW A LAUNCH TAKES, AS `accounts use` SAYS:
/// once `use --auto` ends a pin on `work`, `work` stays the active row with nothing running on
/// it, and a launch takes it while it is fresh (`accounts_pool::launch_env_for`), so `use --auto`
/// names it and `next:` names it too, as the JSON's `to` does, never calling a row "the account
/// in use"; so too with `main` held for another Mac's warm set, where no row past `work` can
/// serve and the report once said none could. Codex (with no program known and on a build that
/// keeps `CODEX_AUTH_PATH`'s login), Grok Build and Kimi Code CLI. CONTROL: with `work` walled a
/// launch takes `main`, and `next:` names it.
#[test]
fn next_is_the_row_a_launch_takes_as_accounts_use_says() {
    let cases: [Case; 3] = [
        ("work active", |_| {}),
        ("work active, main held for another Mac", |pool| {
            hold(pool, "main");
        }),
        ("CONTROL: work active and walled", |pool| {
            pool.wall("work", pool.env.now + 7200);
        }),
    ];
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        // A launch takes a Kimi row of its own only where aterm makes its overlay home.
        if !links_here(cli) {
            continue;
        }
        for (case, set) in cases {
            for n in 0..programs(cli) {
                let pool = Pool::of(cli, "next-use");
                let program = program_of(&pool, n);
                let at = format!("{} on {program:?}, {case}", cli.id());
                let host = CodexProgram(program.clone());
                set(&pool);
                pool.recorded("work", &host);
                let auto = pool.recorded("--auto", &host);
                let launch = pool
                    .launch_on(program.as_deref())
                    .unwrap_or_else(|why| panic!("{at}: {why}"))
                    .unwrap_or_else(|| panic!("{at}: a launch takes a row"));
                assert!(auto.contains(&format!("`{launch}`")), "{at}: {auto}");
                let (text, doc) = pool.report(program.as_deref());
                assert_eq!(next_line(&text), format!("next: {launch}"), "{at}: {text}");
                assert_eq!(
                    doc["next"]["to"],
                    Value::from(launch.as_str()),
                    "{at}: {doc}"
                );
                assert!(!text.contains("the account in use"), "{at}: {text}");
                assert!(!not_next(&text).contains_key(&launch), "{at}: {text}");
                assert_eq!(
                    not_next_json(&doc),
                    not_next(&text),
                    "{at}: JSON matches text"
                );
            }
        }
    }
}

/// A PIN ON A ROW THIS MAC MAY NOT USE NOW IS NOT NEXT FOR WHAT KEEPS IT FROM THIS MAC, IN THE
/// WORDS `accounts use` GIVES IT: with rotation on and the owner's pin on `work`, held for another
/// Mac's warm set, claimed by another Mac, or past a claim this Mac cannot record, `not next:`
/// says that of `work` (`why_not_words`), in text and JSON, never "the account in use" nor the
/// pool's `held-elsewhere` or `leased-elsewhere` slug (`WhyNot::as_str`), and the pin's
/// `accounts use` line says the same; `next:` names the row a launch takes instead where it
/// takes one, else none. Codex (with no program known and on a build that keeps
/// `CODEX_AUTH_PATH`'s login), Grok Build and Kimi Code CLI.
#[test]
fn a_pin_this_mac_may_not_use_is_not_next_for_what_keeps_it_from_this_mac() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for (case, not_ours) in not_ours_cases() {
            for n in 0..programs(cli) {
                let pool = Pool::of(cli, "next-held");
                let program = program_of(&pool, n);
                let at = format!("{} on {program:?}, {case}", cli.id());
                let host = CodexProgram(program.clone());
                pool.recorded("work", &host);
                let verdict = not_ours(&pool);
                let why = why_not_words(verdict);
                let said = pool.recorded("work", &host);
                let (text, doc) = pool.report(program.as_deref());
                // The report and `accounts use` give a claim on the pin in words, never as the
                // pool's slug for it.
                if matches!(
                    verdict,
                    accounts::WhyNot::HeldElsewhere | accounts::WhyNot::LeasedElsewhere { .. }
                ) {
                    let slug = verdict.as_str();
                    for (surface, words) in [
                        ("text", text.clone()),
                        ("JSON", doc.to_string()),
                        ("use", said.clone()),
                    ] {
                        assert!(
                            !words.contains(slug),
                            "{at}: {surface} says {slug}: {words}"
                        );
                    }
                }
                assert_eq!(not_next(&text).get("work"), Some(&why), "{at}: {text}");
                assert_eq!(
                    not_next_json(&doc),
                    not_next(&text),
                    "{at}: JSON matches text"
                );
                assert!(!text.contains("the account in use"), "{at}: {text}");
                match pool.launch_on(program.as_deref()) {
                    Ok(Some(row)) => {
                        assert_ne!(row, "work", "{at}: PRECONDITION: the pin is passed");
                        assert_eq!(next_line(&text), format!("next: {row}"), "{at}: {text}");
                        assert!(said.contains(&why), "{at}: {said}");
                    }
                    other => assert!(
                        next_line(&text).starts_with("next: none"),
                        "{at}: {other:?}: {text}"
                    ),
                }
            }
        }
    }
}

/// A LOW PIN A LAUNCH TAKES IS NEXT, AND NOT ALSO NOT NEXT: under rotation a launch takes the
/// owner's pin while its row's login is here, low or not (`accounts::launch_row`), though the
/// pool's ranking passes a low row over while another opens within `LOW_ONLY_AFTER_S`; with the
/// pin on `work`, its session at 95%, and `main` at a limit that ends in ten minutes, `next:`
/// names `work`, as the JSON's `to` does, and neither lists `work` under `not next:`. Codex (with
/// no program known and on a build that keeps `CODEX_AUTH_PATH`'s login), Grok Build and Kimi
/// Code CLI. CONTROL: before `work` is low, `next:` names the pin too.
#[test]
fn a_low_pin_a_launch_takes_is_next_and_not_also_not_next() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        // A launch takes a Kimi row of its own only where aterm makes its overlay home.
        if !links_here(cli) {
            continue;
        }
        for n in 0..programs(cli) {
            let pool = Pool::of(cli, "next-low-pin");
            let program = program_of(&pool, n);
            let at = format!("{} on {program:?}", cli.id());
            let host = CodexProgram(program.clone());
            pool.recorded("work", &host);
            pool.wall("main", pool.env.now + 600);
            let (text, _) = pool.report(program.as_deref());
            assert_eq!(next_line(&text), "next: work", "{at}: CONTROL: {text}");
            let now = pool.env.now;
            pool.state(|state| {
                accounts::note_sample(
                    state,
                    "work",
                    accounts::Sample {
                        session_pct: Some(95),
                        session_reset: Some(now + 3600),
                        at: now,
                        ..accounts::Sample::default()
                    },
                    now,
                );
            });
            assert_eq!(
                pool.launch_on(program.as_deref()),
                Ok(Some("work".to_string())),
                "{at}: PRECONDITION: a launch takes the low pin"
            );
            let (text, doc) = pool.report(program.as_deref());
            assert_eq!(next_line(&text), "next: work", "{at}: {text}");
            assert_eq!(doc["next"]["to"], Value::from("work"), "{at}: {doc}");
            assert!(!not_next(&text).contains_key("work"), "{at}: {text}");
            assert_eq!(
                not_next_json(&doc),
                not_next(&text),
                "{at}: JSON matches text"
            );
        }
    }
}

/// UNDER ROTATION OFF, A PIN ON A ROW ANOTHER MAC'S WARM SET HOLDS IS REFUSED, AS A PIN ANOTHER
/// MAC'S CLAIM HOLDS IS: rotation on passes such a pin (`accounts::launch_row`), and rotation off
/// starts on nothing but the pin, so a launch is refused (`accounts_pool::launch_env_for`), its
/// refusal naming the warm set and the pin's end (`accounts_pool::unpin_line`); `accounts use`
/// records that refusal, and `next:` names no row and gives it. Codex (with no program known and
/// on a build that keeps `CODEX_AUTH_PATH`'s login), Grok Build and Kimi Code CLI. CONTROL: before
/// the hold, a launch takes the pin and `next:` names it.
#[test]
fn under_rotation_off_a_pin_held_for_another_macs_warm_set_is_refused() {
    use crate::harness::accounts_pool::unpin_line;
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        // A launch takes a Kimi row of its own only where aterm makes its overlay home.
        if !links_here(cli) {
            continue;
        }
        for n in 0..programs(cli) {
            let pool = Pool::of(cli, "off-held");
            pool.mode("off");
            let program = program_of(&pool, n);
            let at = format!("{} on {program:?}", cli.id());
            let host = CodexProgram(program.clone());
            pool.recorded("work", &host);
            assert_eq!(
                pool.launch_on(program.as_deref()),
                Ok(Some("work".to_string())),
                "{at}: CONTROL: a launch takes the pin"
            );
            let (text, _) = pool.report(program.as_deref());
            assert_eq!(next_line(&text), "next: work", "{at}: CONTROL: {text}");
            hold(&pool, "work");
            let refused = pool
                .launch_on(program.as_deref())
                .expect_err("a launch under a held pin is refused");
            let refused = without_no_worker(&refused).to_string();
            assert!(
                refused.contains("held for another Mac's warm set"),
                "{at}: {refused}"
            );
            assert!(refused.contains(&unpin_line(cli)), "{at}: {refused}");
            let said = pool.recorded("work", &host);
            assert!(said.contains(&refused), "{at}: {said}");
            let (text, doc) = pool.report(program.as_deref());
            let none = doc["next"]["none"].as_str().expect("why no row is next");
            assert!(none.contains(&refused), "{at}: {none}");
            assert_eq!(
                next_line(&text),
                format!("next: none ({none})"),
                "{at}: {text}"
            );
            assert_eq!(doc["next"]["to"], Value::Null, "{at}: {doc}");
        }
    }
}

/// A host whose launch of a Grok Build or Kimi Code CLI pool runs the program
/// `worker::pooled_launch_program_in` names in the atpkg layout `layout`, and whose launch's
/// shell reads `path`.
struct PooledLaunch {
    layout: atpkg::store::Layout,
    path: OsString,
}

impl AccountsHost for PooledLaunch {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no Claude probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn launch_program(&self, cli: Cli) -> Option<PathBuf> {
        (cli != Cli::Claude && cli != Cli::Codex)
            .then(|| crate::harness::worker::pooled_launch_program_in(cli, Some(&self.layout)))
    }
    fn launch_path(&self) -> Option<OsString> {
        Some(self.path.clone())
    }
}

/// WITH NO GROK BUILD OR KIMI CODE CLI A LAUNCH CAN START, `accounts use` AND `next:` SAY IT IS
/// NOT INSTALLED, WITH THE DOCTOR'S FIX, AS FOR CODEX: with atpkg's layout holding no copy, a
/// launch runs the program word (`worker::pooled_launch_program_in`), which no directory of the
/// `PATH` its shell reads holds, so the pin's `accounts use` line names no row and gives `aterm
/// pkg install <id>`, and `next:` is none with the same fix, in text and JSON. CONTROL: with
/// atpkg's managed copy laid (and still none on `PATH`), a launch runs that copy, `accounts use`
/// says a launch takes the pin and `next:` names it.
#[test]
fn with_no_grok_or_kimi_a_launch_can_start_use_and_next_say_not_installed() {
    for cli in [Cli::Grok, Cli::Kimi] {
        // A launch takes a Kimi row of its own only where aterm makes its overlay home.
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "pooled-uninstalled");
        let prefix = pool.tmp.path().join("pkg");
        let layout = atpkg::store::Layout {
            prefix: prefix.clone(),
        };
        std::fs::create_dir_all(layout.agents_dir()).expect("atpkg's agents directory");
        let empty = pool.tmp.path().join("path-empty");
        std::fs::create_dir_all(&empty).expect("an empty PATH directory");
        let host = PooledLaunch {
            layout,
            path: std::env::join_paths([&empty]).expect("a PATH"),
        };
        let fix = format!("`aterm pkg install {}`", cli.id());
        let said = pool.recorded("work", &host);
        assert!(said.contains(&fix), "{at}: {said}");
        assert!(said.contains("not installed"), "{at}: {said}");
        assert!(!said.contains("`work`"), "{at}: {said}");
        let (text, doc) = pool.report_on(&host);
        let none = doc["next"]["none"].as_str().expect("why no row is next");
        assert!(none.contains(&fix), "{at}: {none}");
        assert_eq!(
            next_line(&text),
            format!("next: none ({none})"),
            "{at}: {text}"
        );
        assert_eq!(doc["next"]["to"], Value::Null, "{at}: {doc}");

        let managed = host.layout.agents_dir().join(cli.id());
        std::fs::write(&managed, "#!/bin/sh\n").expect("atpkg's managed copy");
        assert_eq!(
            crate::harness::worker::pooled_launch_program_in(cli, Some(&host.layout)),
            managed,
            "{at}: CONTROL: a launch runs atpkg's managed copy"
        );
        let said = pool.recorded("work", &host);
        assert!(said.contains("`work`"), "{at}: CONTROL: {said}");
        assert!(!said.contains(&fix), "{at}: CONTROL: {said}");
        let (text, _) = pool.report_on(&host);
        assert_eq!(next_line(&text), "next: work", "{at}: CONTROL: {text}");
        assert_eq!(
            crate::harness::worker::pooled_launch_program_in(cli, None),
            Path::new(cli.id()),
            "{at}: CONTROL: with no atpkg layout, the program word"
        );
    }
}

/// `accounts use` OF A ROW IT CANNOT PIN NAMES THE STEP THAT ENDS THE REFUSAL, BY ITS KIND: for
/// Codex (on a build that keeps `CODEX_AUTH_PATH`'s login), Grok Build and Kimi Code CLI, a row
/// with no login made here (`work`, its login directory emptied) and a row known signed out
/// (`main`) are each refused with that row's sign-in at a terminal, in the verb `accounts add`
/// names for it (`Cli::sign_in_verb`), and a label the roster does not list with `accounts add`.
/// On a Codex build that keeps no row's login its own, the row on this machine's own login is
/// signed in by Codex's own sign-in (`codex_alab::default_sign_in`), which `accounts login|renew`
/// refuses there. CONTROL: before `main` is signed out, its pin is recorded.
#[test]
fn accounts_use_names_the_sign_in_that_ends_its_refusal() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let id = cli.id();
        let pool = Pool::of(cli, "use-refused");
        let host = CodexProgram((cli == Cli::Codex).then(|| pool.new.clone()));
        let refused = |label: &str, host: &dyn AccountsHost| {
            let (code, out, err) = pool.run_on(&format!("accounts use {label} --cli {id}"), host);
            assert_eq!(code, ExitCode::from(1), "{id}: {label}: {out}{err}");
            err
        };
        pool.recorded("main", &host);
        let store = cli.connector().stores.first().copied().unwrap_or("login");
        let work = pool.tmp.path().join("logins").join(id).join("work");
        std::fs::remove_file(work.join(store)).expect("work's login");
        let err = refused("work", &host);
        let login = format!(
            "`aterm harness accounts {} work --cli {id}`",
            cli.sign_in_verb(false)
        );
        assert!(err.contains(&login), "{id}: {err}");
        pool.signed_out("main");
        let err = refused("main", &host);
        let renew = format!(
            "`aterm harness accounts {} main --cli {id}`",
            cli.sign_in_verb(true)
        );
        assert!(err.contains(&renew), "{id}: {err}");
        if cli == Cli::Codex {
            let err = refused("main", &CodexProgram(Some(pool.old.clone())));
            let own = format!("`{}`", crate::harness::codex_alab::default_sign_in());
            assert!(err.contains(&own), "{id}: {err}");
            assert!(!err.contains(&renew), "{id}: {err}");
        }
        let err = refused("nobody", &host);
        let add = format!("`aterm harness accounts add <label> --cli {id}`");
        assert!(err.contains(&add), "{id}: {err}");
    }
}

/// A host whose launch runs `program` (a bare word its shell looks for, or a path) and whose
/// launch's shell reads `path`.
struct OnPath {
    program: PathBuf,
    path: OsString,
}

impl AccountsHost for OnPath {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no Claude probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn codex_launch_program(&self) -> Option<PathBuf> {
        Some(self.program.clone())
    }
    fn launch_path(&self) -> Option<OsString> {
        Some(self.path.clone())
    }
}

/// WITH NO CODEX A LAUNCH CAN START, NO ROW IS NOT NEXT FOR WHAT A BUILD WOULD RUN: the bare word
/// `codex` a worker types, which no directory of its shell's `PATH` holds, starts nothing, so
/// `next:` is none with the doctor's `not installed` and its fix, and neither the text (no `this
/// Codex build:` line, no row not next for the build) nor the JSON (no `only_default`) judges a
/// build, with or without the owner's pin on `work`. CONTROL: with a `codex` on that `PATH`, a
/// build aterm cannot read before it runs, the build's rule stands: `next:` is `main`, `work` is
/// not next for the build, and the JSON names why.
#[test]
fn with_no_codex_installed_no_row_is_not_next_for_a_build() {
    let pool = Pool::new("codex-uninstalled");
    let dir = pool.tmp.path().join("path-dir");
    std::fs::create_dir_all(&dir).expect("a PATH directory");
    let host = OnPath {
        program: PathBuf::from("codex"),
        path: std::env::join_paths([&dir]).expect("a PATH"),
    };
    for pinned in [false, true] {
        if pinned {
            pool.pin("work");
        }
        let (text, doc) = pool.report_on(&host);
        let none = doc["next"]["none"].as_str().expect("why no row is next");
        assert!(
            none.contains("`aterm pkg install codex`"),
            "{pinned}: {none}"
        );
        assert_eq!(next_line(&text), format!("next: none ({none})"), "{text}");
        assert!(!text.contains("this Codex build"), "{pinned}: {text}");
        assert_eq!(doc["next"]["only_default"], Value::Null, "{pinned}: {doc}");
        assert_eq!(not_next_json(&doc), not_next(&text), "JSON matches text");
    }

    let pool = Pool::new("codex-on-path");
    let dir = pool.tmp.path().join("path-dir");
    std::fs::create_dir_all(&dir).expect("a PATH directory");
    std::fs::write(dir.join("codex"), "#!/bin/sh\n").expect("a codex on PATH");
    let host = OnPath {
        program: PathBuf::from("codex"),
        path: std::env::join_paths([&dir]).expect("a PATH"),
    };
    let (text, doc) = pool.report_on(&host);
    assert_eq!(next_line(&text), "next: main", "CONTROL: {text}");
    assert!(
        not_next(&text)["work"].contains("this Codex build runs only"),
        "CONTROL: {text}"
    );
    assert!(
        doc["next"]["only_default"].as_str().is_some(),
        "CONTROL: {doc}"
    );
}

/// THE `active:` LINE SAYS WHEN A LAUNCH WOULD NOT TAKE THE ACTIVE ROW: for Codex (on a build that
/// keeps `CODEX_AUTH_PATH`'s login), Grok Build and Kimi Code CLI, with `work` the recorded
/// active row, no session on it and no pin, a wall on `work` has a launch take `main`, and the
/// line names `main`, as `next:` does; under rotation off with no pin a launch takes no row, and
/// the line says a launch does not take `work`. CONTROL: with `work` usable a launch takes it,
/// and the line is the active row's alone.
#[test]
fn the_active_line_says_when_a_launch_takes_another_row() {
    let header = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("active: "))
            .unwrap_or_else(|| panic!("an active line: {text}"))
            .to_string()
    };
    let plain = "active: work, 0 of 0 sessions on it";
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        // A launch takes a Kimi row of its own only where aterm makes its overlay home.
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "active-line");
        let program = (cli == Cli::Codex).then(|| pool.new.clone());
        pool.state(|state| state.active = Some("work".to_string()));
        assert_eq!(
            pool.launch_on(program.as_deref()),
            Ok(Some("work".to_string())),
            "{at}: CONTROL"
        );
        let (text, _) = pool.report(program.as_deref());
        assert_eq!(header(&text), plain, "{at}: CONTROL: {text}");

        pool.wall("work", pool.env.now + 7200);
        assert_eq!(
            pool.launch_on(program.as_deref()),
            Ok(Some("main".to_string())),
            "{at}"
        );
        let (text, _) = pool.report(program.as_deref());
        assert_eq!(next_line(&text), "next: main", "{at}: {text}");
        let line = header(&text);
        assert!(
            line.starts_with(plain) && line.contains("takes `main`"),
            "{at}: {text}"
        );

        pool.mode("off");
        assert_eq!(pool.launch_on(program.as_deref()), Ok(None), "{at}");
        let (text, _) = pool.report(program.as_deref());
        assert!(next_line(&text).starts_with("next: none"), "{at}: {text}");
        let line = header(&text);
        assert!(
            line.starts_with(plain) && line != plain && !line.contains("takes `"),
            "{at}: {text}"
        );
    }
}

/// One state of a row's login in the law's matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LawLogin {
    Usable,
    SignedOut,
    NotEnrolled,
    /// At a five-hour limit.
    Walled,
    /// At its weekly limit, the one a Codex session moves at.
    WeeklyWalled,
    /// Served, with little left: a pool's last resort, and no handoff's target.
    Low,
    /// `main` alone: the default login names another account than its row
    /// ([`accounts::default_login_moved_off`]).
    Moved,
    /// `work` alone: usable on its own login, having left the default one, which `main` holds now
    /// (what a reassignment leaves, `accounts_sync::reassign_codex_default`).
    Displaced,
}

/// Who the records hold active, and whether the owner pinned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LawHold {
    None,
    Active(&'static str),
    Pin(&'static str),
}

/// What keeps a row from this Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LawClaim {
    None,
    HeldElsewhere(&'static str),
    LeasedElsewhere(&'static str),
}

/// What `aterm.toml` says of rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LawRotation {
    On,
    AccountsOff,
    HarnessOff,
    AgentOff,
    /// On, with Codex's login in the OS keyring, whose pool takes no row.
    Keyring,
    /// `accounts = "wall"`: rotation at a limit and not ahead of it.
    WallOnly,
    /// `relaunch = false`, which no handoff runs under.
    RelaunchOff,
    /// `resume_limits = false`, which no handoff runs under.
    ResumeLimitsOff,
    /// An `aterm.toml` the supervisor warns of, which no handoff runs under.
    Warned,
    /// `logins = "off"`, which keeps sign-ins off and moves nothing.
    LoginsOff,
}

/// A program a launch runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LawBuild {
    CodexNew,
    CodexOld,
    CodexMissing,
    Grok,
    GrokMissing,
    Kimi,
    KimiMissing,
}

impl LawBuild {
    fn cli(self) -> Cli {
        match self {
            Self::CodexNew | Self::CodexOld | Self::CodexMissing => Cli::Codex,
            Self::Grok | Self::GrokMissing => Cli::Grok,
            Self::Kimi | Self::KimiMissing => Cli::Kimi,
        }
    }

    const fn missing(self) -> bool {
        matches!(
            self,
            Self::CodexMissing | Self::GrokMissing | Self::KimiMissing
        )
    }

    /// The program this build names, and the `PATH` a launch's shell reads.
    fn program(self, pool: &Pool) -> LawHost {
        let path = pool.tmp.path().join("path-empty");
        std::fs::create_dir_all(&path).expect("an empty PATH directory");
        let program = match self {
            Self::CodexNew => pool.new.clone(),
            Self::CodexOld => pool.old.clone(),
            Self::CodexMissing => PathBuf::from("codex"),
            Self::GrokMissing | Self::KimiMissing => pool.tmp.path().join("store/absent"),
            Self::Grok | Self::Kimi => {
                let program = pool.tmp.path().join("store/cli");
                std::fs::write(&program, "#!/bin/sh\n").expect("a stand-in program");
                program
            }
        };
        LawHost {
            program,
            path: std::env::join_paths([&path]).expect("a PATH"),
        }
    }
}

/// The account [`LawLogin::Moved`]'s default login names.
const LAW_MOVED_TO: &str = "moved@example.com";

/// A login file naming the provider account `account`, and the address `email` where it has one.
fn law_auth(path: &Path, account: &str, email: Option<&str>) {
    let mut claims = aterm_json::json!({"https://api.openai.com/auth": {
        "chatgpt_account_id": account, "chatgpt_user_id": "user"
    }});
    if let (Some(email), Some(object)) = (email, claims.as_object_mut()) {
        object.insert("email".to_owned(), Value::from(email.to_owned()));
    }
    let encoded = aterm_codec::base64::encode(claims.to_string().as_bytes())
        .expect("base64")
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_owned();
    let token = format!("e30.{encoded}.fixture");
    std::fs::write(
        path,
        aterm_json::json!({"auth_mode": "chatgpt", "tokens": {
            "id_token": token, "access_token": token, "refresh_token": "fixture"
        }})
        .to_string(),
    )
    .expect("a login file");
}

/// A provider that answers each login's read: its account, and whether its week is spent.
struct LawReader {
    answers: BTreeMap<PathBuf, (String, bool)>,
}

impl crate::harness::codex_rotation::UsageReader for LawReader {
    fn read(
        &self,
        path: &Path,
        _: Option<&str>,
        now: i64,
        _: Duration,
        use_token: crate::harness::provider_read::Use,
    ) -> Option<crate::harness::codex_app_login::IdentifiedUsage> {
        use_token
            .begin_home(
                crate::harness::provider_read::Read::CodexEndpoint,
                path,
                now,
            )
            .ok()?;
        let (account, spent) = self.answers.get(path)?;
        let usage = crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": !spent,
                    "primary_window": {"used_percent": 0, "limit_window_seconds": 18000,
                        "reset_at": now + 3600},
                    "secondary_window": {"used_percent": if *spent { 100 } else { 0 },
                        "limit_window_seconds": 604_800, "reset_at": now + 86_400}
                }
            }),
            now,
        )?;
        Some(crate::harness::codex_app_login::IdentifiedUsage {
            identity: crate::harness::codex_renewal::Identity::from_native(account, "user"),
            usage,
        })
    }
}

/// A host whose launches of every CLI run one program, on one `PATH`.
struct LawHost {
    program: PathBuf,
    path: OsString,
}

impl AccountsHost for LawHost {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no Claude probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn codex_launch_program(&self) -> Option<PathBuf> {
        Some(self.program.clone())
    }
    fn launch_program(&self, _: Cli) -> Option<PathBuf> {
        Some(self.program.clone())
    }
    fn launch_path(&self) -> Option<OsString> {
        Some(self.path.clone())
    }
    fn sign_in_program(&self, _: Cli) -> Option<PathBuf> {
        Some(self.program.clone())
    }
}

/// One cell of the law's matrix. `work: None` is a roster of `main` alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LawWorld {
    main: LawLogin,
    work: Option<LawLogin>,
    hold: LawHold,
    claim: LawClaim,
    rotation: LawRotation,
}

/// What a surface says a launch does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LawSays {
    Row(String),
    NoRow,
    Refused,
}

/// The report's `next` as JSON gives it.
#[derive(Debug, PartialEq, Eq)]
struct LawReport {
    to: Option<String>,
    waits: bool,
    says_none: bool,
    not: BTreeSet<String>,
    why_in_use: bool,
    wall_moves: Vec<(String, String)>,
}

/// What no launch may do, whatever else says so, of what `who` says one does: `launch_now` and the
/// launch are each held to it alone, so two surfaces that loosen a refusal alike still fail. A row
/// taken is one this Mac may use (no other Mac's warm set or claim holds it), has a login here, is a
/// row of the pool where the launch takes one at all, is the pin's under rotation off, and is not a
/// row of its own on a build that never runs one. A pin another Mac holds under rotation off, or a
/// pin on a row of its own on such a build, is a refusal, never a launch on anything else. A
/// roster's one row whose default login names another account is no launch under rotation.
fn law_forbidden(
    (cli, rotation, program): (Cli, &Rotation, Option<&Path>),
    (records, enrolled): (&accounts::AccountRecords, &BTreeSet<String>),
    now: i64,
    (who, says): (&str, &LawSays),
) -> Vec<String> {
    let (roster, state) = (&records.roster, &records.state);
    let off = rotation.mode == accounts::Mode::Off;
    let no_row = crate::harness::accounts_pool::takes_no_row(cli, rotation.keyring);
    let unread =
        crate::harness::accounts_pool::default_only_now(cli, roster, state, enrolled, program, now)
            .is_some();
    let pin = state
        .active
        .as_deref()
        .filter(|_| state.pinned)
        .and_then(|label| roster.row(label));
    let kept_from_here = |row: &accounts::Row| {
        row.held_by.is_some() || state.lease_verdict(&row.label, now).is_err()
    };
    let mut wrong = Vec::new();
    let moved = accounts::default_login_moved_off(roster, rotation.default_login.as_deref());
    if let (Some(moved), [_only]) = (moved, roster.rows.as_slice())
        && *says == LawSays::NoRow
        && !off
        && !no_row
        && !state.pinned
    {
        wrong.push(format!(
            "{who} runs a launch as another account while the pool counts `{}`",
            moved.label
        ));
    }
    if let LawSays::Row(label) = says {
        let row = roster.row(label).expect("a row of the roster");
        if no_row {
            wrong.push(format!("{who} took `{label}` where a launch takes no row"));
        }
        if kept_from_here(row) {
            wrong.push(format!("{who} took `{label}`, which another Mac keeps"));
        }
        if matches!(
            accounts::condition(row, state, enrolled.contains(label), now),
            accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
        ) {
            wrong.push(format!("{who} took `{label}`, which has no login here"));
        }
        if unread && row.dir.is_some() {
            wrong.push(format!(
                "{who} took `{label}` on a build that never runs it"
            ));
        }
        if off && pin.is_none_or(|pin| pin.label != *label) {
            wrong.push(format!(
                "{who} took `{label}` under rotation off with no pin on it"
            ));
        }
    }
    if !no_row && !matches!(says, LawSays::Refused) {
        if off && pin.is_some_and(kept_from_here) {
            wrong.push(format!(
                "{who} says {says:?} under rotation off, with the pin on a row another Mac keeps"
            ));
        }
        if unread && pin.is_some_and(|pin| pin.dir.is_some()) {
            wrong.push(format!(
                "{who} says {says:?} with the pin on a row its build never runs"
            ));
        }
    }
    wrong
}

impl Pool {
    /// The pool's roster, login files, `aterm.toml` and records as `world` has them. A commit lays
    /// the records of a world the pool did not just lay. Laying that world again puts back what
    /// that commit left, whatever a step has made of them since; a step leaves no receipt pending,
    /// since none of these worlds fails a publication.
    fn lay(&self, world: &LawWorld) {
        let (cli, now) = (self.cli, self.env.now);
        let store_file = cli.connector().stores.first().copied().unwrap_or("login");
        let home = self.env.home.as_deref().expect("a home");
        let default = home.join(cli.accounts().default_home).join(store_file);
        let own = self.tmp.path().join("logins").join(cli.id()).join("work");
        for (file, login) in [
            (&default, Some(world.main)),
            (&own.join(store_file), world.work),
        ] {
            match login {
                Some(LawLogin::NotEnrolled) => {
                    let _ = std::fs::remove_file(file);
                }
                Some(LawLogin::Moved) => {
                    law_auth(file, "moved", Some(LAW_MOVED_TO));
                }
                _ => std::fs::write(file, "{}").expect("a stand-in login"),
            }
        }
        let vendor = accounts::Vendor {
            region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
            ..accounts::Vendor::default()
        };
        let held = |label: &str| {
            (world.claim == LawClaim::HeldElsewhere(if label == "main" { "main" } else { "work" }))
                .then(|| "other-mac".to_string())
        };
        let mut rows = vec![accounts::Row {
            label: "main".into(),
            email: Some("owner@example.com".into()),
            held_by: held("main"),
            vendor,
            ..accounts::Row::default()
        }];
        if world.work.is_some() {
            rows.push(accounts::Row {
                label: "work".into(),
                dir: Some(own.display().to_string()),
                held_by: held("work"),
                vendor,
                ..accounts::Row::default()
            });
        }
        self.rows(&rows);
        let agent_off = format!("[harness.{}]\nenabled = false\n", cli.program().name());
        let keeps = home.join(cli.accounts().default_home).join("config.toml");
        if world.rotation == LawRotation::Keyring {
            self.keyring();
        } else {
            let _ = std::fs::remove_file(&keeps);
        }
        self.config(match world.rotation {
            LawRotation::On | LawRotation::Keyring => "",
            LawRotation::AccountsOff => "[harness]\naccounts = \"off\"\n",
            LawRotation::HarnessOff => "[harness]\nenabled = false\n",
            LawRotation::AgentOff => &agent_off,
            LawRotation::WallOnly => "[harness]\naccounts = \"wall\"\n",
            LawRotation::RelaunchOff => "[harness]\nrelaunch = false\n",
            LawRotation::ResumeLimitsOff => "[harness]\nresume_limits = false\n",
            LawRotation::Warned => "[harness]\nno_such_key = true\n",
            LawRotation::LoginsOff => "[harness]\nlogins = \"off\"\n",
        });
        let store = self.env.accounts_store_for(cli).expect("a store");
        let mut laid = self.laid.borrow_mut();
        if let Some(laid) = laid
            .as_ref()
            .filter(|laid| laid.world == *world && laid.now == now)
        {
            std::fs::write(store.state_file(), &laid.state).expect("the state");
            std::fs::write(store.logins_file(), &laid.logins).expect("the logins");
            return;
        }
        store
            .transact_logins(now, &mut None, |state, logins| {
                *state = accounts::State::default();
                logins.labels.clear();
                for (label, login) in [("main", Some(world.main)), ("work", world.work)] {
                    match login {
                        Some(LawLogin::SignedOut) => {
                            accounts::note_auth(state, label, accounts::Auth::SignedOut, now);
                        }
                        Some(LawLogin::Walled) => {
                            accounts::note_wall(
                                state,
                                label,
                                "five_hour",
                                Some(now + 7200),
                                now - 60,
                                now,
                            );
                        }
                        Some(LawLogin::WeeklyWalled) => {
                            accounts::note_wall(
                                state,
                                label,
                                "seven_day",
                                Some(now + 86_400),
                                now - 60,
                                now,
                            );
                        }
                        Some(LawLogin::Low) => {
                            accounts::note_sample(
                                state,
                                label,
                                accounts::Sample {
                                    session_pct: Some(95),
                                    session_reset: Some(now + 7200),
                                    at: now,
                                    ..accounts::Sample::default()
                                },
                                now,
                            );
                        }
                        _ => {}
                    }
                }
                if world.work == Some(LawLogin::Displaced) {
                    logins.labels.insert(
                        "work".to_string(),
                        accounts::Binding {
                            dir: Some(own.display().to_string()),
                            was: vec![None],
                            ..accounts::Binding::default()
                        },
                    );
                }
                if let LawClaim::LeasedElsewhere(label) = world.claim {
                    state.note_lease(label, "other-mac", now + 3600, now);
                }
                match world.hold {
                    LawHold::None => {}
                    LawHold::Active(label) => {
                        state.active = Some(label.to_string());
                        state.proven = true;
                    }
                    LawHold::Pin(label) => {
                        state.active = Some(label.to_string());
                        state.proven = true;
                        state.pinned = true;
                    }
                }
                Ok(Vec::new())
            })
            .expect("the records");
        *laid = Some(LaidRecords {
            world: *world,
            now,
            state: std::fs::read(store.state_file()).expect("the state"),
            // A commit that changed no logins writes none, and a step may write some since: the
            // empty logins read as that absent file does.
            logins: std::fs::read(store.logins_file())
                .unwrap_or_else(|_| (accounts::Logins::default().to_json() + "\n").into_bytes()),
        });
    }

    /// The report's `next`, in structured values, as the JSON command says it.
    fn law_report(&self, host: &dyn AccountsHost) -> LawReport {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let line = format!("accounts --cli {} --json", self.cli.id());
        let code = match parsed(&line).expect("the line") {
            Cmd::Accounts(form) => run_accounts(&form, &self.env, host, &mut out, &mut err),
            Cmd::AccountsOf(of) => run_accounts_of(&of, &self.env, host, &mut out, &mut err),
            _ => panic!("an accounts report"),
        };
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
        let doc: Value = aterm_json::from_str(String::from_utf8_lossy(&out).trim()).expect("JSON");
        let next = &doc["next"];
        let not = next["not"].as_array().expect("next.not");
        let in_use = why_not_words(accounts::WhyNot::Me);
        LawReport {
            to: next["to"].as_str().map(str::to_string),
            waits: next["wait_until"].as_i64().is_some(),
            says_none: next.get("none").is_some_and(|none| !none.is_null()),
            not: not
                .iter()
                .map(|row| row["label"].as_str().expect("label").to_string())
                .collect(),
            why_in_use: not
                .iter()
                .any(|row| row["why"].as_str() == Some(in_use.as_str())),
            wall_moves: next["wall_moves"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|row| {
                            (
                                row["from"].as_str().expect("from").to_string(),
                                row["to"].as_str().expect("to").to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// What `aterm ai review` and the doctor say of a launch on the records as they stand: the
    /// review runs under the row `decided` names, on this machine's own login where it names none,
    /// and runs nothing where it is refused; the doctor's row says it is refused just then.
    fn law_fact(&self) -> crate::ai::facts::Facts {
        let (cli, now) = (self.cli, self.env.now);
        let home = self.env.home.clone().expect("a home");
        let shown = home.display().to_string();
        let var = move |name: &str| (name == "HOME").then(|| shown.clone());
        // The Codex a launch runs, which the facts read from atpkg's layout.
        let prefix = self.tmp.path().join("pkg");
        // One mode for every pool, as `aterm ai` reads it, unless a switch turns one pool off.
        let (policy, _) = SupervisorConfig::from_path(self.env.config.as_deref());
        let accounts_mode = [Cli::Codex, Cli::Grok, Cli::Kimi]
            .into_iter()
            .all(|cli| policy.on_for(cli.program()))
            .then_some(policy.accounts);
        let machine = crate::ai::facts::Machine {
            now,
            home: Some(home),
            var: &var,
            pkg: Some(atpkg::store::Layout { prefix }),
            exclude: Vec::new(),
            records: Default::default(),
            harness_state: Some(self.env.state.clone()),
            roster: self.env.accounts_roster_for(cli),
            accounts_mode,
            config: self.env.config.clone(),
        };
        crate::ai::facts::gather(&machine, cli.connector())
    }

    /// The Codex a launch runs, the one `host`'s build names, in atpkg's layout where
    /// [`Self::law_fact`] reads it.
    fn lay_launcher(&self, host: &LawHost) {
        let agents = self.tmp.path().join("pkg/agents");
        std::fs::create_dir_all(&agents).expect("atpkg's agents");
        std::fs::write(
            agents.join("codex"),
            crate::harness::codex_alab::twin_text(&host.program),
        )
        .expect("atpkg's launcher");
    }

    /// Whether the doctor and a review agree with what `launch_now` decided.
    fn law_review(&self, decided: &LawSays, records: &accounts::AccountRecords) -> Vec<String> {
        let now = self.env.now;
        let mut fact = self.law_fact();
        let doctor = fact
            .login_pool()
            .is_some_and(|pool| pool.launch_refused.is_some());
        let mut wrong = Vec::new();
        if doctor != (*decided == LawSays::Refused) {
            wrong.push(format!(
                "launch_now decided {decided:?}, and the doctor's row says refused: {doctor}"
            ));
        }
        let review = crate::ai::review::review_row(&mut fact, now, self.env.config.as_deref());
        let own = records
            .roster
            .row_for_login(None)
            .map(|row| row.label.as_str());
        let agrees = match (decided, &review) {
            (LawSays::Row(row), Ok(Some(taken))) => row == taken,
            (LawSays::NoRow, Ok(None)) | (LawSays::Refused, Err(_)) => true,
            (LawSays::NoRow, Ok(Some(taken))) => own == Some(taken.as_str()),
            _ => false,
        };
        if !agrees {
            wrong.push(format!(
                "launch_now decided {decided:?}, and the review takes {review:?}"
            ));
        }
        wrong
    }

    /// The report's `at a weekly limit:` of a session on `label`, against what moves one: for Codex
    /// the handoff a codex-alab proves, end to end (`codex_rotation::prepare`, the provider's reads
    /// answered, then `with_prepared_writer`'s verdict), for another CLI the plan a window's
    /// supervisor asks at a wall (`accounts_pool::session_plan`). The same row, or no move in both.
    /// A row with no login here runs no session, so the report has no line of it.
    fn law_wall(&self, host: &LawHost, label: &str) -> Vec<String> {
        use crate::harness::accounts_pool::{Session, session_plan};
        let (cli, now) = (self.cli, self.env.now);
        let path = self.env.accounts_roster_for(cli).expect("a roster");
        let store = self.env.accounts_store_for(cli).expect("a store");
        let report = self.law_report(host);
        let said = report
            .wall_moves
            .iter()
            .find(|(from, _)| from == label)
            .map(|(_, to)| to.clone());
        let records = store
            .read_account_records(Some(&path), now)
            .expect("the records");
        let row = records.roster.row(label).expect("the session's row");
        let enrolled = enrolled_at(cli, &records.roster, &self.env);
        if matches!(
            accounts::condition(row, &records.state, enrolled.contains(label), now),
            accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
        ) {
            return said
                .map(|to| {
                    format!("`{label}` has no login here, and the report moves a session to {to}")
                })
                .into_iter()
                .collect();
        }
        let (var, value) = accounts::login_env_for(cli, row);
        let env: Vec<(&str, &str)> = value
            .as_deref()
            .map(|value| (var, value))
            .into_iter()
            .collect();
        let rotation = rotation_beside(cli, &self.env, &path).1;
        let session = |supervisor, window: &str, at| Session {
            cli,
            sid: "s-law",
            supervisor,
            env: &env,
            sample: None,
            point: accounts::Point::Wall {
                window: window.to_string(),
                at,
            },
            mode: rotation.mode,
            relaunch: crate::harness::relaunch::PoolRelaunch {
                dialect: crate::harness::upgrade::Dialect::Zsh,
                cd: None,
                exe: &host.program,
                flags: &[],
                resume: Some("019a-rollout-id"),
                home: Some(self.tmp.path()),
            },
        };
        let (moves, verdict) = if cli == Cli::Codex {
            self.law_handoff(
                &records,
                label,
                &session(accounts::Supervisor::Own, "seven_day", Some(now)),
            )
        } else {
            let plan = session_plan(
                &self.seat(),
                &session(accounts::Supervisor::Aterm, "five_hour", None),
                |_| {},
            );
            let moves = match plan.advice() {
                accounts::Advice::Move { to, .. } if to != label => Some(to.clone()),
                _ => None,
            };
            (moves, format!("{:?}", plan.advice()))
        };
        // No handoff runs on a host it is not qualified on, whatever its writer would take.
        let moves = moves.filter(|_| {
            cli != Cli::Codex || crate::harness::codex_rotation::QUOTA_ROTATION_QUALIFIED
        });
        let mut wrong = Vec::new();
        if said != moves {
            wrong.push(format!(
                "a session on `{label}`: the report's `at a weekly limit:` says {said:?}, and the \
                 handoff {verdict}"
            ));
        }
        // Held to it alone, so a report and a writer that loosen it alike still fail.
        let low = |to: &String| {
            records.roster.row(to).is_some_and(|row| {
                accounts::condition(row, &records.state, enrolled.contains(to), now)
                    == accounts::Condition::Low
            })
        };
        if cli == Cli::Codex && (said.as_ref().is_some_and(low) || moves.as_ref().is_some_and(low))
        {
            wrong.push(format!(
                "a session on `{label}` hands off to a row with little left: {said:?}, {moves:?}"
            ));
        }
        wrong
    }

    /// Where a Codex session on `label` hands off to at its weekly limit, through the production
    /// path: each login's file names its account as the provider's read does, `prepare` selects
    /// under the provider's answers (the source's week spent, every other's room), and the
    /// writer's verdict decides. The row it moves to, or `None`, and what happened in words.
    fn law_handoff(
        &self,
        records: &accounts::AccountRecords,
        label: &str,
        session: &crate::harness::accounts_pool::Session<'_>,
    ) -> (Option<String>, String) {
        use crate::harness::codex_renewal::{Identity, Intent, Phase, Purpose, Receipt};
        let now = self.env.now;
        let home = self.env.home.as_deref().expect("a home");
        let auth_of = |row: &accounts::Row| {
            row.dir
                .as_deref()
                .map_or_else(|| home.join(".codex"), PathBuf::from)
                .join("auth.json")
        };
        let mut reader = LawReader {
            answers: BTreeMap::new(),
        };
        let default_login = accounts::default_login_address(Cli::Codex, home);
        // A provider names one account for the row on the default login and its twin.
        let twin = accounts::default_login_twin(&records.roster, default_login.as_deref())
            .map(|(own, twin)| (own.label.clone(), twin.label.clone()));
        let account_of = |label: &str| match &twin {
            Some((own, twin)) if twin == label => own.clone(),
            _ => label.to_owned(),
        };
        for row in &records.roster.rows {
            let auth = auth_of(row);
            if !auth.is_file() {
                continue;
            }
            // What the record says of the login is what a provider's read of it would: the
            // moved login keeps naming the account it moved to.
            let moved =
                accounts::default_login_moved_off(&records.roster, default_login.as_deref())
                    .is_some_and(|moved| moved.label == row.label);
            let account = account_of(&row.label);
            law_auth(&auth, &account, moved.then_some(LAW_MOVED_TO));
            reader.answers.insert(auth, (account, row.label == label));
        }
        let proposal = crate::harness::codex_rotation::prepare(
            &self.seat(),
            session,
            &Identity::from_native(&account_of(label), "user"),
            &reader,
            Duration::from_secs(60),
        );
        let proposal = match proposal {
            Ok(Some(proposal)) => proposal,
            other => return (None, format!("prepared nothing: {:?}", other.err())),
        };
        let next = proposal.successor().expect("a successor");
        let receipt = Receipt {
            destination: Some(next.identity.clone()),
            id: "a".repeat(32),
            intent: Intent {
                purpose: Purpose::WeeklyQuota,
                thread: "00000000-0000-0000-0000-000000000001".into(),
                wall: "00000000-0000-0000-0000-000000000002".into(),
                identity: proposal.source_identity.clone(),
            },
            sid: "s-law".into(),
            old_pid: 10,
            old_birth: 1,
            claim_revision: 1,
            human_seq: 0,
            old_auth: next.old_auth.clone(),
            new_auth: next.new_auth.clone(),
            generation: next.generation,
            row_stamp: crate::harness::codex_renewal::row_stamp(&next.row),
            native_prepare: "sealed native recipe".into(),
            expires_ms: u64::MAX,
            phase: Phase::Ready,
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: false,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        let verdict =
            crate::harness::codex_rotation::with_prepared_writer(&self.env, &receipt, |_, _| {
                Ok(true)
            });
        let _ = now;
        match verdict {
            Ok(Some(true)) => (
                Some(proposal.destination.label.clone()),
                format!("moves to `{}`", proposal.destination.label),
            ),
            other => (
                None,
                format!(
                    "selected `{}`, and its writer said {other:?}",
                    proposal.destination.label
                ),
            ),
        }
    }

    /// What the report, `accounts use`, `launch_now`, `aterm ai review`, the doctor and the launch
    /// say of a launch on the records as they stand: where they disagree, or where `launch_now` or
    /// the launch does what no launch may ([`law_forbidden`]), in structured values, empty where
    /// all is well. `use_says` is what `accounts use` decided, where the command itself ran; else
    /// its judge is asked on these records.
    fn law_disagreements(
        &self,
        host: &LawHost,
        pinned: Option<&str>,
        use_says: Option<UseTakes>,
    ) -> Vec<String> {
        let (cli, now) = (self.cli, self.env.now);
        let launch = Launch::of(Some(&host.program), host);
        let path = self.env.accounts_roster_for(cli).expect("a roster");
        let store = self.env.accounts_store_for(cli).expect("a store");
        let records = store
            .read_account_records(Some(&path), now)
            .expect("the records");
        let enrolled = enrolled_at(cli, &records.roster, &self.env);
        let rotation = rotation_beside(cli, &self.env, &path).1;
        let as_laid = use_says.is_none();
        let use_says = use_says.unwrap_or_else(|| {
            use_recorded(
                (cli, launch),
                &rotation,
                pinned,
                (&records.roster, &records.state, &enrolled),
                now,
            )
            .0
        });
        let report = self.law_report(host);
        let mut wrong = Vec::new();
        if report.why_in_use {
            wrong.push(format!(
                "a launch weighs no row as in use, and the report did: {report:?}"
            ));
        }
        if report.to.as_ref().is_some_and(|to| report.not.contains(to)) {
            wrong.push(format!("next and not next both name a row: {report:?}"));
        }
        if launch.uninstalled {
            if use_says != UseTakes::NotInstalled {
                wrong.push(format!(
                    "not installed, and `use` decided {use_says:?}: {report:?}"
                ));
            }
            if report.to.is_some() || !report.says_none {
                wrong.push(format!("not installed, and the report: {report:?}"));
            }
            return wrong;
        }
        let decided = match rotation.launch_now(
            (cli, launch.program),
            &records.roster,
            &records.state,
            &enrolled,
            now,
        ) {
            Ok(Some(row)) => LawSays::Row(row.label.clone()),
            Ok(None) => LawSays::NoRow,
            Err(_) => LawSays::Refused,
        };
        // A launch that takes no row runs on this machine's own login, which the report names
        // where that is the roster's one row and rotation is on, and otherwise says in words.
        let own = (rotation.mode != accounts::Mode::Off
            && !crate::harness::accounts_pool::takes_no_row(cli, rotation.keyring))
        .then(|| records.roster.rows.first().map(|row| row.label.clone()))
        .flatten();
        let report_to = match &decided {
            LawSays::Row(row) => Some(row.clone()),
            LawSays::NoRow => own,
            LawSays::Refused => None,
        };
        if report.to != report_to {
            wrong.push(format!(
                "launch_now decided {decided:?}, so the report's `to` is {report_to:?}: {report:?}"
            ));
        }
        if decided == LawSays::Refused
            && !report.waits
            && !report.says_none
            && report.not.len() != records.roster.rows.len()
        {
            wrong.push(format!(
                "a refused launch, and the report leaves a row unjudged: {report:?}"
            ));
        }
        if decided != LawSays::Refused && report.waits {
            wrong.push(format!(
                "launch_now decided {decided:?}, and the report waits"
            ));
        }
        let use_law = match use_says {
            UseTakes::Row(row) => LawSays::Row(row),
            UseTakes::NoRow => LawSays::NoRow,
            UseTakes::Refused | UseTakes::NotInstalled => LawSays::Refused,
        };
        if use_law != decided {
            wrong.push(format!(
                "`use` decided {use_law:?}, launch_now decided {decided:?}"
            ));
        }
        // The launch below writes its claim, so what reads the records as they stand comes first.
        // The records `use` leaves are shapes the laid worlds hold too, so a review reads those.
        if as_laid {
            wrong.extend(self.law_review(&decided, &records));
        }
        let unread = |_: Cli, _: &Path, _: Option<&str>, _: i64| None;
        let env_says = match crate::harness::accounts_pool::launch_env_for(
            &self.seat(),
            cli,
            &unread,
            launch.program,
        ) {
            Ok(Some(env)) => LawSays::Row(env.label),
            Ok(None) => LawSays::NoRow,
            Err(_) => LawSays::Refused,
        };
        if env_says != decided {
            wrong.push(format!(
                "the launch took {env_says:?}, launch_now decided {decided:?}"
            ));
        }
        for said in [("launch_now", &decided), ("the launch", &env_says)] {
            wrong.extend(law_forbidden(
                (cli, &rotation, launch.program),
                (&records, &enrolled),
                now,
                said,
            ));
        }
        wrong
    }
}

/// The matrix, for one build. Every pair of row conditions and a roster of `main` alone, each with
/// no one active, either row active and either row pinned, under rotation on, and with no one
/// active or a pin under `[harness] accounts = "off"`. A row held for another Mac or leased
/// elsewhere crosses with the pairs where a row can serve, since a claim on a row that cannot serve
/// decides nothing. The switches that turn rotation off for a whole harness or one agent cross with
/// fewer, since they read off what `accounts = "off"` does, as Codex's login in the OS keyring does.
/// On a build that is not there `use` says so and the report names no row whatever the records say,
/// and [`Pool::law_disagreements`] judges no launch, so it takes one usable pair and one with no
/// login, each with no pin under rotation on and with one under `accounts = "off"`, and no claim.
/// `every_step_a_surface_names_can_be_taken` reads these worlds too, and does read a launch's refusal
/// there. On a Codex that is not there a pin on a row of its own draws one, by a path of its own
/// under each of rotation on and `accounts = "off"` (`accounts_pool::refused_pin`), that names
/// `accounts use main`, so Codex's usable pair also takes a pin on `work` under each. Only Codex has
/// that refusal (`accounts_pool::default_only`), and that pin makes no surface of Grok's or Kimi's
/// name anything more.
///
/// A claim crosses those pairs and every hold on the two Codex builds there, and on Grok and Kimi
/// only the pair where both rows can serve, since `launch_now` and what a claim decides read no
/// CLI past the default-only gate these builds do not take. THE GAP: a defect in Grok's or Kimi's
/// claims away from that pair goes unseen; and those two share an arm Codex does not take (no
/// contextual auth, and claims that apply to Claude and Codex alone), so a defect in it that a
/// claim away from that pair shows is seen by no build.
fn law_worlds(build: LawBuild) -> Vec<LawWorld> {
    use LawLogin::{Displaced, Low, Moved, NotEnrolled, SignedOut, Usable, Walled, WeeklyWalled};
    const LOGINS: [LawLogin; 4] = [Usable, SignedOut, NotEnrolled, Walled];
    const HOLDS: [LawHold; 5] = [
        LawHold::None,
        LawHold::Active("main"),
        LawHold::Active("work"),
        LawHold::Pin("main"),
        LawHold::Pin("work"),
    ];
    const OFF_HOLDS: [LawHold; 3] = [LawHold::None, LawHold::Pin("main"), LawHold::Pin("work")];
    const CLAIMED: [(LawLogin, Option<LawLogin>); 8] = [
        (Usable, Some(Usable)),
        (Usable, Some(SignedOut)),
        (Usable, Some(Walled)),
        (SignedOut, Some(Usable)),
        (Walled, Some(Usable)),
        (Walled, Some(Walled)),
        (Usable, None),
        (Walled, None),
    ];
    const CLAIMS: [LawClaim; 4] = [
        LawClaim::HeldElsewhere("main"),
        LawClaim::HeldElsewhere("work"),
        LawClaim::LeasedElsewhere("main"),
        LawClaim::LeasedElsewhere("work"),
    ];
    let world = |(main, work), hold, claim, rotation| LawWorld {
        main,
        work,
        hold,
        claim,
        rotation,
    };
    let mut worlds = Vec::new();
    let every_pair = LOGINS.into_iter().flat_map(|main| {
        LOGINS
            .into_iter()
            .map(Some)
            .chain([None])
            .map(move |work| (main, work))
    });
    if build.missing() {
        for logins in [(Usable, Some(Usable)), (NotEnrolled, None)] {
            worlds.push(world(
                logins,
                LawHold::None,
                LawClaim::None,
                LawRotation::On,
            ));
            worlds.push(world(
                logins,
                LawHold::Pin("main"),
                LawClaim::None,
                LawRotation::AccountsOff,
            ));
        }
        if build.cli() == Cli::Codex {
            for rotation in [LawRotation::On, LawRotation::AccountsOff] {
                worlds.push(world(
                    (Usable, Some(Usable)),
                    LawHold::Pin("work"),
                    LawClaim::None,
                    rotation,
                ));
            }
        }
        return worlds;
    }
    for logins in every_pair {
        for hold in HOLDS {
            worlds.push(world(logins, hold, LawClaim::None, LawRotation::On));
        }
        for hold in OFF_HOLDS {
            worlds.push(world(
                logins,
                hold,
                LawClaim::None,
                LawRotation::AccountsOff,
            ));
        }
    }
    for logins in CLAIMED {
        if build.cli() != Cli::Codex && logins != (Usable, Some(Usable)) {
            continue;
        }
        let claims = CLAIMS.into_iter().filter(|claim| {
            logins.1.is_some()
                || !matches!(
                    claim,
                    LawClaim::HeldElsewhere("work") | LawClaim::LeasedElsewhere("work")
                )
        });
        for claim in claims {
            for hold in HOLDS {
                worlds.push(world(logins, hold, claim, LawRotation::On));
            }
            for hold in OFF_HOLDS {
                worlds.push(world(logins, hold, claim, LawRotation::AccountsOff));
            }
        }
    }
    for logins in [
        (Usable, Some(Usable)),
        (Walled, Some(Usable)),
        (Usable, None),
    ] {
        for hold in OFF_HOLDS {
            for rotation in [LawRotation::HarnessOff, LawRotation::AgentOff] {
                worlds.push(world(logins, hold, LawClaim::None, rotation));
            }
            if build.cli() == Cli::Codex {
                worlds.push(world(logins, hold, LawClaim::None, LawRotation::Keyring));
            }
        }
    }
    // Where a weekly limit stands, a target has little left, or the default login moved on: each
    // pair where it decides something, under what a handoff reads of aterm.toml.
    if build.cli() == Cli::Codex {
        for logins in [
            (WeeklyWalled, Some(Usable)),
            (Usable, Some(WeeklyWalled)),
            (Usable, Some(Low)),
            (Low, Some(Usable)),
            (Low, Some(Low)),
            (Moved, Some(Usable)),
            (Moved, Some(NotEnrolled)),
            (Moved, None),
            (Usable, Some(Displaced)),
            (WeeklyWalled, Some(Displaced)),
        ] {
            for hold in HOLDS {
                worlds.push(world(logins, hold, LawClaim::None, LawRotation::On));
            }
            for hold in OFF_HOLDS {
                worlds.push(world(
                    logins,
                    hold,
                    LawClaim::None,
                    LawRotation::AccountsOff,
                ));
            }
            for claim in [
                LawClaim::HeldElsewhere("main"),
                LawClaim::HeldElsewhere("work"),
            ] {
                if logins.1.is_some() || claim == LawClaim::HeldElsewhere("main") {
                    worlds.push(world(logins, LawHold::None, claim, LawRotation::On));
                }
            }
        }
        for logins in [
            (Usable, Some(Usable)),
            (WeeklyWalled, Some(Usable)),
            (Usable, Some(Low)),
        ] {
            for rotation in [
                LawRotation::WallOnly,
                LawRotation::RelaunchOff,
                LawRotation::ResumeLimitsOff,
                LawRotation::Warned,
                LawRotation::LoginsOff,
            ] {
                for hold in OFF_HOLDS {
                    worlds.push(world(logins, hold, LawClaim::None, rotation));
                }
            }
        }
    }
    worlds
}

/// [`law_worlds`]'s `share` of `build`, in one pool: how many, and each disagreement's kind and whole
/// line.
fn law_share(build: LawBuild, share: &[LawWorld]) -> (usize, Vec<(String, String)>) {
    let pool = Pool::of(build.cli(), "launch-law");
    let host = build.program(&pool);
    pool.lay_launcher(&host);
    let mut wrong = Vec::new();
    for world in share {
        let mut say = |what: &str, found: Vec<String>| {
            wrong.extend(found.into_iter().map(|disagreement| {
                let class: String = disagreement.chars().take(24).collect();
                (
                    format!("{what}: {class}"),
                    format!("{build:?} {world:?} {what}: {disagreement}"),
                )
            }));
        };
        pool.lay(world);
        let pinned = match world.hold {
            LawHold::Pin(label) => Some(label),
            _ => None,
        };
        say("as laid", pool.law_disagreements(&host, pinned, None));
        let rows: &[&str] = if world.work.is_some() {
            &["main", "work"]
        } else {
            &["main"]
        };
        // A wall is judged on every record where rotation can move a session; elsewhere nothing
        // moves one whatever the records say, so one healthy pair of rows speaks for those.
        let walls = if matches!(build, LawBuild::CodexNew)
            && matches!(
                world.rotation,
                LawRotation::On
                    | LawRotation::WallOnly
                    | LawRotation::RelaunchOff
                    | LawRotation::ResumeLimitsOff
                    | LawRotation::Warned
                    | LawRotation::LoginsOff
            ) {
            rows
        } else if matches!(
            (world.main, world.work, world.hold),
            (LawLogin::Usable, Some(LawLogin::Usable), LawHold::None)
        ) {
            &rows[..1]
        } else {
            &[]
        };
        for label in walls {
            pool.lay(world);
            say("at a wall", pool.law_wall(&host, label));
        }
        if world.hold != LawHold::None {
            continue;
        }
        for label in rows {
            pool.lay(world);
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let (code, decided) = accounts_use_decided(
                (pool.cli, &host),
                &pool.env,
                Some(label),
                &mut out,
                &mut err,
            );
            let store = pool.env.accounts_store_for(pool.cli).expect("a store");
            let pin_held = store.load(pool.env.now).expect("state").0.pinned;
            if code != ExitCode::SUCCESS {
                if pin_held || decided.is_some() {
                    say(
                        "a refused `use`",
                        vec![format!("pinned {label} ({pin_held}), decided {decided:?}")],
                    );
                }
                continue;
            }
            if !pin_held || decided.is_none() {
                say(
                    "a recorded `use`",
                    vec![format!("pinned {label} ({pin_held}), decided {decided:?}")],
                );
                continue;
            }
            say(
                "after `use`",
                pool.law_disagreements(&host, Some(label), decided),
            );
            let (code, decided) =
                accounts_use_decided((pool.cli, &host), &pool.env, None, &mut out, &mut err);
            assert_eq!(code, ExitCode::SUCCESS);
            say(
                "after `use --auto`",
                pool.law_disagreements(&host, None, decided),
            );
        }
    }
    (share.len(), wrong)
}

/// A WORLD LAID AGAIN IS THE ONE ITS COMMIT LEFT, whatever a step made of the records since, and
/// another world is never laid from it.
///
/// NEGATIVE CONTROL (2026-11-05): with the slot keyed on `main` alone this fails at "another world
/// took this one's records", and so does the launch law, with 2 disagreements. With the state not
/// put back it fails at the bytes compared after the second lay, and the step law with 7 wrong in 4
/// classes; the launch law, which judges agreement on the records as they stand, passes that one.
/// With the logins not put back it fails at the records compared after the second lay.
#[test]
fn a_world_laid_again_is_the_one_its_commit_left() {
    let pool = Pool::new("lay-again");
    let now = pool.env.now;
    let store = pool.env.accounts_store_for(pool.cli).expect("a store");
    let world = LawWorld {
        main: LawLogin::SignedOut,
        work: Some(LawLogin::Usable),
        hold: LawHold::None,
        claim: LawClaim::None,
        rotation: LawRotation::On,
    };
    // A fresh pool's first commit has no logins to write, and the step below does.
    pool.lay(&world);
    assert!(!store.logins_file().exists());
    let as_laid = (
        store.load(now).expect("state").0,
        store.load_logins(now).expect("logins").0,
    );
    let state = std::fs::read(store.state_file()).expect("state");
    let step = || {
        store
            .transact_logins(now, &mut None, |state, logins| {
                state.active = Some("work".into());
                state.proven = true;
                state.pinned = true;
                logins.labels.insert(
                    "work".to_string(),
                    accounts::Binding {
                        was: vec![None],
                        ..accounts::Binding::default()
                    },
                );
                Ok(Vec::new())
            })
            .expect("the step");
    };
    step();
    assert_ne!(std::fs::read(store.state_file()).expect("state"), state);
    assert!(store.logins_file().exists());
    pool.lay(&world);
    assert_eq!(std::fs::read(store.state_file()).expect("state"), state);
    assert_eq!(
        (
            store.load(now).expect("state").0,
            store.load_logins(now).expect("logins").0,
        ),
        as_laid
    );
    // With logins of its own, as each later world of a pool has.
    let world = LawWorld {
        work: Some(LawLogin::Displaced),
        hold: LawHold::Pin("main"),
        ..world
    };
    pool.lay(&world);
    let images = || {
        (
            std::fs::read(store.state_file()).expect("state"),
            std::fs::read(store.logins_file()).expect("logins"),
        )
    };
    let laid = images();
    step();
    assert_ne!(images(), laid);
    pool.lay(&world);
    assert_eq!(images(), laid);
    let other = LawWorld {
        hold: LawHold::None,
        ..world
    };
    pool.lay(&other);
    assert_ne!(images().0, laid.0, "another world took this one's records");
    assert!(!store.load(now).expect("state").0.pinned);
}

/// THE SURFACES THAT SAY WHAT A LAUNCH DOES SAY THE SAME, AND NO LAUNCH DOES WHAT IT MUST REFUSE:
/// across Codex (a build that keeps `CODEX_AUTH_PATH`'s login, one that does not, none there), Grok
/// Build and Kimi Code CLI (there and not), the report's `next`, what `accounts use` decides,
/// `accounts_pool::launch_now`, `aterm ai review`'s row, the doctor's refusal and the launch itself
/// (`launch_env_for`, which writes its claim) name one row or all refuse, on the records as they
/// stand (a row signed out, with no login, walled; pinned, active or neither; held for another Mac
/// or leased elsewhere; rotation on or off, by `[harness] accounts`, the harness's switch or the
/// agent's). A session on each row moves where the report's `at a weekly limit:` says, to the row it
/// says, through the handoff's own prepare and writer ([`Pool::law_wall`]), under each of what that
/// writer reads of aterm.toml, on a held source and a low target. Agreement alone cannot catch a
/// refusal loosened everywhere, so `launch_now` and the launch are each held to [`law_forbidden`]
/// too. The command itself runs from every state with no pin (`use <row>` then `use --auto`): a pin
/// it records is judged like any other, and one it refuses records nothing. It compares labels and
/// refused-ness, never words. No world spawns anything. Claude Code is outside this law: it has no
/// `launch_now` (a Claude session starts on the login it is started with, and its moves are the
/// supervisor's), and its `next:` has its own tests. A launch and a sign-in share one program here
/// ([`LawHost`]). What Grok and Kimi cross a claim with, and the gap that leaves, is [`law_worlds`]'s.
#[test]
fn the_launch_surfaces_agree_and_no_launch_does_what_it_must_refuse() {
    let builds = [
        LawBuild::CodexNew,
        LawBuild::CodexOld,
        LawBuild::CodexMissing,
        LawBuild::Grok,
        LawBuild::GrokMissing,
        LawBuild::Kimi,
        LawBuild::KimiMissing,
    ];
    let started = std::time::Instant::now();
    let runs: Vec<(usize, Vec<(String, String)>)> = std::thread::scope(|scope| {
        let threads: Vec<_> = builds
            .into_iter()
            .filter(|build| links_here(build.cli()))
            .flat_map(|build| {
                let worlds = law_worlds(build);
                worlds
                    .chunks(worlds.len().div_ceil(4))
                    .map(<[LawWorld]>::to_vec)
                    .map(|share| scope.spawn(move || law_share(build, &share)))
                    .collect::<Vec<_>>()
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().expect("a share of a build's worlds"))
            .collect()
    });
    let worlds: usize = runs.iter().map(|(worlds, _)| worlds).sum();
    let mut classes: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    let mut count = 0;
    for (class, line) in runs.iter().flat_map(|(_, wrong)| wrong) {
        count += 1;
        classes.entry(class).or_insert((0, line)).0 += 1;
    }
    eprintln!(
        "the_launch_surfaces_agree_and_no_launch_does_what_it_must_refuse: {worlds} worlds, \
         {count} disagreements, {:?}",
        started.elapsed()
    );
    // The matrix's size, to the world the code builds, so a build or a share that quietly runs
    // nothing is a failure: a build that is not there is only four or six of them.
    let matrix: usize = builds
        .into_iter()
        .filter(|build| links_here(build.cli()))
        .map(|build| law_worlds(build).len())
        .sum();
    assert_eq!(worlds, matrix, "every world of the matrix runs");
    if builds.iter().all(|build| links_here(build.cli())) {
        assert!(matrix >= 1544, "the matrix shrank: {matrix} worlds");
    }
    assert_eq!(
        count,
        0,
        "over {worlds} worlds, the first of each kind:\n{}",
        classes
            .values()
            .map(|(many, line)| format!("{many} times: {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

impl Pool {
    /// `work`'s login file gone: no login of it is made here.
    fn no_work_login(&self) {
        let store = self
            .cli
            .connector()
            .stores
            .first()
            .copied()
            .unwrap_or("login");
        let work = self
            .tmp
            .path()
            .join("logins")
            .join(self.cli.id())
            .join("work");
        std::fs::remove_file(work.join(store)).expect("work's login");
    }

    /// The roster's rows, as written, with `change` applied to `label`'s.
    fn change_row(&self, label: &str, change: impl FnOnce(&mut accounts::Row)) {
        let mut rows = self.roster().rows;
        change(
            rows.iter_mut()
                .find(|row| row.label == label)
                .expect("the row"),
        );
        self.rows(&rows);
    }

    /// What [`sign_in_plan`] decides for `label`, as `host` answers: whether it would run, and
    /// what it said. It starts no sign-in.
    fn plan(&self, label: &str, host: &dyn AccountsHost) -> (bool, String) {
        let path = self.env.accounts_roster_for(self.cli).expect("a roster");
        let store = self.env.accounts_store_for(self.cli).expect("a store");
        let records = store
            .read_account_records(Some(&path), self.env.now)
            .expect("the records");
        let row = records.roster.row(label).expect("the row");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let plan = sign_in_plan(
            self.cli,
            &store,
            SignInPolicy {
                host,
                row,
                logins: &records.logins,
                roster_path: &path,
                own_dir: row.dir.as_deref(),
                codex_program: None,
                sign_in: crate::harness::sign_in_ladder::asked_fixture(self.cli, label),
            },
            self.env.now,
            "login",
            &mut out,
            &mut err,
        );
        (plan.is_ok(), String::from_utf8(err).expect("utf-8"))
    }

    /// `accounts use <label>`'s refusal, as `host` answers.
    fn use_refused(&self, label: &str, host: &dyn AccountsHost) -> String {
        let line = format!("accounts use {label} --cli {}", self.cli.id());
        let (code, out, err) = self.run_on(&line, host);
        assert_eq!(code, ExitCode::from(1), "{line}: {out}{err}");
        err
    }
}

/// A ROSTER OF THIS MACHINE'S OWN LOGIN ALONE IS WHERE A LAUNCH RUNS UNTIL A WALL STANDS ON IT.
/// Free, the report names it and nothing waits, as `launch_env_for` starts at once on the login it
/// carries. At its wall a Codex worker sent there would only show that wall, so the launch is
/// refused and held as any pool's is: `next` waits until the wall ends, and `main` is under `not
/// next`. Another CLI's launch reads no row, so it could only wait a wall out on its record: it
/// still runs as it is, and `next` says what stands on it. CONTROL: the owner's pin on the walled
/// row: report and launch name the row, and nothing is said to stand on it, since the owner asked
/// for it.
#[test]
fn a_lone_default_row_at_its_wall_waits_as_a_launch_holds_for_it() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "lone-wall");
        let main = pool.roster().row("main").expect("main").clone();
        pool.rows(&[main]);
        let program = (cli == Cli::Codex).then(|| pool.new.clone());
        let (_, free) = pool.report(program.as_deref());
        assert_eq!(free["next"]["to"].as_str(), Some("main"), "{at}: {free}");
        assert_eq!(free["next"]["wait_until"], Value::Null, "{at}: {free}");
        assert_eq!(pool.launch_on(program.as_deref()), Ok(None), "{at}");

        pool.wall("main", pool.env.now + 7200);
        let (text, doc) = pool.report(program.as_deref());
        let next = &doc["next"];
        if cli == Cli::Codex {
            assert_eq!(next["to"], Value::Null, "{at}: {doc}");
            assert!(!next["wait_until"].is_null(), "{at}: {doc}");
            assert!(
                next_line(&text).starts_with("next: wait until"),
                "{at}: {text}"
            );
            assert!(not_next_json(&doc).contains_key("main"), "{at}: {doc}");
            let refused = pool.launch_on(program.as_deref()).expect_err("walled");
            assert!(refused.contains("no usable account"), "{at}: {refused}");
        } else {
            assert_eq!(next["to"].as_str(), Some("main"), "{at}: {doc}");
            assert_eq!(next["wait_until"], Value::Null, "{at}: {doc}");
            assert!(
                next["to_note"]
                    .as_str()
                    .is_some_and(|note| note.contains("limited until")),
                "{at}: {doc}"
            );
            assert_eq!(pool.launch_on(program.as_deref()), Ok(None), "{at}");
        }

        pool.pin("main");
        let (_, doc) = pool.report(program.as_deref());
        assert_eq!(
            doc["next"]["to"].as_str(),
            Some("main"),
            "{at}: CONTROL: {doc}"
        );
        assert_eq!(doc["next"]["to_note"], Value::Null, "{at}: CONTROL: {doc}");
        assert_eq!(
            pool.launch_on(program.as_deref()),
            Ok(Some("main".to_string())),
            "{at}: CONTROL"
        );
    }
}

/// ON A CODEX THAT RUNS ONLY THIS MACHINE'S OWN LOGIN, `accounts use` OFFERS NO SIGN-IN OF A ROW
/// OF ITS OWN, WHICH THAT BUILD NEVER RUNS: it names the build that would. CONTROL: on a build that
/// keeps the row's login, that row's sign-in is the step.
#[test]
fn use_offers_no_sign_in_of_a_row_the_build_never_runs() {
    let pool = Pool::new("use-old-build");
    pool.no_work_login();
    let login = "accounts login work";
    let err = pool.use_refused("work", &CodexProgram(Some(pool.old.clone())));
    assert!(!err.contains(login), "{err}");
    assert!(
        err.contains(&crate::harness::codex_alab::build_served()),
        "{err}"
    );
    let err = pool.use_refused("work", &CodexProgram(Some(pool.new.clone())));
    assert!(err.contains(login), "CONTROL: {err}");
}

/// A ROW ANOTHER MAC'S WARM SET HOLDS IS THAT MAC'S TO SIGN IN, WHATEVER ASKS: `accounts use` names
/// the holder and no sign-in, and [`sign_in_plan`], which every verb goes through, starts nothing
/// and names it too. CONTROL: with no hold, the same row's plan runs and `use` names its sign-in.
#[test]
fn a_row_held_for_another_mac_is_signed_in_by_that_mac() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "use-held");
        let host = CodexProgram((cli == Cli::Codex).then(|| pool.new.clone()));
        pool.no_work_login();
        let login = format!("accounts {} work", cli.sign_in_verb(false));
        let err = pool.use_refused("work", &host);
        assert!(err.contains(&login), "{at}: CONTROL: {err}");
        let (runs, said) = pool.plan("work", &host);
        assert!(runs, "{at}: CONTROL: {said}");

        pool.change_row("work", |row| row.held_by = Some("other-mac".into()));
        let err = pool.use_refused("work", &host);
        assert!(err.contains("other-mac"), "{at}: {err}");
        assert!(!err.contains(&login), "{at}: {err}");
        let (runs, said) = pool.plan("work", &host);
        assert!(!runs, "{at}: a held row's sign-in starts nothing");
        assert!(said.contains("other-mac"), "{at}: {said}");
    }
}

/// A ROW IN SHARED CUSTODY TAKES A ROOT'S DECISION, WHICH NO SIGN-IN HERE MAKES: `accounts use`
/// names `accounts shared-renew` and no sign-in. CONTROL: the same row out of custody names its
/// sign-in.
#[test]
fn use_of_a_row_in_shared_custody_names_its_shared_renewal() {
    let pool = Pool::new("use-shared");
    let host = CodexProgram(Some(pool.new.clone()));
    pool.no_work_login();
    let login = "accounts login work";
    let err = pool.use_refused("work", &host);
    assert!(err.contains(login), "CONTROL: {err}");
    let store = pool.env.accounts_store_for(Cli::Codex).expect("a store");
    let staging = store.claim_staging_dir("work").expect("staging");
    store
        .transact_logins(pool.env.now - 10, &mut None, |_, logins| {
            let token = logins.shared.begin(
                accounts::shared::Intent {
                    source: accounts::shared::Source {
                        network: [1; 32],
                        account: [2; 32],
                    },
                    decision: [1; 32],
                    generation: 1,
                    configuration: [4; 32],
                    request: [5; 16],
                    label: "work".into(),
                    identity: "work@example.com".into(),
                    staging: staging.to_str().expect("utf-8").into(),
                    before_generation: None,
                    begun_at: pool.env.now - 10,
                },
                None,
            )?;
            logins.shared.finish(
                token,
                accounts::shared::Outcome::Unknown,
                None,
                pool.env.now - 10,
            )?;
            Ok(Vec::new())
        })
        .expect("custody");
    let err = pool.use_refused("work", &host);
    assert!(
        err.contains("accounts shared-renew work --cli codex"),
        "{err}"
    );
    assert!(!err.contains(login), "{err}");
}

/// UNDER CODEX'S LOGIN IN THE OS KEYRING, `accounts use` SAYS WHAT A SIGN-IN WOULD DO THERE, AS THE
/// SIGN-IN'S OWN REFUSAL DOES, AND OFFERS NONE ([`keyring_staging`]) for a missing row login.
/// Legacy rejection history cannot authenticate the hidden default login. CONTROL: with a
/// file store, the missing row's sign-in.
#[test]
fn use_under_a_keyring_login_offers_no_sign_in_that_would_replace_it() {
    let pool = Pool::new("use-keyring");
    let host = CodexProgram(Some(pool.new.clone()));
    pool.no_work_login();
    let login = "accounts login work";
    let err = pool.use_refused("work", &host);
    assert!(err.contains(login), "CONTROL: {err}");
    pool.keyring();
    let err = pool.use_refused("work", &host);
    assert!(err.contains(&keyring_staging("work", false)), "{err}");
    assert!(!err.contains(login), "{err}");
    let now = pool.env.now;
    pool.state(|state| {
        let _ = accounts::note_auth(state, "main", accounts::Auth::SignedOut, now);
    });
    // Hidden keyring credentials cannot inherit a legacy file-independent rejection.
    // The history remains recorded, while the existing unknown-identity launch policy applies.
    let (code, out, err) = pool.run_on("accounts use main --cli codex", &host);
    assert_eq!(code, ExitCode::SUCCESS, "{out}{err}");
    assert_eq!(
        pool.env
            .accounts_store_for(Cli::Codex)
            .unwrap()
            .load(now)
            .unwrap()
            .0
            .accounts["main"]
            .auth,
        accounts::Auth::SignedOut
    );
}

/// A ROW WHOSE RENEWAL STOPPED FOR A PERSON HAS ONE NEXT STEP, IN EVERY SURFACE: `accounts use`
/// names what the report's `sign-in needed:` names (the browser's sign-in, then its renewal), not
/// the plain sign-in. CONTROL: with no task on it, the row's plain sign-in.
#[test]
fn use_of_a_row_with_a_sign_in_task_names_the_reports_step() {
    let pool = Pool::new("use-task");
    let host = CodexProgram(Some(pool.new.clone()));
    pool.no_work_login();
    pool.change_row("work", |row| row.email = Some("work@example.com".into()));
    let err = pool.use_refused("work", &host);
    assert!(err.contains("accounts renew work"), "CONTROL: {err}");
    assert!(!err.contains("accounts browser work"), "CONTROL: {err}");
    let store = pool.env.accounts_store_for(Cli::Codex).expect("a store");
    store
        .transact_pool_logins_within(
            &accounts::LOCK_LADDER,
            &pool.env.accounts_roster_for(Cli::Codex).unwrap(),
            pool.env.now,
            &mut None,
            |_, _, logins| {
                accounts::admit_workflow(logins, "work")?;
                accounts::note_sign_in_task(
                    logins,
                    "work",
                    accounts::SignInAction::Browser,
                    pool.env.now,
                );
                Ok(Vec::new())
            },
        )
        .expect("a task");
    let step = accounts::SignInAction::Browser.next_step(Cli::Codex, "work");
    let err = pool.use_refused("work", &host);
    let tail = step.strip_prefix("Run ").expect("a step");
    assert!(err.contains(tail), "{err}");
}

/// WITH THE PROGRAM A SIGN-IN RUNS NOT HERE, THE INSTALL COMES FIRST: [`sign_in_plan`] starts
/// nothing and names it, and `accounts use` of a row with no login leads with it, then the row's
/// sign-in. CONTROL: with the program here, the plan runs and `use` names no install.
#[test]
fn with_no_program_a_sign_in_and_use_lead_with_the_install() {
    for cli in [Cli::Grok, Cli::Kimi] {
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "sign-in-install");
        let missing = LawBuild::program(
            if cli == Cli::Grok {
                LawBuild::GrokMissing
            } else {
                LawBuild::KimiMissing
            },
            &pool,
        );
        let here = LawBuild::program(
            if cli == Cli::Grok {
                LawBuild::Grok
            } else {
                LawBuild::Kimi
            },
            &pool,
        );
        let install = format!("`aterm pkg install {at}`");
        pool.no_work_login();
        let (runs, said) = pool.plan("work", &missing);
        assert!(!runs, "{at}: nothing starts: {said}");
        assert!(said.contains(&install), "{at}: {said}");
        let err = pool.use_refused("work", &missing);
        let login = format!("accounts {} work", cli.sign_in_verb(false));
        let lead = err.find(&install).unwrap_or_else(|| panic!("{at}: {err}"));
        let then = err.find(&login).unwrap_or_else(|| panic!("{at}: {err}"));
        assert!(lead < then, "{at}: {err}");

        let (runs, said) = pool.plan("work", &here);
        assert!(runs, "{at}: CONTROL: {said}");
        assert!(!said.contains(&install), "{at}: CONTROL: {said}");
        let err = pool.use_refused("work", &here);
        assert!(!err.contains(&install), "{at}: CONTROL: {err}");
        assert!(err.contains(&login), "{at}: CONTROL: {err}");
    }
}

/// THE REPORT JUDGES ROTATION AS A LAUNCH READS IT, BY THE SWITCH THAT TURNS IT OFF: `[harness]
/// enabled = false`, and `[harness.<agent>] enabled = false` where the agent has such a table
/// (Codex; Grok Build and Kimi Code have none, so that table turns nothing off), each say `rotation
/// off:` naming that switch, and `next.off` says what is left. CONTROL: the switch of another agent
/// leaves this pool's rotation on.
#[test]
fn a_harness_or_agent_switch_turns_rotation_off_in_the_report_as_in_a_launch() {
    for (cli, other) in [
        (Cli::Codex, "grok"),
        (Cli::Grok, "kimi"),
        (Cli::Kimi, "codex"),
    ] {
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "rotation-switch");
        let program = (cli == Cli::Codex).then(|| pool.new.clone());
        let agent = cli.program().name();
        let own_switch = format!("[harness.{agent}]\nenabled = false\n");
        let mut switches = vec![(
            "[harness]\nenabled = false\n".to_string(),
            "`[harness] enabled = false`".to_string(),
        )];
        if cli.program().harness_table() {
            switches.push((
                own_switch.clone(),
                format!("`[harness.{agent}] enabled = false`"),
            ));
        } else {
            pool.config(&own_switch);
            let (_, doc) = pool.report(program.as_deref());
            assert_eq!(
                doc["next"]["off"],
                Value::Null,
                "{at}: no switch of its own"
            );
        }
        for (config, names) in switches {
            pool.config(&config);
            let (text, doc) = pool.report(program.as_deref());
            assert!(
                doc["next"]["off"].as_str().is_some(),
                "{at}: {config}: {doc}"
            );
            let line = text
                .lines()
                .find(|line| line.starts_with("rotation off: "))
                .unwrap_or_else(|| panic!("{at}: {config}: {text}"));
            assert!(line.contains(&names), "{at}: {line}");
        }
        pool.config(&format!("[harness.{other}]\nenabled = false\n"));
        let (text, doc) = pool.report(program.as_deref());
        assert_eq!(doc["next"]["off"], Value::Null, "{at}: CONTROL: {doc}");
        assert!(!text.contains("rotation off: "), "{at}: CONTROL: {text}");
    }
}

/// `next:` IS WHAT A NEW LAUNCH TAKES; A LIVE SESSION'S WALL IS ITS OWN TO ANSWER: for each row a
/// session of a Codex build that keeps a row's login can run on, the report says the row its weekly
/// limit moves it to (`wall_moves`), with no session recorded (no writer records a Codex session's
/// row on a real Mac), beside a `next:` that still names `main`. CONTROL: on a build that runs only
/// `main`, with no build, under rotation off, in the OS keyring, or on a CLI no running session of
/// which moves, nothing is said.
#[test]
fn the_report_says_where_a_wall_moves_a_running_session() {
    let moves = |doc: &Value| -> Vec<(String, String)> {
        doc["next"]["wall_moves"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        (
                            row["from"].as_str().expect("from").to_string(),
                            row["to"].as_str().expect("to").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let pool = Pool::new("wall-moves");
    let (text, doc) = pool.report(Some(&pool.new));
    assert_eq!(
        moves(&doc),
        vec![
            ("main".to_string(), "work".to_string()),
            ("work".to_string(), "main".to_string())
        ],
        "{doc}"
    );
    assert_eq!(doc["next"]["to"].as_str(), Some("main"), "{doc}");
    assert!(
        text.contains("at a weekly limit: a session on `main` moves to `work`"),
        "{text}"
    );

    let (text, doc) = pool.report(Some(&pool.old));
    assert!(
        moves(&doc).is_empty(),
        "a build that runs only `main`: {doc}"
    );
    assert!(
        !text.contains("at a weekly limit:"),
        "a build that runs only `main`: {text}"
    );
    let absent = pool.law_report(&LawBuild::CodexMissing.program(&pool));
    assert!(absent.wall_moves.is_empty(), "no build: {absent:?}");

    pool.mode("off");
    let (text, doc) = pool.report(Some(&pool.new));
    assert!(moves(&doc).is_empty(), "CONTROL: rotation off: {doc}");
    assert!(
        !text.contains("at a weekly limit:"),
        "CONTROL: rotation off: {text}"
    );
    pool.mode("early");
    pool.keyring();
    let (_, doc) = pool.report(Some(&pool.new));
    assert!(moves(&doc).is_empty(), "CONTROL: the OS keyring: {doc}");
    for cli in [Cli::Grok, Cli::Kimi] {
        if !links_here(cli) {
            continue;
        }
        let pool = Pool::of(cli, "wall-moves-unmeasured");
        let (_, doc) = pool.report(None);
        assert!(moves(&doc).is_empty(), "{}: CONTROL: {doc}", cli.id());
    }
}

/// A ROW WITH NO CURRENT LOGIN SAYS SO IN ONE PHRASE, IN TEXT AND JSON, ON EVERY BUILD AND CLI, WITH
/// THE ROW'S OWN SIGN-IN: `accounts <verb> <label>` by [`Cli::sign_in_verb`], signed out or never
/// signed in. The phrase holds no `; `, the report's own separator between rows.
#[test]
fn a_row_with_no_current_login_says_so_in_one_phrase_with_its_own_sign_in() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "not-next-phrase");
        let program = (cli == Cli::Codex).then(|| pool.new.clone());
        pool.no_work_login();
        pool.signed_out("main");
        let (text, doc) = pool.report(program.as_deref());
        let rows = not_next(&text);
        assert_eq!(
            reason_words(rows.clone()),
            not_next_json(&doc),
            "{at}: {text}"
        );
        for (label, addressed, word) in [
            ("main", true, "signed-out"),
            ("work", false, "not-enrolled"),
        ] {
            let why = &rows[label];
            assert!(why.starts_with(word), "{at}: {label}: {why}");
            let verb = cli.sign_in_verb(addressed);
            assert!(
                why.contains(&format!("accounts {verb} {label} --cli {at}")),
                "{at}: {label}: {why}"
            );
        }
    }
}

/// THE REFUSAL OF `accounts renew` FOR A ROW WITH NO EMAIL NAMES ONLY A STEP THAT WORKS, NEVER AN
/// EDIT THAT BREAKS THE ROSTER: where the row's login names an account another row holds, it names
/// that row, and never an address, which would be a second row of the account and refuse the whole
/// roster; where the lab names this Mac, whose rows come from it, the sign-in alone. On a build
/// that runs only this machine's own login, that refusal comes first. CONTROL: on a Mac no lab
/// names and with no twin, the address is an option, one no other row names.
#[test]
fn a_renewal_with_no_email_names_a_step_that_keeps_the_roster_admitted() {
    /// A Codex whose default login names one account.
    struct Named {
        program: PathBuf,
        account: Option<String>,
    }
    impl AccountsHost for Named {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            panic!("no Claude probe")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no login")
        }
        fn now(&self) -> i64 {
            0
        }
        fn codex_launch_program(&self) -> Option<PathBuf> {
            Some(self.program.clone())
        }
        fn codex_account(&self, _: &Path) -> Option<String> {
            self.account.clone()
        }
    }
    let pool = Pool::new("renew-no-email");
    let twin = pool.tmp.path().join("logins/codex/codex-7d229c");
    std::fs::create_dir_all(&twin).expect("the twin's login directory");
    pool.rows(&[
        accounts::Row {
            label: "main".into(),
            ..accounts::Row::default()
        },
        accounts::Row {
            label: "codex-7d229c".into(),
            dir: Some(twin.display().to_string()),
            email: Some("owner@example.com".into()),
            ..accounts::Row::default()
        },
    ]);
    let renew = |host: &Named| {
        let (code, out, err) = pool.run_on("accounts renew main --cli codex", host);
        assert_eq!(code, ExitCode::from(1), "{out}{err}");
        err
    };
    let edit = "add `email";
    let err = renew(&Named {
        program: pool.new.clone(),
        account: Some("owner@example.com".into()),
    });
    assert!(
        err.contains("accounts renew codex-7d229c --cli codex"),
        "{err}"
    );
    assert!(!err.contains(edit), "{err}");

    let no_twin = Named {
        program: pool.new.clone(),
        account: None,
    };
    let lab = crate::harness::alab_roster::Lab {
        me: Some("m16".into()),
        settled: true,
        accounts: Vec::new(),
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let err = crate::harness::alab_roster::with_lab(lab, || renew(&no_twin));
    assert!(err.contains("accounts login main --cli codex"), "{err}");
    assert!(!err.contains(edit), "{err}");

    let err = renew(&no_twin);
    assert!(err.contains(edit), "CONTROL: {err}");
    assert!(
        err.contains("accounts login main --cli codex"),
        "CONTROL: {err}"
    );

    let old = pool.old.clone();
    let why = crate::harness::codex_alab::auth_path_unread(&old).expect("an old build");
    let err = renew(&Named {
        program: old,
        account: Some("owner@example.com".into()),
    });
    assert!(
        err.contains(&crate::harness::codex_alab::default_row_staging(
            "main", &why
        )),
        "{err}"
    );
}

/// TWO ROWS NAMING ONE ACCOUNT ARE REFUSED WITH WHICH TO REMOVE: the row with a login directory of
/// its own beside the row on this machine's own login (which keeps the login this machine runs),
/// else the later row. A Kimi Code row's `region` alone admits, so the refusal of its sign-in may
/// ask for that; an `email` with no region refuses the roster, so it must not be the step.
#[test]
fn a_refused_pair_of_rows_says_which_to_remove() {
    let refusal =
        |rows: &str| Roster::parse_for(Cli::Codex, rows).expect_err("one account in two rows");
    let row = |label: &str, dir: &str| {
        format!(
            "[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\n{dir}email = \"a@example.com\"\n"
        )
    };
    let own = row("main", "");
    let dir = row("lab", "dir = \"/logins/lab\"\n");
    let dir2 = row("two", "dir = \"/logins/two\"\n");
    for (rows, drop, keep) in [
        (format!("{own}{dir}"), "lab", "main"),
        (format!("{dir}{own}"), "lab", "main"),
        (format!("{dir}{dir2}"), "two", "lab"),
    ] {
        let why = refusal(&rows);
        assert!(
            why.contains(&format!("remove `{drop}` and keep `{keep}`")),
            "{why}"
        );
    }
    let kimi = "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n";
    assert!(
        Roster::parse_for(Cli::Kimi, &format!("{kimi}region = \"global\"\n")).is_ok(),
        "a region alone admits"
    );
    assert!(
        Roster::parse_for(Cli::Kimi, &format!("{kimi}email = \"a@example.com\"\n")).is_err(),
        "an address with no region does not, so it is no step"
    );
}

/// CLAUDE CODE'S REPORT READS ITS ROW ON THIS MACHINE'S OWN LOGIN AS THE DOCTOR DOES: with neither
/// a profile naming an account nor a login file in its home, the row has no login here, so it is
/// not enrolled and its login is not `ok`, whatever its label and records say. CONTROL: the profile
/// naming an account, and the login file alone, each make it the row it was.
#[test]
fn a_claude_row_on_the_default_login_is_reported_as_the_doctor_sees_it() {
    let mut pool = Pool::of(Cli::Claude, "claude-default-login");
    let home = pool.env.home.clone().expect("a home");
    let credentials = home.join(".claude/.credentials.json");
    let profile = pool.tmp.path().join("claude.json");
    pool.env.claude_profile = Some(profile.clone());
    let store = pool.env.accounts_store_for(Cli::Claude).expect("a store");
    let now = pool.env.now;
    store
        .transact_logins(now, &mut None, |_, logins| {
            accounts::note_served_login(logins, "main", now);
            Ok(Vec::new())
        })
        .expect("a login served");
    let main = |pool: &Pool| -> (String, String) {
        let (_, doc) = pool.report(None);
        let rows = doc["accounts"].as_array().expect("the rows");
        let row = rows
            .iter()
            .find(|row| row["label"].as_str() == Some("main"))
            .expect("main");
        (
            row["condition"].as_str().expect("a condition").to_string(),
            row["login"].as_str().expect("a login word").to_string(),
        )
    };
    assert_eq!(
        main(&pool),
        ("unseen".to_string(), "ok".to_string()),
        "CONTROL: its login file"
    );
    std::fs::remove_file(&credentials).expect("the login file");
    let (condition, login) = main(&pool);
    assert_eq!(condition, "not-enrolled", "no login here");
    assert_ne!(login, "ok", "no login here");
    std::fs::write(
        &profile,
        "{\"oauthAccount\":{\"accountUuid\":\"00000000-0000-4000-8000-000000000001\",\
         \"emailAddress\":\"owner@example.com\"}}",
    )
    .expect("a profile");
    assert_eq!(
        main(&pool),
        ("unseen".to_string(), "ok".to_string()),
        "CONTROL: a profile that names an account"
    );
}

/// WHY A DRIVEN SIGN-IN STOPPED IS SAID WHOLE ON ONE LINE: the end of a long reason is what says
/// what to do, so nothing clips it.
#[test]
fn a_driven_sign_ins_reason_is_said_whole_on_one_line() {
    use crate::harness::login_drive::Outcome;
    let long = format!("{}\nends here", "a reason ".repeat(60));
    for outcome in [
        Outcome::Refused(long.clone()),
        Outcome::NeedsYou(long.clone()),
        Outcome::NotRun(long.clone()),
        Outcome::BotCheck(long.clone()),
    ] {
        let words = drove_words(&outcome);
        assert!(words.contains("ends here"), "{words}");
        assert_eq!(words.lines().count(), 1, "{words}");
    }
}

/// THE SIGN-IN aterm RUNS ITSELF IS SAID TO BE aterm's: Codex's browser route runs in process, with
/// no vendor program. CONTROL: its device route runs the vendor's own.
#[test]
fn the_sign_in_aterm_runs_itself_is_said_to_be_aterms() {
    use crate::ai::connector::{Drive, Flow};
    let signin = Cli::Codex.accounts().signin.as_ref().expect("a sign-in");
    let browser = signin
        .route(Some(Flow::Browser))
        .expect("the browser route");
    assert!(matches!(browser.drive, Drive::CodexPkce), "PRECONDITION");
    assert!(sign_in_words(Cli::Codex, "x", browser, &[]).starts_with("aterm's "));
    let device = signin.route(Some(Flow::Device)).expect("the device route");
    assert!(!matches!(device.drive, Drive::CodexPkce), "PRECONDITION");
    assert!(!sign_in_words(Cli::Codex, "x", device, &[]).starts_with("aterm's "));
}

/// `accounts use` AND THE REPORT SAY ONE THING OF A RUNNING SESSION: where its weekly limit moves a
/// session on `main` the words point at the report's `at a weekly limit:` line; where nothing moves
/// one (a build that runs only `main`, rotation off, the OS keyring, a CLI none of whose running
/// sessions moves) they do not, and the report has no such line. Each is read of a pin and of its
/// end.
#[test]
fn use_and_the_report_say_one_thing_of_a_running_session() {
    type World = (&'static str, Cli, LawBuild, fn(&Pool), bool);
    let worlds: [World; 6] = [
        (
            "a build that keeps a row's login",
            Cli::Codex,
            LawBuild::CodexNew,
            |_| {},
            true,
        ),
        (
            "a build that runs only `main`",
            Cli::Codex,
            LawBuild::CodexOld,
            |_| {},
            false,
        ),
        (
            "rotation off",
            Cli::Codex,
            LawBuild::CodexNew,
            |pool| pool.mode("off"),
            false,
        ),
        (
            "the OS keyring",
            Cli::Codex,
            LawBuild::CodexNew,
            Pool::keyring,
            false,
        ),
        ("Grok Build", Cli::Grok, LawBuild::Grok, |_| {}, false),
        ("Kimi Code CLI", Cli::Kimi, LawBuild::Kimi, |_| {}, false),
    ];
    for (world, cli, build, set, moves) in worlds {
        if !links_here(cli) {
            continue;
        }
        let pool = Pool::of(cli, "use-and-the-wall");
        let host = build.program(&pool);
        set(&pool);
        for label in [Some("main"), None] {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let (code, _) =
                accounts_use_decided((cli, &host), &pool.env, label, &mut out, &mut err);
            let (out, err) = (String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
            assert_eq!(code, ExitCode::SUCCESS, "{world}: {label:?}: {err}");
            assert_eq!(
                out.contains("`at a weekly limit:`"),
                moves,
                "{world}: {label:?}: {out}"
            );
        }
        let report = pool.law_report(&host);
        assert_eq!(!report.wall_moves.is_empty(), moves, "{world}: {report:?}");
    }
}

/// A ROW NO SESSION HERE RUNS ON IS TOLD IT WAITS FOR ITS SIGN-IN ONLY WHERE THAT SIGN-IN HAS A ROUTE:
/// not under `[harness] logins = "off"`, which keeps that off and is named, and not with the CLI not
/// there, whose install leads, on the `not next:` line and on Grok Build's `sign-in needed:` (Codex's
/// too: aterm's browser sign-in runs in process, and what it signs in runs nothing until Codex is
/// there). A Kimi Code CLI row says it only while its ledger's last word on its browser is a window
/// a person signed in: not on a profile directory a stopped attempt made, another row's window, or
/// after a later attempt stopped for a person, where it names the step that gives it a route.
/// Nothing here promises a sign-in by itself: `claudes_report_says_by_itself_only_where_this_mac_uses_the_row`
/// holds that promise to this Mac's use of the row. CONTROL: with neither switch nor absence, Kimi Code CLI's row
/// with that window and Codex's say it, the sign-in they name there too.
#[test]
fn a_row_nobody_runs_on_waits_only_where_its_sign_in_has_a_route() {
    let line = |text: &str, start: &str| -> String {
        text.lines()
            .find(|line| line.trim_start().starts_with(start))
            .unwrap_or_else(|| panic!("no `{start}`: {text}"))
            .to_string()
    };
    for (cli, build, gone, needs_program) in [
        (Cli::Codex, LawBuild::CodexNew, LawBuild::CodexMissing, true),
        (Cli::Kimi, LawBuild::Kimi, LawBuild::KimiMissing, true),
    ] {
        if !links_here(cli) {
            continue;
        }
        let at = cli.id();
        let pool = Pool::of(cli, "signs-in-by-itself");
        pool.no_work_login();
        pool.change_row("work", |row| row.email = Some("work@example.com".into()));
        let renew = format!("aterm harness accounts renew work --cli {at}");
        if cli == Cli::Kimi {
            let no_route = |why: &str| {
                let (text, _) = pool.report_on(&build.program(&pool));
                let said = line(&text, "not next:");
                assert!(!said.contains("waits for"), "{why}: {text}");
                assert!(
                    said.contains("aterm harness accounts browser work --cli kimi"),
                    "{why}: {text}"
                );
            };
            no_route("nothing on record");
            let store = pool.env.accounts_store_for(cli).unwrap();
            let shared = store.shared_dir();
            std::fs::create_dir_all(crate::harness::headless_oauth::row_browser_profile(
                cli,
                &shared,
                &shared,
                "work",
                Some("work@example.com"),
            ))
            .unwrap();
            no_route("a directory an attempt made");
            let sign_in = |label, at, why: &str| {
                accounts::Event::new(at, accounts::Kind::SignIn, label).because(why)
            };
            let window = "www.kimi.ai window done session-kept";
            let now = pool.env.now;
            store.save(&accounts::State::default()).unwrap();
            store.append(&[sign_in("main", now - 3, window)]).unwrap();
            no_route("another row's window");
            store
                .append(&[
                    sign_in("work", now - 2, window),
                    sign_in("work", now - 1, "www.kimi.ai device needs-you"),
                ])
                .unwrap();
            no_route("an attempt that stopped for a person since");
            store.append(&[sign_in("work", now, window)]).unwrap();
        }
        let (text, _) = pool.report_on(&build.program(&pool));
        let said = line(&text, "not next:");
        assert!(
            said.contains("waits for") && said.contains(&renew) && !said.contains("by itself"),
            "CONTROL: {at}: {text}"
        );
        {
            let store = pool.env.accounts_store_for(cli).unwrap();
            if cli == Cli::Codex {
                store.save(&accounts::State::default()).unwrap();
            }
            let roster = pool.env.accounts_roster_for(cli).unwrap();
            let sign_in = |at, why: &str| {
                store
                    .transact_pool_logins_within(&[], &roster, at, &mut None, |_, _, logins| {
                        accounts::admit_workflow(logins, "work")?;
                        Ok(vec![
                            accounts::Event::new(at, accounts::Kind::SignIn, "work").because(why),
                        ])
                    })
                    .unwrap();
            };
            let now = pool.env.now + 10;
            sign_in(now - 1, "auth.vendor.example browser bot-check");
            let (text, _) = pool.report_on(&build.program(&pool));
            let said = line(&text, "not next:");
            assert!(
                !said.contains("waits for"),
                "{at}: a bot check on record: {text}"
            );
            assert!(said.contains("accounts browser work"), "{at}: {text}");
            sign_in(now, "auth.vendor.example window done session-kept");
            let (text, _) = pool.report_on(&build.program(&pool));
            assert!(
                line(&text, "not next:").contains("waits for"),
                "CONTROL: a signed-in window since: {text}"
            );
        }
        pool.config("[harness]\nlogins = \"off\"\n");
        let (text, _) = pool.report_on(&build.program(&pool));
        let said = line(&text, "not next:");
        assert!(!said.contains("waits for"), "{at}: logins off: {text}");
        assert!(said.contains("logins"), "{at}: names the switch: {text}");
        assert!(said.contains(&renew), "{at}: logins off: {text}");
        pool.config("");
        let (text, _) = pool.report_on(&gone.program(&pool));
        let said = line(&text, "not next:");
        assert_eq!(
            said.contains("waits for"),
            !needs_program,
            "{at}: no program: {text}"
        );
        let install = format!("aterm pkg install {at}");
        assert_eq!(
            said.find(&install)
                .is_some_and(|at| said.find(&renew).is_some_and(|step| at < step)),
            needs_program,
            "{at}: the install leads: {text}"
        );
    }
    if links_here(Cli::Grok) {
        let pool = Pool::of(Cli::Grok, "signs-in-grok");
        pool.no_work_login();
        let (text, _) = pool.report_on(&LawBuild::GrokMissing.program(&pool));
        for start in ["not next:", "sign-in needed:"] {
            let said = line(&text, start);
            assert!(said.contains("aterm pkg install grok"), "{start}: {text}");
        }
        let (text, _) = pool.report_on(&LawBuild::Grok.program(&pool));
        for start in ["not next:", "sign-in needed:"] {
            let said = line(&text, start);
            assert!(
                !said.contains("aterm pkg install"),
                "CONTROL: {start}: {text}"
            );
        }
    }
}

/// CLAUDE CODE'S REPORT SAYS aterm SIGNS A ROW IN BY ITSELF ONLY WHERE THIS MAC USES IT AND A
/// ROUTE DOES IT WITH NOBODY AT THIS MAC (`DESIGN-alab-one-mesh-2026-11-05` §4.15): a row signed out,
/// with no mailbox and no profile, names the browser step that gives it one; with a mailbox and no
/// session here on it, ever, it waits for its sign-in, a person's; once a session here ran on it,
/// aterm signs it in by itself for a week, and it waits again after. CONTROL: with a session on
/// it, it says it.
#[test]
fn claudes_report_says_by_itself_only_where_this_mac_uses_the_row() {
    let tmp = tmp_dir("claude-route");
    let mut env = env_at(&tmp);
    env.sock = None;
    let home = env.home.clone().expect("a home");
    std::fs::create_dir_all(&home).expect("home");
    let (code, out, err) = run_line(
        "accounts adopt-default main --email owner@example.com",
        &env,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let (code, out, err) = run_line("accounts add work --email work@example.com", &env);
    assert_eq!(code, exit(0), "{out}{err}");
    let store = env.accounts_store();
    store
        .transact(env.now, &mut None, |state| {
            accounts::note_auth(state, "work", accounts::Auth::SignedOut, env.now);
            Ok(Vec::new())
        })
        .expect("the sign-out");
    let not_next = |env: &Env| -> String {
        let (code, out, err) = run_line("accounts", env);
        assert_eq!(code, exit(0), "{err}");
        out.lines()
            .find(|line| line.starts_with("not next:"))
            .unwrap_or_else(|| panic!("no `not next:`: {out}"))
            .to_string()
    };
    let said = not_next(&env);
    assert!(!said.contains("by itself"), "no route: {said}");
    assert!(
        said.contains("aterm harness accounts browser work"),
        "no route: {said}"
    );
    std::fs::create_dir_all(
        store
            .dir
            .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    )
    .expect("a mailbox");
    let said = not_next(&env);
    assert!(!said.contains("by itself"), "no session on it: {said}");
    assert!(
        said.contains("no login made yet"),
        "no session on it: {said}"
    );
    assert!(
        said.contains(
            "no session here runs on it or ran on it within a week, so it waits for `aterm \
             harness accounts renew work`"
        ),
        "no session on it: {said}"
    );
    store
        .transact(env.now, &mut None, |state| {
            let ran = accounts::note_session(state, "s-ran", "work", None, env.now);
            state.sessions.clear();
            Ok(ran)
        })
        .expect("a session that ran on the row and ended");
    let said = not_next(&env);
    assert!(said.contains("by itself"), "a session ran on it: {said}");
    assert!(!said.contains("waits for"), "a session ran on it: {said}");
    let week_on = Env {
        now: env.now + accounts::USED_WITHIN_S,
        ..env.clone()
    };
    let said = not_next(&week_on);
    assert!(!said.contains("by itself"), "a week on: {said}");
    assert!(
        said.contains("within a week, so it waits for"),
        "a week on: {said}"
    );
    // A login made here once stays one that went, however long ago its session ran.
    assert!(
        said.contains("its login here has gone"),
        "a week on: {said}"
    );
    store
        .transact(env.now, &mut None, |state| {
            Ok(accounts::note_session(
                state, "s-work", "work", None, env.now,
            ))
        })
        .expect("a session on the row");
    let said = not_next(&env);
    assert!(said.contains("by itself"), "CONTROL: a session: {said}");
    assert!(!said.contains("waits for"), "CONTROL: a session: {said}");
}

/// `accounts login` WITH NO TERMINAL SAYS WHAT A TERMINAL WOULD NOT MEND FIRST: a program that is not
/// here, as `accounts renew` does. CONTROL: with the program here, the terminal is what is missing.
#[test]
fn a_login_with_no_terminal_says_the_missing_program_first() {
    if !links_here(Cli::Kimi) {
        return;
    }
    let pool = Pool::of(Cli::Kimi, "login-no-program");
    pool.no_work_login();
    let run = |build: LawBuild| {
        let host = build.program(&pool);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_accounts_login_for(Cli::Kimi, &pool.env, &host, "work", &mut out, &mut err);
        assert_eq!(code, ExitCode::from(1));
        String::from_utf8_lossy(&err).into_owned()
    };
    let said = run(LawBuild::KimiMissing);
    assert!(said.contains("aterm pkg install kimi"), "{said}");
    assert!(!said.contains("stdin is not a terminal"), "{said}");
    let said = run(LawBuild::Kimi);
    assert!(said.contains("stdin is not a terminal"), "CONTROL: {said}");
}

/// `accounts use` READS A CLAUDE CODE ROW'S LOGIN AS THE REPORT DOES: the default row of a home whose
/// profile names no account is `not-enrolled` in the report, and a pin on it is refused. CONTROL:
/// once the profile names one, the pin is recorded.
#[test]
fn use_refuses_the_claude_row_the_report_calls_not_signed_in() {
    let tmp = tmp_dir("claude-use");
    let mut env = env_at(&tmp);
    env.sock = None;
    let home = env.home.clone().expect("a home");
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    let profile = home.join(".claude.json");
    env.claude_profile = Some(profile.clone());
    let (code, out, err) = run_line(
        "accounts adopt-default main --email owner@example.com",
        &env,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let (code, out, err) = run_line("accounts use main", &env);
    assert_ne!(code, exit(0), "{out}{err}");
    std::fs::write(
        &profile,
        r#"{"oauthAccount":{"accountUuid":"00000000-0000-4000-8000-000000000001"}}"#,
    )
    .expect("the profile");
    let (code, out, err) = run_line("accounts use main", &env);
    assert_eq!(code, exit(0), "CONTROL: {out}{err}");
}

/// A ROW A PAGE STOPPED NAMES THE RENEWAL OF ITS OWN CLI: the command in `not next:` is parsed as a
/// command, and runs `renew` on the row with `--cli <id>` for a pool; Claude Code's is the default
/// and takes none.
#[test]
fn a_stopped_row_names_the_renewal_of_its_own_cli() {
    let command = |cli: Cli| -> Vec<String> {
        let step = stopped_step(cli, "work");
        let quoted = step.split('`').nth(1).expect("a command");
        quoted.split_whitespace().map(str::to_owned).collect()
    };
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        assert_eq!(
            command(cli),
            [
                "aterm",
                "harness",
                "accounts",
                "renew",
                "work",
                "--cli",
                cli.id()
            ],
            "{}",
            cli.id()
        );
    }
    assert_eq!(
        command(Cli::Claude),
        ["aterm", "harness", "accounts", "renew", "work"]
    );
}

impl Pool {
    /// `work` in shared custody: a shared renewal it began and whose outcome is unknown.
    fn work_in_custody(&self) {
        let store = self.env.accounts_store_for(self.cli).expect("a store");
        let staging = store.claim_staging_dir("work").expect("staging");
        store
            .transact_logins(self.env.now - 10, &mut None, |_, logins| {
                let token = logins.shared.begin(
                    accounts::shared::Intent {
                        source: accounts::shared::Source {
                            network: [1; 32],
                            account: [2; 32],
                        },
                        decision: [1; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "work".into(),
                        identity: "work@example.com".into(),
                        staging: staging.to_str().expect("utf-8").into(),
                        before_generation: None,
                        begun_at: self.env.now - 10,
                    },
                    None,
                )?;
                logins.shared.finish(
                    token,
                    accounts::shared::Outcome::Unknown,
                    None,
                    self.env.now - 10,
                )?;
                Ok(Vec::new())
            })
            .expect("custody");
    }
}

/// THE REPORT NAMES THE ONE STEP A ROW IN SHARED CUSTODY HAS, AS `accounts use` DOES: the
/// `shared-renew` a root decides, and no sign-in, which `renew` would refuse. CONTROL: out of
/// custody its `not next:` names the row's sign-in.
#[test]
fn the_report_names_a_shared_rows_shared_renewal() {
    let pool = Pool::new("report-shared");
    pool.no_work_login();
    pool.change_row("work", |row| row.email = Some("work@example.com".into()));
    let (text, _) = pool.report(Some(&pool.new));
    let step = "accounts shared-renew work --cli codex";
    let sign_in = "accounts renew work --cli codex";
    assert!(not_next(&text)["work"].contains(sign_in), "CONTROL: {text}");
    pool.work_in_custody();
    let (text, _) = pool.report(Some(&pool.new));
    let words = &not_next(&text)["work"];
    assert!(words.contains(step), "{text}");
    assert!(!words.contains(sign_in), "{text}");
}

/// WITH NO CODEX INSTALLED, `accounts use` AND THE REPORT LEAD WITH THAT: not a build's fault, nor
/// aterm signing the row in by itself. CONTROL: an old build there is what `use` names.
#[test]
fn an_uninstalled_codex_is_named_first_by_use_and_by_the_report() {
    let pool = Pool::new("use-uninstalled");
    pool.no_work_login();
    let gone = LawBuild::CodexMissing.program(&pool);
    let lead = not_installed_words(Cli::Codex);
    let build = crate::harness::codex_alab::build_served();
    let build = build.as_str();
    let err = pool.use_refused("work", &gone);
    assert!(err.contains(&lead), "{err}");
    assert!(!err.contains(build), "{err}");
    let (_, text, _) = pool.run_on("accounts --cli codex", &gone);
    let words = &not_next(&text)["work"];
    assert!(words.contains(&lead), "{text}");
    assert!(!words.contains("by itself"), "{text}");
    let err = pool.use_refused("work", &CodexProgram(Some(pool.old.clone())));
    assert!(err.contains(build), "CONTROL: {err}");
}

/// THE REVIEW OF CLAUDE CODE UNDER ROTATION OFF STARTS WHERE A NEW SESSION DOES, ON THE LOGIN IT IS
/// STARTED WITH: with `main` at its wall, the pool's pick is `work`, and under `accounts = "off"`
/// the review takes `main`, as the report says a new session does.
#[test]
fn a_claude_review_under_rotation_off_takes_the_login_it_is_started_with() {
    let pool = Pool::of(Cli::Claude, "claude-review-off");
    let now = pool.env.now;
    pool.wall("main", now + 7200);
    let review = |pool: &Pool| {
        let home = pool.env.home.clone().expect("a home");
        let shown = home.display().to_string();
        let var = move |name: &str| (name == "HOME").then(|| shown.clone());
        let machine = crate::ai::facts::Machine {
            now,
            home: Some(home),
            var: &var,
            pkg: None,
            exclude: Vec::new(),
            records: Default::default(),
            harness_state: Some(pool.env.state.clone()),
            roster: pool.env.accounts_roster_for(Cli::Claude),
            accounts_mode: None,
            config: pool.env.config.clone(),
        };
        let mut fact = crate::ai::facts::gather(&machine, Cli::Claude.connector());
        crate::ai::review::review_row(&mut fact, now, pool.env.config.as_deref())
    };
    let captured = |pool: &Pool, mode| {
        let roster = pool.env.accounts_roster_for(Cli::Claude).expect("a roster");
        let store = pool.env.accounts_store_for(Cli::Claude).expect("a store");
        crate::harness::accounts_sync::capture_current_review(
            &store,
            Cli::Claude,
            Some(&roster),
            mode,
            None,
            now,
        )
        .expect("a review's login")
        .witness
        .row()
        .map(|row| row.label.clone())
    };
    assert_eq!(
        review(&pool),
        Ok(Some("work".to_string())),
        "CONTROL: rotating"
    );
    assert_eq!(
        captured(&pool, accounts::Mode::Early),
        Some("work".to_string()),
        "CONTROL: rotating"
    );
    pool.mode("off");
    assert_eq!(review(&pool), Ok(Some("main".to_string())));
    assert_eq!(
        captured(&pool, accounts::Mode::Off),
        Some("main".to_string())
    );
}

/// The default Codex login of `pool` signed in as `address`.
fn default_login_names(pool: &Pool, address: &str) {
    let home = pool.env.home.as_deref().expect("a home");
    law_auth(&home.join(".codex/auth.json"), "moved", Some(address));
}

/// A ROW THE DEFAULT LOGIN MOVED OFF IS SAID IN THE REPORT: it is not next, with what that login
/// names and the sync that answers for it, in words and JSON, and a lone row is judged so too.
/// CONTROL: a login on the row's own address says nothing of it.
#[test]
fn the_report_says_a_row_the_default_login_moved_off() {
    let pool = Pool::new("report-moved");
    let host = CodexProgram(Some(pool.new.clone()));
    default_login_names(&pool, "owner@example.com");
    let (text, json) = pool.report_on(&host);
    assert!(!not_next(&text).contains_key("main"), "CONTROL: {text}");
    assert!(!text.contains("the default login names"), "CONTROL: {text}");

    default_login_names(&pool, "far@example.com");
    let (text, json_moved) = pool.report_on(&host);
    let words = "the default login names far@example.com: `aterm harness accounts login main \
                 --cli codex` signs this row in again";
    assert_eq!(
        not_next(&text).get("main").map(String::as_str),
        Some(words),
        "{text}"
    );
    assert_eq!(next_line(&text), "next: work", "{text}");
    let row = json_moved["next"]["not"]
        .as_array()
        .expect("not")
        .iter()
        .find(|row| row["label"] == Value::from("main"))
        .unwrap_or_else(|| panic!("{json_moved}"));
    assert_eq!(row["why"], Value::from(words));
    assert_ne!(json["next"], json_moved["next"], "CONTROL");

    pool.rows(&[accounts::Row {
        label: "main".into(),
        email: Some("owner@example.com".into()),
        ..accounts::Row::default()
    }]);
    let (text, _) = pool.report_on(&host);
    assert_eq!(
        not_next(&text).get("main").map(String::as_str),
        Some(words),
        "a lone row: {text}"
    );
}

/// THE REPORT NAMES THE SYNC THAT FOLDS A TWIN ONLY WHERE IT DOES: with a source to read and a twin
/// that holds nothing it names it, and that sync folds it. With no source to read, or a login of the
/// twin's own, it says both stay, and the sync it no longer names would have done nothing.
#[test]
fn the_report_names_the_sync_that_folds_a_twin_only_where_one_does() {
    let pool = Pool::new("report-twin");
    let host = CodexProgram(Some(pool.new.clone()));
    let twin = pool.tmp.path().join("logins/codex/twin");
    std::fs::create_dir_all(&twin).expect("the twin's directory");
    pool.rows(&[
        accounts::Row {
            label: "main".into(),
            ..accounts::Row::default()
        },
        accounts::Row {
            label: "twin".into(),
            dir: Some(twin.display().to_string()),
            email: Some("ann@example.com".into()),
            ..accounts::Row::default()
        },
    ]);
    default_login_names(&pool, "ann@example.com");
    let line = |text: &str| -> String {
        text.lines()
            .find(|line| line.starts_with("one account:"))
            .unwrap_or_else(|| panic!("{text}"))
            .to_string()
    };
    let (text, json) = pool.report_on(&host);
    assert!(line(&text).contains("both stay"), "no source: {text}");
    assert!(!line(&text).contains("accounts sync"), "no source: {text}");
    assert_eq!(json["next"]["twin"]["sync_folds"], Value::from(false));

    let home = pool.env.home.as_deref().expect("a home");
    let secrets = home.join(".secrets");
    std::fs::create_dir_all(&secrets).expect(".secrets");
    std::fs::write(
        secrets.join(crate::harness::accounts_sync::cli_fleet_file(Cli::Codex)),
        "[[account]]\nlabel = \"twin\"\nemail = \"ann@example.com\"\nmachines = [\"m16\"]\n",
    )
    .expect("the fleet file");
    pool.config("[fleet]\nmachine = \"m16\"\n");
    let (text, json) = pool.report_on(&host);
    assert!(
        line(&text).contains("`aterm harness accounts sync --cli codex` folds `twin`"),
        "{text}"
    );
    assert_eq!(json["next"]["twin"]["sync_folds"], Value::from(true));

    std::fs::write(twin.join("auth.json"), "{}").expect("the twin's own login");
    let (text, json) = pool.report_on(&host);
    assert!(
        line(&text).contains("both stay"),
        "a login of its own: {text}"
    );
    assert_eq!(json["next"]["twin"]["sync_folds"], Value::from(false));
}

/// `accounts use` CITES THE REPORT'S WEEKLY-LIMIT LINE ONLY WHERE THAT LINE PRINTS: a lone row's pin
/// says a session stays, with no move to take. CONTROL: with a row to move to, its words and the
/// report's line stand together.
#[test]
fn accounts_use_cites_the_weekly_limit_line_only_where_the_report_prints_it() {
    let pool = Pool::new("use-lone");
    let host = CodexProgram(Some(pool.new.clone()));
    pool.pin("main");
    let (text, _) = pool.report_on(&host);
    if crate::harness::codex_rotation::QUOTA_ROTATION_QUALIFIED {
        assert!(
            text.contains("at a weekly limit: a session on `main`"),
            "CONTROL: {text}"
        );
        let said = pool.recorded("main", &host);
        assert!(said.contains("but for one on `main`"), "CONTROL: {said}");
    }
    pool.rows(&[accounts::Row {
        label: "main".into(),
        email: Some("owner@example.com".into()),
        ..accounts::Row::default()
    }]);
    pool.pin("main");
    let (text, _) = pool.report_on(&host);
    assert!(!text.contains("at a weekly limit:"), "{text}");
    let said = pool.recorded("main", &host);
    assert!(!said.contains("at a weekly limit"), "{said}");
    assert!(!said.contains("but for one on"), "{said}");
}
