// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every command an account surface names can be taken. The law reads what each surface printed
//! (the report's text and JSON, `accounts use` and its refusals, a launch's refusal, the doctor's
//! fix, mend and fact, a review's refusal, `aterm ai login`'s refusal), parses each command there
//! into a [`Step`], which fails one no `Step` spells, and takes it in a fresh copy of that world
//! through the real parser and the real entry. No world runs a person's or a vendor's program: a
//! sign-in stops at a stand-in.

use super::*;
use crate::harness::accounts_step::Step;
use std::cell::Cell;
use std::collections::BTreeSet;

/// [`LawHost`], at a terminal, whose sign-in and browser window are stand-ins that count what
/// reached them.
struct StepHost {
    host: LawHost,
    reached: Cell<usize>,
}

impl AccountsHost for StepHost {
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
        0
    }
    fn codex_launch_program(&self) -> Option<PathBuf> {
        self.host.codex_launch_program()
    }
    fn launch_program(&self, cli: Cli) -> Option<PathBuf> {
        self.host.launch_program(cli)
    }
    fn launch_path(&self) -> Option<OsString> {
        self.host.launch_path()
    }
    fn sign_in_program(&self, cli: Cli) -> Option<PathBuf> {
        self.host.sign_in_program(cli)
    }
    fn vendor_login(&self, _: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        self.reached.set(self.reached.get() + 1);
        Err("this stand-in signs nothing in".to_string())
    }
    fn vendor_sign_in(
        &self,
        _: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.reached.set(self.reached.get() + 1);
        crate::harness::login_drive::Drove {
            outcome: crate::harness::login_drive::Outcome::NotRun(
                "this stand-in signs nothing in".to_string(),
            ),
            host: None,
        }
    }
    fn window_browser(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/nonexistent/stand-in-browser"))
    }
    fn open_window(
        &self,
        _: &Path,
        _: &Path,
        _: &str,
    ) -> Result<crate::harness::browser_window::WindowEnd, String> {
        self.reached.set(self.reached.get() + 1);
        Err("this stand-in opens no window".to_string())
    }
}

/// A command a surface names, and whether it names one before it to do first (`` `a`, then `b` ``):
/// that one is what the surface asks now, and this one is asked of the world it leaves.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Named {
    line: String,
    after_another: bool,
}

/// Every command `text` names: a backticked one that starts with `aterm ` or is a CLI's own
/// `login`, and the rest of a `run:` or `list one:` line.
fn named_commands(text: &str) -> Vec<Named> {
    let mut named = Vec::new();
    for line in text.lines() {
        let mut between = String::new();
        let mut earlier = false;
        for (at, part) in line.split('`').enumerate() {
            let vendor = part
                .strip_suffix(" login")
                .is_some_and(|id| Cli::parse(id).is_some());
            if at % 2 == 1 && (part.starts_with("aterm ") || vendor) {
                named.push(Named {
                    line: part.to_owned(),
                    after_another: earlier && between.contains("then"),
                });
                earlier = true;
                between.clear();
            } else if at % 2 == 0 {
                between.push_str(part);
            }
        }
        for prefix in ["run: aterm ", "list one: aterm "] {
            if let Some(at) = line.find(prefix) {
                named.push(Named {
                    line: line[at + prefix.find("aterm").expect("the word")..].to_owned(),
                    after_another: false,
                });
            }
        }
    }
    named
}

/// The two forms a surface names that no `Step` spells, since nothing a surface reads fills their
/// arguments ([`crate::harness::accounts_step`]).
fn outside_step(line: &str) -> bool {
    line.contains(" shared-renew ")
        || (line.starts_with("aterm harness accounts sync ") && line.contains("secrets"))
}

/// The [`Step`] `line` spells, which prints back as `line`.
fn step_of(line: &str) -> Result<Step, String> {
    let mut words: Vec<&str> = line.split_whitespace().collect();
    let cli = words.iter().position(|word| *word == "--cli").map(|at| {
        let id = words[at + 1];
        words.drain(at..at + 2);
        Cli::parse(id).ok_or_else(|| format!("{line}: no CLI `{id}`"))
    });
    let cli = cli.transpose()?.unwrap_or(Cli::Claude);
    let step = match words.as_slice() {
        ["aterm", "harness", "accounts"] => Step::Report(cli),
        ["aterm", "harness", "accounts", "sync"] => Step::Sync(cli),
        ["aterm", "harness", "accounts", "use", "--auto"] => Step::Auto(cli),
        ["aterm", "harness", "accounts", "use", label] => Step::Use(cli, (*label).to_owned()),
        ["aterm", "harness", "accounts", "login", label] => Step::Login(cli, (*label).to_owned()),
        ["aterm", "harness", "accounts", "renew", labels @ ..] if !labels.is_empty() => {
            Step::Renew(cli, labels.join(" "))
        }
        ["aterm", "harness", "accounts", "browser", label] => {
            Step::Browser(cli, (*label).to_owned())
        }
        ["aterm", "harness", "accounts", "add", "<label>"] => Step::Add { cli, email: None },
        [
            "aterm",
            "harness",
            "accounts",
            "add",
            "<label>",
            "--email",
            email,
        ] => Step::Add {
            cli,
            email: Some((*email).to_owned()),
        },
        [
            "aterm",
            "harness",
            "accounts",
            "adopt-default",
            label,
            "--email",
            "<e>",
        ] => Step::AdoptDefault(cli, (*label).to_owned()),
        ["aterm", "pkg", "install", id] => Step::Install((*id).to_owned()),
        ["aterm", "pkg", "update", id] => Step::Update((*id).to_owned()),
        ["aterm", "ai", "login", id] => Step::AiLogin((*id).to_owned()),
        ["aterm", "ai", "doctor"] => Step::Doctor,
        [id, "login"] => Step::VendorLogin(Cli::parse(id).ok_or("no such CLI")?),
        _ => return Err(format!("no Step spells `{line}`")),
    };
    if step.to_string() == line {
        Ok(step)
    } else {
        Err(format!("`{line}` is spelled `{step}` by a Step"))
    }
}

/// What the records, the logins and the roster hold: what a step changes where it makes progress.
fn fingerprint(pool: &Pool) -> Vec<Option<Vec<u8>>> {
    let store = pool.env.accounts_store_for(pool.cli).expect("a store");
    let home = pool.env.home.as_deref().expect("a home");
    let dir = home.join(pool.cli.accounts().default_home);
    let file = pool
        .cli
        .connector()
        .stores
        .first()
        .copied()
        .unwrap_or("login");
    let work = pool
        .tmp
        .path()
        .join("logins")
        .join(pool.cli.id())
        .join("work");
    [
        pool.env.accounts_roster_for(pool.cli).expect("a roster"),
        store.state_file(),
        store.logins_file(),
        dir.join(file),
        work.join(file),
    ]
    .iter()
    .map(|path| std::fs::read(path).ok())
    .collect()
}

/// One surface's words, and the command that printed them where the step it names is taken from
/// what that command left.
struct Said {
    surface: String,
    words: String,
    after: Option<String>,
}

/// What each surface of the laid world prints. `lay` lays the world again after a surface that
/// writes. A Grok Build or Kimi Code CLI build here is a stand-in program with nothing laid in
/// atpkg's store, where the doctor's facts read theirs, so only Codex's doctor is read.
fn surfaces(pool: &Pool, host: &StepHost, lay: &dyn Fn()) -> Vec<Said> {
    let (cli, id) = (pool.cli, pool.cli.id());
    let mut said: Vec<(String, String)> = Vec::new();
    lay();
    let (text, json) = pool.report_on(&host.host);
    said.push(("the report".to_owned(), text));
    said.push(("the report's JSON".to_owned(), json.to_string()));
    let launch = Launch::of(Some(&host.host.program), &host.host);
    let unread = |_: Cli, _: &Path, _: Option<&str>, _: i64| None;
    let records = pool
        .env
        .accounts_store_for(cli)
        .expect("a store")
        .read_account_records(pool.env.accounts_roster_for(cli).as_deref(), pool.env.now)
        .expect("the records");
    if cli == Cli::Codex {
        let mut fact = fact_on(pool, &host.host);
        if let Some(row) = fact.login_pool() {
            if let Some(why) = &row.launch_refused {
                said.push(("the doctor's refusal".to_owned(), why.clone()));
            }
            if let Err(why) = crate::ai::login::pool_target(cli, row) {
                said.push(("`ai login`'s refusal".to_owned(), why));
            }
        }
        let row = crate::ai::state::assess(&fact, true, pool.env.now);
        said.push(("the doctor's fact".to_owned(), row.fact.clone()));
        for (name, words) in [("fix", &row.fix), ("mend", &row.mend)] {
            if let Some(words) = words {
                // A fix is one command, whatever it is quoted as; a mend is a sentence.
                let quoted = if name == "fix" {
                    format!("`{words}`")
                } else {
                    words.clone()
                };
                said.push((format!("the doctor's {name}"), quoted));
            }
        }
        if let Some(why) = crate::ai::login::refusal(&fact, &row) {
            said.push(("`ai login`'s refusal".to_owned(), why));
        }
        if let Err(why) =
            crate::ai::review::review_row(&mut fact, pool.env.now, pool.env.config.as_deref())
        {
            said.push(("a review's refusal".to_owned(), why));
        }
    }
    if let Err(why) =
        crate::harness::accounts_pool::launch_env_for(&pool.seat(), cli, &unread, launch.program)
    {
        said.push(("a launch's refusal".to_owned(), why));
    }
    let mut said: Vec<Said> = said
        .into_iter()
        .map(|(surface, words)| Said {
            surface,
            words,
            after: None,
        })
        .collect();
    for row in &records.roster.rows {
        lay();
        let line = format!("accounts use {} --cli {id}", row.label);
        let (code, out, err) = pool.run_on(&line, &host.host);
        said.push(Said {
            surface: format!("`{line}`"),
            words: format!("{out}\n{err}"),
            after: (code == ExitCode::SUCCESS).then_some(line),
        });
    }
    said
}

/// A step's command line for the real parser: a label and an address a person chooses filled in.
/// What the facts say of `pool` under the launcher `host`'s build names.
fn fact_on(pool: &Pool, host: &LawHost) -> crate::ai::facts::Facts {
    pool.lay_launcher(host);
    pool.law_fact()
}

fn command_line(step: &Step) -> String {
    step.to_string()
        .replace("<label>", "fresh")
        .replace("<e>", "fresh@example.com")
        .replacen("aterm harness ", "", 1)
}

/// The real parser's and the real entry's answer to `step` in the world `lay` lays: `Ok` where it
/// made progress, reached a stand-in a person's act stops at, or is a look that answers.
fn take(pool: &Pool, build: LawBuild, host: &StepHost, step: &Step) -> Result<(), String> {
    match step {
        Step::Install(id) => {
            if !build.missing() {
                return Err(format!("{id} is installed here"));
            }
            if id != pool.cli.id() {
                return Err(format!("{id} is not the CLI of this pool"));
            }
            return Ok(());
        }
        Step::Update(id) => {
            if build.missing() {
                return Err(format!("{id} is not installed here"));
            }
            return Ok(());
        }
        Step::Doctor => return Ok(()),
        Step::VendorLogin(cli) => {
            if *cli != pool.cli || build.missing() {
                return Err("no such program here".to_owned());
            }
            let home = pool.env.home.as_deref().expect("a home");
            if crate::harness::codex_alab::auth_path_unread(&host.host.program).is_none()
                && !crate::harness::accounts_pool::keyring_held_in(pool.cli, home)
            {
                return Err("a build that keeps a row's login signs the row in".to_owned());
            }
            return Ok(());
        }
        // It installs what is not here first ([`crate::ai::cli`]).
        Step::AiLogin(id) if build.missing() && id == pool.cli.id() => return Ok(()),
        _ => {}
    }
    let before = fingerprint(pool);
    let reached = host.reached.get();
    let line = if let Step::AiLogin(id) = step {
        if id != pool.cli.id() {
            return Err(format!("{id} is not the CLI of this pool"));
        }
        let fact = fact_on(pool, &host.host);
        let row = fact.login_pool().ok_or("`aterm ai login` sees no pool")?;
        if crate::ai::login::default_home_sign_in(row) {
            // Its own sign-in in the default home, which is the vendor's program.
            return take(pool, build, host, &Step::VendorLogin(pool.cli));
        }
        let (label, verb) = crate::ai::login::pool_target(pool.cli, row)?;
        format!("accounts {verb} {label} --cli {id}")
    } else {
        command_line(step)
    };
    let (code, out, err) = run_line(pool, host, &line)?;
    if matches!(step, Step::Report(_)) {
        return (code == ExitCode::SUCCESS)
            .then_some(())
            .ok_or_else(|| format!("the report refused: {err}"));
    }
    if host.reached.get() > reached || fingerprint(pool) != before {
        return Ok(());
    }
    Err(format!("`{line}` did nothing ({code:?}): {out}{err}"))
}

/// `aterm ai login` TAKES THE STEP A MOVED DEFAULT LOGIN'S REFUSAL NAMES: with a pin on `main` and a
/// row that holds that account the step is the sync, so no sign-in starts and its refusal names that
/// sync. CONTROL: with a pin on `work`, which has an address and no login, it signs `work` in.
#[test]
fn ai_login_takes_the_step_a_moved_default_logins_refusal_names() {
    let pool = Pool::of(Cli::Codex, "ai-login-moved");
    let host = LawBuild::CodexNew.program(&pool);
    pool.lay_moved(MovedWorld::PinOnMainToRow);
    let fact = fact_on(&pool, &host);
    let row = fact.login_pool().expect("a pool");
    let why = crate::ai::login::pool_target(Cli::Codex, row).expect_err("no sign-in ends it");
    let sync = Step::Sync(Cli::Codex).to_string();
    assert!(
        named_commands(&why).iter().any(|named| named.line == sync),
        "{why}"
    );

    // A row a sign-in would make is no way out of that refusal.
    let mut rows = pool.roster().rows;
    rows.push(accounts::Row {
        label: "third".into(),
        email: Some("third@example.com".into()),
        dir: Some(pool.tmp.path().join("third").display().to_string()),
        ..accounts::Row::default()
    });
    pool.rows(&rows);
    let fact = fact_on(&pool, &host);
    let row = fact.login_pool().expect("a pool");
    assert_eq!(row.next.as_deref(), Some("third"), "CONTROL");
    assert!(
        row.default_moved.is_some(),
        "CONTROL: {:?}",
        row.launch_refused
    );
    let why = crate::ai::login::pool_target(Cli::Codex, row).expect_err("no sign-in ends it");
    assert!(
        named_commands(&why).iter().any(|named| named.line == sync),
        "{why}"
    );

    // The move refused, so that sign-in is the step, and it is the row's own.
    pool.lay_moved(MovedWorld::DirNamesAnother);
    let store = pool.env.accounts_store_for(Cli::Codex).expect("a store");
    store
        .transact(pool.env.now, &mut None, |state| {
            state.active = Some("main".to_owned());
            state.proven = true;
            state.pinned = true;
            Ok(Vec::new())
        })
        .expect("the pin");
    let fact = fact_on(&pool, &host);
    let row = fact.login_pool().expect("a pool");
    assert_eq!(
        row.default_moved,
        Some((
            "main".to_owned(),
            Step::Login(Cli::Codex, "main".to_owned())
        )),
        "CONTROL"
    );
    let target = crate::ai::login::pool_target(Cli::Codex, row);
    assert_eq!(target, Ok(("main", Cli::Codex.sign_in_verb(false))));

    pool.lay_moved(MovedWorld::PinOnWorkNoLogin);
    let mut rows = pool.roster().rows;
    let work = rows.iter_mut().find(|row| row.label == "work");
    work.expect("a row").email = Some("work@example.com".into());
    pool.rows(&rows);
    let fact = fact_on(&pool, &host);
    let row = fact.login_pool().expect("a pool");
    assert_eq!(row.default_moved, None, "CONTROL");
    let target = crate::ai::login::pool_target(Cli::Codex, row).expect("CONTROL");
    assert_eq!(target.0, "work", "CONTROL");
}

/// `line`, through the real parser and the real entry: its exit, and what it printed.
fn run_line(
    pool: &Pool,
    host: &StepHost,
    line: &str,
) -> Result<(ExitCode, String, String), String> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = match parsed(line).map_err(|why| format!("the parser refused: {why}"))? {
        Cmd::Accounts(form) => run_accounts(&form, &pool.env, host, &mut out, &mut err),
        Cmd::AccountsOf(of) => run_accounts_of(&of, &pool.env, host, &mut out, &mut err),
        _ => return Err("the parser read another command".to_owned()),
    };
    Ok((
        code,
        String::from_utf8(out).expect("utf-8"),
        String::from_utf8(err).expect("utf-8"),
    ))
}

/// A world past what [`LawWorld`] says: the default login names `moved@example.com`, another account
/// than `main`'s, and the rest of the roster and `main`'s managed directory are as `self` says.
#[derive(Clone, Copy, Debug)]
enum MovedWorld {
    /// `work` is that account and nothing keeps the move from happening.
    ToRow,
    /// So is a row held for another Mac.
    ToHeldRow,
    /// `main`'s managed directory holds another account's login.
    DirNamesAnother,
    /// `main`'s managed directory holds a login of `main`'s, which no record says it left.
    DirOwnNeverLeft,
    /// `main`'s managed directory holds a login that names nothing.
    DirUnreadable,
    /// `main`'s managed directory keeps its logins in the keyring.
    DirKeyring,
    /// `main`'s managed directory holds something else.
    DirOtherFiles,
    /// A pin on `work`, which has no login: what ends that is `work`'s sign-in.
    PinOnWorkNoLogin,
    /// A pin on `main` and the move held.
    PinOnMainHeld,
    /// A pin on `main`, and `work` is that account: the sync moves the login.
    PinOnMainToRow,
    /// A pin on `work`, whose own directory holds another account's login.
    PinOnWorkNamesAnother,
    /// `main` names nobody, `work` is that account and holds nothing, and a source lists it.
    TwinFolds,
    /// So, but `work` holds a login of its own.
    TwinHolds,
    /// So, but no source lists it.
    TwinNoSource,
}

const MOVED_WORLDS: [MovedWorld; 14] = [
    MovedWorld::ToRow,
    MovedWorld::ToHeldRow,
    MovedWorld::DirNamesAnother,
    MovedWorld::DirOwnNeverLeft,
    MovedWorld::DirUnreadable,
    MovedWorld::DirKeyring,
    MovedWorld::DirOtherFiles,
    MovedWorld::PinOnWorkNoLogin,
    MovedWorld::PinOnMainHeld,
    MovedWorld::PinOnMainToRow,
    MovedWorld::PinOnWorkNamesAnother,
    MovedWorld::TwinFolds,
    MovedWorld::TwinHolds,
    MovedWorld::TwinNoSource,
];

impl Pool {
    /// The default login of this pool names `moved@example.com`, `main` on it is `owner@…`, and
    /// the rest is as `world` says.
    fn lay_moved(&self, world: MovedWorld) {
        use MovedWorld as W;
        let twin = matches!(world, W::TwinFolds | W::TwinHolds | W::TwinNoSource);
        let pair = LawWorld {
            main: LawLogin::Moved,
            work: (!twin).then_some(if matches!(world, W::PinOnWorkNoLogin) {
                LawLogin::NotEnrolled
            } else {
                LawLogin::Usable
            }),
            hold: match world {
                W::PinOnWorkNoLogin | W::PinOnWorkNamesAnother => LawHold::Pin("work"),
                W::PinOnMainHeld | W::PinOnMainToRow => LawHold::Pin("main"),
                _ => LawHold::None,
            },
            claim: LawClaim::None,
            rotation: LawRotation::On,
        };
        self.lay(&pair);
        let store = self.env.accounts_store_for(self.cli).expect("a store");
        let managed = store.login_dir("main").expect("a managed directory");
        let _ = std::fs::remove_dir_all(&managed);
        std::fs::create_dir_all(&managed).expect("the managed directory");
        match world {
            W::DirNamesAnother => {
                law_auth(&managed.join("auth.json"), "z", Some("z@example.com"));
            }
            W::DirOwnNeverLeft => {
                law_auth(&managed.join("auth.json"), "o", Some("owner@example.com"));
            }
            W::DirUnreadable => std::fs::write(managed.join("auth.json"), "{}").expect("a file"),
            W::DirKeyring => std::fs::write(
                managed.join("config.toml"),
                "cli_auth_credentials_store = \"keyring\"\n",
            )
            .expect("a file"),
            W::DirOtherFiles => std::fs::write(managed.join("notes"), "x").expect("a file"),
            _ => {}
        }
        let secrets = self.env.home.as_deref().expect("a home").join(".secrets");
        let fleet = secrets.join(crate::harness::accounts_sync::cli_fleet_file(self.cli));
        let _ = std::fs::remove_file(&fleet);
        if matches!(world, W::PinOnWorkNoLogin | W::PinOnMainHeld) {
            return;
        }
        if matches!(world, W::PinOnWorkNamesAnother) {
            let mut rows = self.roster().rows;
            let work = rows.iter_mut().find(|row| row.label == "work");
            let work = work.expect("a row");
            work.email = Some("work@example.com".into());
            let own = work.dir.clone().expect("its directory");
            law_auth(
                &Path::new(&own).join("auth.json"),
                "z",
                Some("z@example.com"),
            );
            self.rows(&rows);
            return;
        }
        if twin {
            let own = self
                .tmp
                .path()
                .join("logins")
                .join(self.cli.id())
                .join("work");
            let _ = std::fs::remove_dir_all(&own);
            std::fs::create_dir_all(&own).expect("the twin's directory");
            if matches!(world, W::TwinHolds) {
                law_auth(&own.join("auth.json"), "m", Some(LAW_MOVED_TO));
            }
            let mut rows = self.roster().rows;
            for row in &mut rows {
                row.email = None;
            }
            rows.push(accounts::Row {
                label: "work".into(),
                dir: Some(own.display().to_string()),
                email: Some(LAW_MOVED_TO.into()),
                ..accounts::Row::default()
            });
            self.rows(&rows);
            if !matches!(world, W::TwinNoSource) {
                std::fs::create_dir_all(&secrets).expect(".secrets");
                std::fs::write(
                    &fleet,
                    format!(
                        "[[account]]\nlabel = \"work\"\nemail = \"{LAW_MOVED_TO}\"\n\
                         machines = [\"m16\"]\n"
                    ),
                )
                .expect("the fleet file");
                self.config("[fleet]\nmachine = \"m16\"\n");
            }
            return;
        }
        let mut rows = self.roster().rows;
        let work = rows
            .iter_mut()
            .find(|row| row.label == "work")
            .expect("a row");
        work.email = Some(LAW_MOVED_TO.into());
        work.held_by = matches!(world, W::ToHeldRow).then(|| "other-mac".to_owned());
        let own = work.dir.clone().expect("its directory");
        law_auth(&Path::new(&own).join("auth.json"), "m", Some(LAW_MOVED_TO));
        self.rows(&rows);
    }
}

/// One world's law: `lay` lays it; every command its surfaces name can be taken.
fn law_of_world(
    pool: &Pool,
    build: LawBuild,
    name: &str,
    lay: &dyn Fn(),
    (seen, wrong): (&mut BTreeSet<String>, &mut Vec<String>),
) {
    let host = StepHost {
        host: build.program(pool),
        reached: Cell::new(0),
    };
    let said = surfaces(pool, &host, lay);
    let mut taken = BTreeSet::new();
    for Said {
        surface,
        words,
        after,
    } in &said
    {
        for Named {
            line,
            after_another,
        } in named_commands(words)
        {
            if outside_step(&line) {
                continue;
            }
            let step = match step_of(&line) {
                Ok(step) => step,
                Err(why) => {
                    wrong.push(format!("{name}: {surface}: {why}"));
                    continue;
                }
            };
            if after_another || !taken.insert((line.clone(), after.clone())) {
                continue;
            }
            seen.insert(
                line.split_whitespace()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            lay();
            if let Some(after) = after {
                let (code, out, err) = pool.run_on(after, &host.host);
                assert_eq!(code, ExitCode::SUCCESS, "`{after}`: {out}{err}");
            }
            if let Err(why) = take(pool, build, &host, &step) {
                wrong.push(format!("{name}: {surface} names `{line}`: {why}"));
            }
        }
    }
}

/// EVERY STEP A SURFACE NAMES CAN BE TAKEN: across the launch law's worlds and those of a moved
/// default login, each command any surface prints parses into a [`Step`], parses through the real
/// command parser, and in a fresh copy of that world runs through the real entry to progress: the
/// records, the roster or a login change, or a person's sign-in reaches its stand-in. A step that
/// exits with the refusal it was named from, or changes nothing, fails it. A pool's install and
/// update, and a vendor's own login, are judged by what is on this Mac, since aterm cannot run
/// them here. FLOOR: the steps it took cannot silently shrink.
///
/// THE GAP, from [`law_worlds`]'s cuts. A build that is not there takes only that function's few
/// worlds, so what a surface asks there of any other world is not taken: Codex's `use --auto`,
/// which a refusal asks first only with both rows walled (the launch law holds its exit on such a
/// build, from the pin `use` leaves), and `accounts --cli <id>`, from `ai login`'s refusal and the
/// doctor's mend on Codex and from a launch's refusal with both rows walled on Grok and Kimi. Grok
/// and Kimi cross a claim only where both rows can serve, so `accounts --cli <id>`, which
/// `use <row>` names with both rows walled and one claimed, is not taken on either. A defect in one
/// of those steps that shows only in such a world passes here.
#[test]
fn every_step_a_surface_names_can_be_taken() {
    let builds = [
        LawBuild::CodexNew,
        LawBuild::CodexOld,
        LawBuild::CodexMissing,
        LawBuild::Grok,
        LawBuild::GrokMissing,
        LawBuild::Kimi,
        LawBuild::KimiMissing,
    ];
    let runs: Vec<(BTreeSet<String>, Vec<String>)> = std::thread::scope(|scope| {
        builds
            .into_iter()
            .filter(|build| links_here(build.cli()))
            .flat_map(|build| {
                let worlds: Vec<LawWorld> = law_worlds(build)
                    .into_iter()
                    .filter(|world| {
                        matches!(
                            world.rotation,
                            LawRotation::On | LawRotation::AccountsOff | LawRotation::Keyring
                        )
                    })
                    .collect();
                let chunks: Vec<Vec<LawWorld>> = worlds
                    .chunks(worlds.len().div_ceil(3))
                    .map(<[LawWorld]>::to_vec)
                    .collect();
                chunks
                    .into_iter()
                    .map(|share| {
                        scope.spawn(move || {
                            let pool = Pool::of(build.cli(), "step-law");
                            let (mut seen, mut wrong) = (BTreeSet::new(), Vec::new());
                            for world in &share {
                                law_of_world(
                                    &pool,
                                    build,
                                    &format!("{build:?} {world:?}"),
                                    &|| pool.lay(world),
                                    (&mut seen, &mut wrong),
                                );
                            }
                            (seen, wrong)
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|run| run.join().expect("a share"))
            .collect()
    });
    let (mut seen, mut wrong) = (BTreeSet::new(), Vec::new());
    for (share_seen, share_wrong) in runs {
        seen.extend(share_seen);
        wrong.extend(share_wrong);
    }
    for build in [LawBuild::CodexNew, LawBuild::CodexOld] {
        for world in MOVED_WORLDS {
            let pool = Pool::of(build.cli(), "step-law-moved");
            law_of_world(
                &pool,
                build,
                &format!("{build:?} {world:?}"),
                &|| pool.lay_moved(world),
                (&mut seen, &mut wrong),
            );
            // A launch refused for what its default login names says what ends it.
            let host = StepHost {
                host: build.program(&pool),
                reached: Cell::new(0),
            };
            for said in surfaces(&pool, &host, &|| pool.lay_moved(world)) {
                let about_the_login = !matches!(world, MovedWorld::PinOnWorkNoLogin);
                if about_the_login
                    && said.surface == "a launch's refusal"
                    && named_commands(&said.words).is_empty()
                {
                    wrong.push(format!(
                        "{build:?} {world:?}: a launch's refusal names no step: {}",
                        said.words
                    ));
                }
            }
        }
    }
    // One example of each class: the words past the world, with each scratch path's words gone.
    let mut classes = BTreeMap::<String, &String>::new();
    for why in &wrong {
        let class = why
            .split_once(" }: ")
            .map_or(why.as_str(), |(_, rest)| rest);
        let class = class
            .split_whitespace()
            .filter(|word| !word.contains("/var/") && !word.contains("/tmp"))
            .collect::<Vec<_>>()
            .join(" ");
        classes.entry(class).or_insert(why);
    }
    assert!(
        wrong.is_empty(),
        "{} wrong, {} classes:\n{}",
        wrong.len(),
        classes.len(),
        classes
            .values()
            .map(|why| why.as_str())
            .take(60)
            .collect::<Vec<_>>()
            .join("\n")
    );
    for floor in [
        "aterm harness accounts --cli",
        "aterm harness accounts sync",
        "aterm harness accounts login",
        "aterm harness accounts use",
        "aterm pkg install codex",
        "aterm ai login codex",
        "codex login",
    ] {
        assert!(seen.contains(floor), "no surface named `{floor}`: {seen:?}");
    }
}
