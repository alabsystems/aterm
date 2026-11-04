// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Law B: a bounded exhaustive walk of a Codex pool through what happens to it, by the product's own
//! sync, report, launch, sign-in and writer entries. No world runs a person's or a vendor's program,
//! and every address is synthetic.

use super::*;
use crate::harness::accounts_sync::run_cli_accounts_sync_with;
use crate::harness::alab_roster::{self, Lab, LabAccount};
use std::collections::BTreeSet;

const OWNER: &str = "owner@example.com";
const WORK: &str = "work@example.com";
const THIRD: &str = "third@example.com";
const FAR: &str = "far@example.com";

/// The roster a walk starts from: `main` on the default login, and `work` on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Start {
    Two,
    /// With `third`, a login of its own too.
    Three,
    /// `work` is held for another Mac.
    Held,
    /// `work` is in shared custody, with a renewal of unknown outcome.
    Custody,
    /// `work` is `main`'s account, with a directory that holds nothing.
    Twin,
    /// `main`'s managed directory holds another account's login, so a sync refuses to move it.
    Occupied,
}

const STARTS: [Start; 6] = [
    Start::Two,
    Start::Three,
    Start::Held,
    Start::Custody,
    Start::Twin,
    Start::Occupied,
];

/// The starts every short walk crosses: the others take the long walks and [`ONCE`]'s.
const CROSSED_STARTS: [Start; 3] = [Start::Two, Start::Held, Start::Twin];

/// One thing that happens to the pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// A sign-in outside aterm: the default login names this address.
    Names(&'static str),
    /// The default login names no address.
    NamesNobody,
    /// The default login is gone.
    Absent,
    /// The default home keeps its login in the OS keyring.
    Keyring,
    /// `accounts sync` with nothing to read.
    Sync,
    /// `accounts sync` from a fleet sheet on this Mac.
    SyncSheet,
    /// `accounts sync` from the lab.
    SyncLab,
    /// `accounts login` of a row, through a stand-in that makes that row's login.
    SignIn(&'static str),
    /// `accounts use` of a row.
    Pin(&'static str),
    /// A session on this row reached its weekly limit.
    Wall(&'static str),
    /// A launch.
    Launch,
}

/// The events every short walk crosses.
const CROSSED: [Event; 7] = [
    Event::Names(WORK),
    Event::Names(FAR),
    Event::Absent,
    Event::Sync,
    Event::SyncLab,
    Event::SignIn("main"),
    Event::Launch,
];

/// The events a walk takes once, before each one of [`CROSSED`] and a sync.
const ONCE: [Event; 5] = [
    Event::NamesNobody,
    Event::SyncSheet,
    Event::Pin("work"),
    Event::Wall("work"),
    Event::SignIn("work"),
];

/// A host at a terminal whose sign-in writes the row's login in its staging directory.
struct WalkHost {
    host: LawHost,
}

impl WalkHost {
    fn make(req: &crate::harness::login_drive::Request<'_>) -> Option<String> {
        let email = req.row.expected_identity_email()?.to_owned();
        law_auth(&req.staging.join("auth.json"), &req.row.label, Some(&email));
        Some(email)
    }
}

impl AccountsHost for WalkHost {
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
    fn codex_account(&self, home: &Path) -> Option<String> {
        crate::harness::codex_app_login::read_account_email(home)
    }
    fn vendor_login(&self, req: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        Self::make(req);
        Ok(0)
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        crate::harness::login_drive::Drove {
            outcome: crate::harness::login_drive::Outcome::SignedIn {
                email: Self::make(req),
                page: false,
            },
            host: None,
        }
    }
}

/// One walk's pool.
struct Walk {
    pool: Pool,
    host: WalkHost,
    /// Each directory a sign-in moved its row out of.
    superseded: std::cell::RefCell<BTreeSet<PathBuf>>,
}

fn lab_account(label: &str, email: &str) -> LabAccount {
    LabAccount {
        pk: format!("ac-{email}"),
        cli: Cli::Codex,
        vendor: accounts::Vendor::default(),
        label: label.to_owned(),
        legacy_label: None,
        email: email.to_owned(),
        identity: None,
        browser: None,
        joinable: true,
        preferred: [("m16".to_owned(), Some(1))].into_iter().collect(),
    }
}

impl Walk {
    fn new(start: Start) -> Walk {
        let pool = Pool::new("walk");
        let host = WalkHost {
            host: LawBuild::CodexNew.program(&pool),
        };
        let walk = Walk {
            pool,
            host,
            superseded: std::cell::RefCell::default(),
        };
        walk.lay(start);
        walk
    }

    fn home(&self) -> PathBuf {
        self.pool.env.home.clone().expect("a home")
    }

    fn default_file(&self) -> PathBuf {
        self.home().join(".codex/auth.json")
    }

    fn store(&self) -> accounts::Store {
        self.pool
            .env
            .accounts_store_for(Cli::Codex)
            .expect("a store")
    }

    fn lay(&self, start: Start) {
        let own = |label: &str, email: Option<&str>| {
            let dir = self.pool.tmp.path().join("logins/codex").join(label);
            std::fs::create_dir_all(&dir).expect("a directory");
            let file = dir.join("auth.json");
            match email {
                Some(email) => law_auth(&file, label, Some(email)),
                None => {
                    let _ = std::fs::remove_file(&file);
                }
            }
            dir.display().to_string()
        };
        let mut rows = vec![accounts::Row {
            label: "main".into(),
            email: (start != Start::Twin).then(|| OWNER.into()),
            ..accounts::Row::default()
        }];
        let (work_email, work_login) = match start {
            Start::Twin => (OWNER, None),
            _ => (WORK, Some(WORK)),
        };
        rows.push(accounts::Row {
            label: "work".into(),
            email: Some(work_email.into()),
            dir: Some(own("work", work_login)),
            held_by: (start == Start::Held).then(|| "other-mac".to_owned()),
            ..accounts::Row::default()
        });
        if start == Start::Three {
            rows.push(accounts::Row {
                label: "third".into(),
                email: Some(THIRD.into()),
                dir: Some(own("third", Some(THIRD))),
                ..accounts::Row::default()
            });
        }
        self.pool.rows(&rows);
        self.pool.config("[fleet]\nmachine = \"m16\"\n");
        law_auth(&self.default_file(), "main", Some(OWNER));
        let store = self.store();
        if start == Start::Occupied {
            // That login is no row's: nothing here is to reach it, and it stays put.
            let managed = store.login_dir("main").expect("a managed directory");
            std::fs::create_dir_all(&managed).expect("the managed directory");
            law_auth(&managed.join("auth.json"), "z", Some("z@example.com"));
            self.superseded.borrow_mut().insert(managed);
        }
        let now = self.pool.env.now;
        store
            .transact_logins(now, &mut None, |_, logins| {
                if start == Start::Custody {
                    let staging = store
                        .claim_staging_dir("work")
                        .map_err(|why| why.to_string())?;
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
                            identity: WORK.into(),
                            staging: staging.to_str().expect("utf-8").into(),
                            before_generation: None,
                            begun_at: now - 10,
                        },
                        None,
                    )?;
                    logins.shared.finish(
                        token,
                        accounts::shared::Outcome::Unknown,
                        None,
                        now - 10,
                    )?;
                }
                Ok(Vec::new())
            })
            .expect("the records");
    }

    /// The records as a launch reads them.
    fn records(&self) -> accounts::AccountRecords {
        let path = self
            .pool
            .env
            .accounts_roster_for(Cli::Codex)
            .expect("a roster");
        self.store()
            .read_account_records(Some(&path), self.pool.env.now)
            .expect("the records")
    }

    /// The raw roster and what it holds.
    fn fingerprint(&self) -> Vec<Option<Vec<u8>>> {
        let store = self.store();
        [
            self.pool
                .env
                .accounts_roster_for(Cli::Codex)
                .expect("a roster"),
            store.state_file(),
            store.logins_file(),
        ]
        .iter()
        .map(|path| std::fs::read(path).ok())
        .collect()
    }

    /// Each login on this Mac: where it is and what it says.
    fn logins(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut found = BTreeMap::new();
        let mut look = |dir: &Path| {
            let file = dir.join("auth.json");
            if let Ok(bytes) = std::fs::read(&file) {
                found.insert(dir.to_path_buf(), bytes);
            }
        };
        look(&self.home().join(".codex"));
        for parent in [
            self.pool.tmp.path().join("logins/codex"),
            self.store().logins_dir().to_path_buf(),
        ] {
            for entry in std::fs::read_dir(&parent).into_iter().flatten().flatten() {
                look(&entry.path());
            }
        }
        found
    }

    fn sync_plain(&self) -> (ExitCode, String, String) {
        self.pool.run_on("accounts sync --cli codex", &self.host)
    }

    fn sheet(&self) -> PathBuf {
        let secrets = self.home().join(".secrets");
        std::fs::create_dir_all(&secrets).expect(".secrets");
        secrets.join(crate::harness::accounts_sync::cli_fleet_file(Cli::Codex))
    }

    fn sync_sheet(&self) -> (ExitCode, String, String) {
        let rows: String = self
            .records()
            .roster
            .rows
            .iter()
            .filter_map(|row| Some((row.label.clone(), row.expected_identity_email()?.to_owned())))
            .map(|(label, email)| {
                format!(
                    "[[account]]\nlabel = \"{label}\"\nemail = \"{email}\"\nmachines = [\"m16\"]\n"
                )
            })
            .collect();
        std::fs::write(self.sheet(), rows).expect("the sheet");
        let ran = self.sync_plain();
        std::fs::remove_file(self.sheet()).expect("the sheet");
        ran
    }

    fn sync_lab(&self) -> (ExitCode, String, String) {
        let accounts = self
            .records()
            .roster
            .rows
            .iter()
            .filter_map(|row| Some(lab_account(&row.label, row.expected_identity_email()?)))
            .collect();
        let lab = Lab {
            me: Some("m16".into()),
            settled: true,
            accounts,
            aliases: BTreeMap::new(),
            mailboxes: BTreeMap::new(),
            places: Default::default(),
        };
        let fetch =
            |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let program = self.host.host.program.clone();
        let code = alab_roster::with_lab(lab, || {
            run_cli_accounts_sync_with(
                Cli::Codex,
                (&self.pool.env, Some(&program)),
                (Some(alab_roster::SOURCE), None, false),
                (&mut out, &mut err),
                &fetch,
            )
        });
        (
            code,
            String::from_utf8(out).expect("utf-8"),
            String::from_utf8(err).expect("utf-8"),
        )
    }

    /// `event` happens; what a sync said, where it is one. A launch that acts on a login that
    /// names another account is `wrong`.
    fn happen(&self, event: Event, wrong: &mut Vec<String>) -> Option<(ExitCode, String, String)> {
        let keeps = self.home().join(".codex/config.toml");
        match event {
            Event::Names(address) => {
                let _ = std::fs::remove_file(&keeps);
                law_auth(&self.default_file(), address, Some(address));
                None
            }
            Event::NamesNobody => {
                let _ = std::fs::remove_file(&keeps);
                std::fs::write(self.default_file(), "{}").expect("a login");
                None
            }
            Event::Absent => {
                let _ = std::fs::remove_file(self.default_file());
                None
            }
            Event::Keyring => {
                std::fs::write(&keeps, "cli_auth_credentials_store = \"keyring\"\n")
                    .expect("a config");
                None
            }
            Event::Sync => Some(self.sync_plain()),
            Event::SyncSheet => Some(self.sync_sheet()),
            Event::SyncLab => Some(self.sync_lab()),
            Event::SignIn(label) => {
                let line = format!("accounts login {label} --cli codex");
                if self.records().roster.row(label).is_some() {
                    let _ = self.pool.run_on(&line, &self.host);
                }
                None
            }
            Event::Pin(label) => {
                let line = format!("accounts use {label} --cli codex");
                if self.records().roster.row(label).is_some() {
                    let _ = self.pool.run_on(&line, &self.host);
                }
                None
            }
            Event::Wall(label) => {
                let now = self.pool.env.now;
                self.store()
                    .transact(now, &mut None, |state| {
                        Ok(accounts::note_wall(
                            state,
                            label,
                            "seven_day",
                            Some(now + 86_400),
                            now,
                            now,
                        ))
                    })
                    .expect("the wall");
                None
            }
            Event::Launch => {
                let unread = |_: Cli, _: &Path, _: Option<&str>, _: i64| None;
                let took = crate::harness::accounts_pool::launch_env_for(
                    &self.pool.seat(),
                    Cli::Codex,
                    &unread,
                    Some(&self.host.host.program),
                );
                if let Ok(Some(env)) = took {
                    let records = self.records();
                    if let Some(row) = records.roster.row(&env.label)
                        && self.names_another(row).is_some()
                    {
                        wrong.push(format!(
                            "a launch took `{}`, whose login names another account",
                            row.label
                        ));
                    }
                }
                None
            }
        }
    }

    /// The address `row`'s login names where that is another account than the row's.
    fn names_another(&self, row: &accounts::Row) -> Option<String> {
        let dir = row
            .dir
            .as_ref()
            .map_or_else(|| self.home().join(".codex"), PathBuf::from);
        if !crate::ai::facts::codex_file_store(&dir) {
            return None;
        }
        let named = crate::harness::codex_app_login::read_account_email(&dir)?;
        let expected = row.expected_identity_email()?;
        (!accounts::email_matches(expected, &named)).then_some(named)
    }

    /// What holds after every step, whatever the step was: `event` happened, from `before`.
    fn hold(
        &self,
        event: Event,
        before: &BeforeStep,
        said: Option<&(ExitCode, String, String)>,
        wrong: &mut Vec<String>,
    ) {
        let records = self.records();
        let roster = &records.roster;
        let default_dir = self.home().join(".codex");
        // Every login that was here is here, and a row or a row's `was` reaches it: a sign-in
        // replaces its own row's, which stays in its directory until the store collects it.
        let replaced: Option<PathBuf> = match event {
            Event::Names(_) | Event::NamesNobody | Event::Absent => Some(default_dir.clone()),
            Event::SignIn(label) => before.dirs.get(label).cloned(),
            _ => None,
        };
        if let Event::SignIn(_) = event {
            self.superseded.borrow_mut().extend(replaced.clone());
        }
        let after = self.logins();
        for (dir, bytes) in &before.logins {
            if Some(dir) != replaced.as_ref() && !after.values().any(|held| held == bytes) {
                wrong.push(format!("a login left: the one in {}", tail(dir)));
            }
        }
        let was = |label: &str| {
            records
                .logins
                .get(label)
                .map(|binding| binding.was.clone())
                .unwrap_or_default()
        };
        let mut reached: BTreeSet<PathBuf> = BTreeSet::new();
        for row in &roster.rows {
            reached.insert(
                row.dir
                    .as_ref()
                    .map_or_else(|| default_dir.clone(), PathBuf::from),
            );
            for left in was(&row.label) {
                reached.insert(left.map_or_else(|| default_dir.clone(), PathBuf::from));
            }
        }
        for dir in after.keys() {
            if !reached.contains(dir) && !self.superseded.borrow().contains(dir) {
                wrong.push(format!("a login no row reaches: the one in {}", tail(dir)));
            }
        }
        // At most one row owns a directory.
        let mut owned = BTreeSet::new();
        for row in &roster.rows {
            if !owned.insert(row.dir.clone()) {
                wrong.push(format!(
                    "two rows own one directory: `{}`'s {}",
                    row.label,
                    row.dir.as_deref().map_or("default login", tail_str)
                ));
            }
        }
        // A row whose login names another account is said by the report, and no launch takes it.
        let (text, json) = self.pool.report_on(&self.host.host);
        let enrolled = enrolled_at(Cli::Codex, roster, &self.pool.env);
        let picked = crate::harness::accounts_pool::launch_observed(
            (Cli::Codex, false, accounts::Mode::Early),
            roster,
            &records.state,
            &enrolled,
            Some(&self.host.host.program),
            self.pool.env.now,
            (&self.home(), None),
        );
        if let Ok(Some(row)) = picked
            && self.names_another(row).is_some()
        {
            wrong.push(format!(
                "a launch would take `{}`, whose login names another account",
                row.label
            ));
        }
        let not = not_next_json(&json);
        for row in roster.rows.iter().filter(|row| row.held_by.is_none()) {
            if self.names_another(row).is_some() && !not.contains_key(&row.label) {
                wrong.push(format!(
                    "the report does not say `{}`'s login names another account: {text}",
                    row.label
                ));
            }
        }
        let Some((code, out, err)) = said else {
            return;
        };
        // What a receipt counts is what the file holds.
        if let Some(count) = out
            .split(" rows in the roster")
            .next()
            .filter(|_| out.contains(" rows in the roster"))
            .and_then(|head| head.rsplit(' ').next())
            .and_then(|count| count.parse::<usize>().ok())
        {
            let held = Roster::load_for(
                Cli::Codex,
                &self
                    .pool
                    .env
                    .accounts_roster_for(Cli::Codex)
                    .expect("a roster"),
            )
            .expect("the roster")
            .rows
            .len();
            if count != held {
                wrong.push(format!(
                    "the receipt counts {count} rows, and the file holds {held}"
                ));
            }
        }
        // A sync that refuses leaves the roster or its receipt names each change.
        if *code != ExitCode::SUCCESS {
            let raw = Roster::load_for(
                Cli::Codex,
                &self
                    .pool
                    .env
                    .accounts_roster_for(Cli::Codex)
                    .expect("a roster"),
            )
            .expect("the roster");
            for row in &raw.rows {
                let same = before.raw.iter().any(|old| old == row);
                if !same && !out.contains(&row.label) && !err.contains(&row.label) {
                    wrong.push(format!(
                        "a refused sync changed `{}` and did not say so: {out}{err}",
                        row.label
                    ));
                }
            }
        }
    }
}

/// A path's last two parts, which say the same on every run.
fn tail(path: &Path) -> String {
    let parts: Vec<_> = path.iter().rev().take(2).collect();
    parts
        .iter()
        .rev()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn tail_str(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What a step starts from.
struct BeforeStep {
    logins: BTreeMap<PathBuf, Vec<u8>>,
    dirs: BTreeMap<&'static str, PathBuf>,
    raw: Vec<accounts::Row>,
}

impl Walk {
    fn before_step(&self) -> BeforeStep {
        let records = self.records();
        let mut dirs = BTreeMap::new();
        for label in ["main", "work", "third"] {
            if let Some(row) = records.roster.row(label) {
                dirs.insert(
                    label,
                    row.dir
                        .as_ref()
                        .map_or_else(|| self.home().join(".codex"), PathBuf::from),
                );
            }
        }
        BeforeStep {
            logins: self.logins(),
            dirs,
            raw: Roster::load_for(
                Cli::Codex,
                &self
                    .pool
                    .env
                    .accounts_roster_for(Cli::Codex)
                    .expect("a roster"),
            )
            .expect("the roster")
            .rows,
        }
    }

    /// Takes `events` from `start`, holding [`Walk::hold`] after each, and the writer to the report
    /// at the end.
    fn run(start: Start, events: &[Event], wrong: &mut Vec<(String, String)>) {
        let walk = Walk::new(start);
        let mut found = Vec::new();
        for (at, event) in events.iter().enumerate() {
            let before = walk.before_step();
            let said = walk.happen(*event, &mut found);
            walk.hold(*event, &before, said.as_ref(), &mut found);
            if said.is_some() {
                let settled = walk.fingerprint();
                let again = match event {
                    Event::SyncSheet => walk.sync_sheet(),
                    Event::SyncLab => walk.sync_lab(),
                    _ => walk.sync_plain(),
                };
                if walk.fingerprint() != settled {
                    found.push(format!(
                        "a sync repeated with nothing changed wrote: {}{}",
                        again.1, again.2
                    ));
                }
            }
            for why in found.drain(..) {
                wrong.push((why, format!("{start:?} {:?} at {at}", &events[..=at])));
            }
        }
        let labels: Vec<String> = walk
            .records()
            .roster
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect();
        for label in labels {
            for why in walk.pool.law_wall(&walk.host.host, &label) {
                wrong.push((why, format!("{start:?} {events:?} at the end")));
            }
        }
    }
}

/// THE POOL KEEPS EVERY LOGIN AND SAYS WHAT IT DID, WHATEVER HAPPENS TO IT: from each roster (two
/// rows, three, a hold, a custody row, a twin), through every short sequence of a sign-in outside
/// aterm, a sync of each source, a row's sign-in, a pin, a wall and a launch, and a long walk through
/// every change of the default login with a sync after each, by the real entries: every login that
/// was here still is, a row or its `was` reaches it; at most one row owns a directory; a row whose
/// login names another account is said by the report, and no launch takes it; what a receipt counts
/// is what the file holds; a sync repeated with nothing changed writes nothing; a sync that refuses
/// leaves the roster or names each change; and the report's `at a weekly limit:` is what the writer
/// does. FLOOR: the walks it takes cannot silently shrink.
#[test]
fn a_walk_keeps_every_login_and_says_what_it_did() {
    let mut walks: Vec<(Start, Vec<Event>)> = Vec::new();
    for start in STARTS {
        for a in CROSSED
            .into_iter()
            .filter(|_| CROSSED_STARTS.contains(&start))
        {
            for b in CROSSED {
                for c in CROSSED {
                    walks.push((start, vec![a, b, c]));
                }
            }
        }
        for a in ONCE {
            for b in CROSSED {
                walks.push((start, vec![a, b, Event::Sync]));
            }
        }
        // Every change of the default login, each followed by each sync: `a` to `b` once.
        const NAMES: [Event; 7] = [
            Event::Names(OWNER),
            Event::Names(WORK),
            Event::Names(THIRD),
            Event::Names(FAR),
            Event::NamesNobody,
            Event::Absent,
            Event::Keyring,
        ];
        for sync in [Event::Sync, Event::SyncSheet, Event::SyncLab] {
            let mut long = Vec::new();
            for a in NAMES {
                for b in NAMES {
                    long.extend([a, sync, b, sync]);
                }
            }
            walks.push((start, long));
        }
    }
    assert!(walks.len() >= 1_250, "{} walks", walks.len());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let found: Vec<Vec<(String, String)>> = std::thread::scope(|scope| {
        walks
            .chunks(walks.len().div_ceil(threads))
            .map(|share| {
                scope.spawn(move || {
                    let mut wrong = Vec::new();
                    for (start, events) in share {
                        Walk::run(*start, events, &mut wrong);
                    }
                    wrong
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|run| run.join().expect("a share"))
            .collect()
    });
    let mut classes = BTreeMap::<String, (usize, String)>::new();
    for (why, at) in found.into_iter().flatten() {
        let class: String = why
            .split_whitespace()
            .filter(|word| !word.contains("/var/") && !word.contains("/tmp"))
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(70)
            .collect();
        let entry = classes.entry(class).or_insert((0, format!("{at}: {why}")));
        entry.0 += 1;
    }
    assert!(
        classes.is_empty(),
        "{} classes:\n{}",
        classes.len(),
        classes
            .values()
            .map(|(count, example)| format!("{count}x {example}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
