// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `accounts --cli codex`'s `next:` says what a launch would take: on a Codex program that keeps
//! no row's login its own, only the row on this machine's own login; and under
//! `[harness] accounts = "off"`, for every CLI, what rotation off leaves a launch. The report,
//! `accounts login|renew` and `accounts use` judge the program a launch runs; `accounts use` of
//! every CLI but Claude Code says what a launch does under the pin, or once it ends.

use super::*;

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
        }
    }

    /// `[harness] accounts = "<mode>"` in the `aterm.toml` beside the rosters.
    fn mode(&self, mode: &str) {
        self.config(&format!("[harness]\naccounts = \"{mode}\"\n"));
    }

    /// `text` as the `aterm.toml` beside the rosters.
    fn config(&self, text: &str) {
        std::fs::write(self.env.config.as_deref().expect("aterm.toml"), text).expect("aterm.toml");
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
            let now = pool.env.now;
            pool.state(|state| {
                accounts::note_auth(state, "main", accounts::Auth::SignedOut, now);
            });
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
/// byte, the JSON's `next` too (and, under
/// `early`, the default, all of the JSON); no `rotation off:` line, and the JSON's `next` holds
/// the keys it held before rotation off had words (`to`, `wait_until` with its RFC 3339 twin,
/// `not`, and on a Codex that keeps no row's login its own `only_default` and `none`), no `off`.
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
            } else {
                &["not", "to", "wait_until", "wait_until_rfc3339"]
            };
            assert_eq!(keys, want_keys, "{at}: {doc}");
            assert_eq!(text, plain_text, "{at}");
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
        assert_eq!(pool.seat().mode(), accounts::Mode::Off, "{toml}");
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
        assert_eq!(pool.seat().mode(), accounts::Mode::Off, "{toml}");
        let why = crate::supervise::config::SupervisorConfig::accounts_off_by(toml)
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
/// row), `main`. The JSON's `to` is the line's, its `not` the text's.
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
                let launch = pool
                    .launch_on(program.as_deref())
                    .unwrap_or_else(|why| panic!("{at}: {why}"))
                    .unwrap_or_else(|| panic!("{at}: a launch takes a row"));
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

/// A ROW THAT CANNOT SERVE IS NOT NEXT FOR ITS OWN CONDITION, EVEN WHERE IT IS THE ROW IN USE:
/// for Codex (on a program that keeps `CODEX_AUTH_PATH`'s login) with no row active, `main`
/// signed out or walled is not next as such, never as "the account in use" under `active: none`;
/// with `main` the active row, signed out, it is not next as `signed-out`, and only while it can
/// serve is it "the account in use". With the owner's pin on `work` and no login of it here, the
/// pin refuses every launch, so no row is next, and `next:` says why in text and JSON (CONTROL:
/// with its login here, `next:` names a row). Claude Code, whose session on this machine's own login
/// rotates off it, says `main`'s own condition too, and "the account in use" while it serves.
/// Kimi Code CLI's one row on this machine's own login, with no login made here (as `accounts
/// adopt-default` leaves it), is not next as not signed in here, so no other row is offered to
/// list while that row can be signed in.
#[test]
fn a_row_that_cannot_serve_is_not_next_for_its_own_condition() {
    let signed_out = |pool: &Pool| {
        let now = pool.env.now;
        pool.state(|state| {
            accounts::note_auth(state, "main", accounts::Auth::SignedOut, now);
        });
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
        assert_eq!(
            not_next(&text)["main"],
            "signed-out",
            "{}: {text}",
            cli.id()
        );
        assert_eq!(
            not_next_json(&doc),
            not_next(&text),
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
    assert_eq!(
        not_next(&text)["main"],
        "the account in use",
        "CONTROL: {text}"
    );
    signed_out(&pool);
    let (text, _) = pool.report(Some(&pool.new));
    assert_eq!(not_next(&text)["main"], "signed-out", "{text}");

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
    let (text, _) = pool.report(None);
    assert!(
        not_next(&text)["main"].starts_with("not-enrolled"),
        "{text}"
    );
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
/// and `work` is not "the account in use" under `not next:`. CONTROL: with `work` active but no
/// pin, `next:` is the row a rotation would move to, `main`, past `work` in use; with the pin on
/// `work` leased by another Mac, a launch does not take it, and `next:` stays `main`.
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
            pool.state(|state| state.active = Some("work".to_string()));
            let (text, _) = pool.report(program.as_deref());
            assert_eq!(next_line(&text), "next: main", "{at}: CONTROL: {text}");
            assert!(
                not_next(&text).contains_key("work"),
                "{at}: CONTROL: {text}"
            );
            pool.pin("work");
            assert_eq!(
                pool.launch_on(program.as_deref()),
                Ok(Some("work".to_string())),
                "{at}: PRECONDITION: a launch takes the pin"
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
            let now = pool.env.now;
            pool.state(|state| state.note_lease("work", "other-mac", now + 3600, now));
            assert_ne!(
                pool.launch_on(program.as_deref()),
                Ok(Some("work".to_string())),
                "{at}: CONTROL: a launch leaves a pin leased elsewhere"
            );
            let (text, _) = pool.report(program.as_deref());
            assert_eq!(next_line(&text), "next: main", "{at}: CONTROL: {text}");
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
        let now = pool.env.now;
        if recorded {
            pool.state(|state| {
                accounts::note_auth(state, "main", accounts::Auth::SignedOut, now);
            });
        }
        if removed {
            let home = pool.env.home.as_deref().expect("a home");
            std::fs::remove_file(
                home.join(Cli::Codex.accounts().default_home)
                    .join("auth.json"),
            )
            .expect("the default login");
        }
        let roster = accounts::Roster::load_for(
            Cli::Codex,
            &pool.env.accounts_roster_for(Cli::Codex).expect("a roster"),
        )
        .expect("roster");
        let (state, _) = pool
            .env
            .accounts_store_for(Cli::Codex)
            .expect("a store")
            .load(now)
            .expect("state");
        let enrolled = accounts::enrolled_in(
            Cli::Codex,
            &roster,
            pool.env.home.as_deref().expect("a home"),
        );
        assert_eq!(
            crate::harness::accounts_pool::default_serving(&roster, &state, &enrolled, now)
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
    use crate::harness::codex_alab::UNPIN_FIX;
    for mode in ["early", "off"] {
        let pool = Pool::new("use-old-own");
        pool.mode(mode);
        let said = pool.recorded("work", &CodexProgram(Some(pool.old.clone())));
        let refused = pool
            .launch(&pool.old)
            .expect_err("PRECONDITION: a launch under the pin is refused");
        let refused = without_no_worker(&refused);
        assert!(said.ends_with(refused), "{mode}: {said}");
        for back in [UNPIN_FIX, "aterm pkg update codex"] {
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
/// each with none of Claude Code's words.
#[test]
fn for_grok_and_kimi_accounts_use_says_what_a_launch_does() {
    for cli in [Cli::Grok, Cli::Kimi] {
        for mode in ["early", "off"] {
            let at = format!("{} under {mode}", cli.id());
            let pool = Pool::of(cli, "use-grok-kimi");
            pool.mode(mode);
            let host = CodexProgram(None);
            let said = pool.recorded("work", &host);
            assert_eq!(
                pool.launch_on(None),
                Ok(Some("work".to_string())),
                "{at}: PRECONDITION: a launch takes the pin"
            );
            assert!(said.contains("`work`"), "{at}: {said}");
            assert!(said.contains(cli.connector().product), "{at}: {said}");
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
/// (`not_this_macs`); where it takes the pin all the same, the line is the one before; where it
/// is refused, the line carries that refusal, and where the pin's end ends the refusal it names
/// that way back (`accounts_pool::unpin_line`), else it says what `use --auto` says. Codex (on a
/// build that keeps `CODEX_AUTH_PATH`'s login), Grok Build and Kimi Code CLI, under rotation and
/// rotation off. CONTROL: with none of it, a launch takes the pin and the line names it.
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
                assert_eq!(
                    pool.launch_on(program.as_deref()),
                    Ok(Some("work".to_string())),
                    "{at}: CONTROL: PRECONDITION: a launch takes the pin"
                );
                assert!(control.contains("`work`"), "{at}: CONTROL: {control}");
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
                        assert!(said.contains(&not_this_macs(why)), "{at}: {said}");
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
