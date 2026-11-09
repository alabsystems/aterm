// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Law B: a bounded exhaustive walk of a Codex pool through what happens to it, by the product's own
//! sync, report, launch, sign-in and writer entries. No world runs a person's or a vendor's program,
//! and every address is synthetic.

use super::*;
use crate::harness::accounts_sync::run_cli_accounts_sync_with;
use crate::harness::alab_roster::test_machine;
use crate::harness::alab_roster::{self, Lab, LabAccount};
use std::collections::BTreeSet;
use std::time::SystemTime;

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
        preferred: [(test_machine("m16"), Some(1))].into_iter().collect(),
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
            me: Some(test_machine("m16")),
            settled: true,
            accounts,
            aliases: BTreeMap::new(),
            mailboxes: BTreeMap::new(),
            #[cfg(unix)]
            machine_name: None,
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

    /// The last of `events` happens to the world the ones before it left, holding [`Walk::hold`]
    /// after it, and after a sync, that the same sync repeated writes nothing.
    fn step(&self, start: Start, events: &[Event], wrong: &mut Vec<(String, String)>) {
        let at = events.len() - 1;
        let event = events[at];
        let mut found = Vec::new();
        let before = self.before_step();
        let said = self.happen(event, &mut found);
        self.hold(event, &before, said.as_ref(), &mut found);
        if said.is_some() {
            let settled = self.fingerprint();
            let again = match event {
                Event::SyncSheet => self.sync_sheet(),
                Event::SyncLab => self.sync_lab(),
                _ => self.sync_plain(),
            };
            if self.fingerprint() != settled {
                found.push(format!(
                    "a sync repeated with nothing changed wrote: {}{}",
                    again.1, again.2
                ));
            }
        }
        for why in found {
            wrong.push((why, format!("{start:?} {events:?} at {at}")));
        }
    }

    /// At the end of the walk `events`: the writer to the report, for each row.
    fn end(&self, start: Start, events: &[Event], wrong: &mut Vec<(String, String)>) {
        let labels: Vec<String> = self
            .records()
            .roster
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect();
        for label in labels {
            for why in self.pool.law_wall(&self.host.host, &label) {
                wrong.push((why, format!("{start:?} {events:?} at the end")));
            }
        }
    }

    /// Takes every walk of `tree` on from the world `events` left, depth first: [`Walk::step`] into
    /// each next event, and [`Walk::end`] where a walk ends. Where they part, it takes the world's
    /// [`Image`] and lays it again before each branch that is not the first thing done there, a
    /// walk's end included ([`Walk::restore`]), so a prefix the walks share is taken once, and each
    /// branch starts from the world that prefix left. How many walks it ended.
    fn explore(
        &self,
        start: Start,
        tree: &Tree,
        events: &mut Vec<Event>,
        wrong: &mut Vec<(String, String)>,
    ) -> usize {
        let image = (tree.next.len() + usize::from(tree.ends > 0) > 1).then(|| self.image());
        let mut ended = 0;
        if tree.ends > 0 {
            self.end(start, events, wrong);
            ended += tree.ends;
        }
        for (n, (event, next)) in tree.next.iter().enumerate() {
            if let Some(image) = &image
                && (n > 0 || tree.ends > 0)
            {
                self.restore(image);
            }
            events.push(*event);
            self.step(start, events, wrong);
            ended += self.explore(start, next, events, wrong);
            events.pop();
        }
        ended
    }

    /// The world as it is now.
    fn image(&self) -> Image {
        Image {
            entries: scan(self.pool.tmp.path()),
            superseded: self.superseded.borrow().clone(),
        }
    }

    /// The world `image` took, laid again in place: what came under the pool's root since goes,
    /// what went comes back, and a file or directory whose bytes, mode or last write changed is
    /// put back; what holds as it was is left as it is. Then a fresh look must find `image`.
    fn restore(&self, image: &Image) {
        let root = self.pool.tmp.path();
        let now = scan(root);
        // Each entry the image does not hold, or holds as another kind, before its directory.
        for (rel, entry) in now.iter().rev() {
            if image
                .entries
                .get(rel)
                .is_some_and(|kept| kept.kind() == entry.kind())
            {
                continue;
            }
            let gone = match entry {
                Entry::Dir { .. } => std::fs::remove_dir(root.join(rel)),
                Entry::File { .. } | Entry::Link(_) => std::fs::remove_file(root.join(rel)),
            };
            gone.unwrap_or_else(|why| panic!("{}: {why}", rel.display()));
        }
        // Each entry the image holds, after its directory.
        for (rel, entry) in &image.entries {
            let path = root.join(rel);
            let held = now.get(rel).filter(|held| held.kind() == entry.kind());
            match entry {
                Entry::Dir { .. } => {
                    if held.is_none() {
                        std::fs::create_dir(&path).expect("a directory");
                    }
                }
                Entry::File {
                    mode,
                    modified,
                    bytes,
                } => {
                    let (was_mode, was_modified, was) = match held {
                        Some(Entry::File {
                            mode,
                            modified,
                            bytes,
                        }) => (Some(*mode), Some(*modified), Some(bytes)),
                        _ => (None, None, None),
                    };
                    let written = was != Some(bytes);
                    if written {
                        if was.is_some() {
                            std::fs::remove_file(&path).expect("a file to write again");
                        }
                        std::fs::write(&path, bytes).expect("a file");
                    }
                    if written || was_mode != Some(*mode) {
                        set_mode(&path, *mode);
                    }
                    if written || was_modified != Some(*modified) {
                        set_modified(&path, *modified);
                    }
                }
                Entry::Link(to) => {
                    if held != Some(entry) {
                        if held.is_some() {
                            std::fs::remove_file(&path).expect("a link to lay again");
                        }
                        link(to, &path);
                    }
                }
            }
        }
        // Then each directory's mode and last write, which what came and went in it moved.
        for (rel, entry) in &image.entries {
            if let Entry::Dir { mode, modified } = entry {
                let path = root.join(rel);
                let meta = std::fs::symlink_metadata(&path).expect("a directory");
                if mode_of(&meta) != *mode {
                    set_mode(&path, *mode);
                }
                if meta.modified().ok() != Some(*modified) {
                    set_modified(&path, *modified);
                }
            }
        }
        self.superseded.replace(image.superseded.clone());
        if let Some(differs) = image.differs(&self.image()) {
            panic!("the world laid again is not its image: {differs}");
        }
    }
}

/// A walk's world as a step reads it by path: each entry under the pool's root by its path there
/// (a directory or a file with its permission bits and last write, a file's bytes, a link's
/// target), and [`Walk::superseded`]. It holds no entry's identity (its inode, its last status
/// change) and no listing's order: a world laid again from it is a fresh pool's equal only while
/// the product keeps none of those from one command to the next, as the side-by-side trace of the
/// two runners found on macOS (2026-11-09).
#[derive(Clone, PartialEq, Eq)]
struct Image {
    entries: BTreeMap<PathBuf, Entry>,
    superseded: BTreeSet<PathBuf>,
}

/// One entry under a pool's root: a directory or a file, with its permission bits and last write,
/// and what a file holds, or a link and what it names.
#[derive(Clone, PartialEq, Eq)]
enum Entry {
    Dir {
        mode: u32,
        modified: SystemTime,
    },
    File {
        mode: u32,
        modified: SystemTime,
        bytes: Vec<u8>,
    },
    Link(PathBuf),
}

impl Entry {
    fn kind(&self) -> &'static str {
        match self {
            Entry::Dir { .. } => "directory",
            Entry::File { .. } => "file",
            Entry::Link(_) => "link",
        }
    }

    /// What a failure says of it.
    fn said(entry: Option<&Entry>) -> String {
        match entry {
            None => "nothing".to_owned(),
            Some(Entry::Dir { mode, modified }) => format!("a directory {mode:o}, {modified:?}"),
            Some(Entry::File {
                mode,
                modified,
                bytes,
            }) => format!(
                "a file {mode:o}, {modified:?}: {}",
                String::from_utf8_lossy(bytes)
            ),
            Some(Entry::Link(to)) => format!("a link to {}", to.display()),
        }
    }
}

impl Image {
    /// Where `other` is not this image, if it is not: the first path whose entry differs, or the
    /// directories each says a sign-in moved a row out of.
    fn differs(&self, other: &Image) -> Option<String> {
        if self.superseded != other.superseded {
            return Some(format!(
                "superseded {:?}, and {:?}",
                self.superseded, other.superseded
            ));
        }
        let paths: BTreeSet<&PathBuf> = self.entries.keys().chain(other.entries.keys()).collect();
        paths
            .into_iter()
            .find(|rel| self.entries.get(*rel) != other.entries.get(*rel))
            .map(|rel| {
                format!(
                    "{}: {}, and {}",
                    rel.display(),
                    Entry::said(self.entries.get(rel)),
                    Entry::said(other.entries.get(rel))
                )
            })
    }
}

/// Each entry under `root`, `root` itself the empty path, without following a link.
fn scan(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut entries = BTreeMap::new();
    let mut look = vec![PathBuf::new()];
    while let Some(rel) = look.pop() {
        let path = root.join(&rel);
        let meta = std::fs::symlink_metadata(&path).expect("an entry");
        let modified = meta.modified().expect("a last write");
        let entry = if meta.file_type().is_symlink() {
            Entry::Link(std::fs::read_link(&path).expect("a link"))
        } else if meta.is_dir() {
            for child in std::fs::read_dir(&path).expect("a directory") {
                look.push(rel.join(child.expect("an entry").file_name()));
            }
            Entry::Dir {
                mode: mode_of(&meta),
                modified,
            }
        } else if meta.is_file() {
            Entry::File {
                mode: mode_of(&meta),
                modified,
                bytes: std::fs::read(&path).expect("a file"),
            }
        } else {
            panic!("{}: neither a directory, a file nor a link", rel.display());
        };
        entries.insert(rel, entry);
    }
    entries
}

/// An entry's permission bits; where there are none, whether it is read-only.
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        u32::from(meta.permissions().readonly())
    }
}

/// `path` given `mode` ([`mode_of`]).
fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::Permissions::from_mode(mode)
    };
    #[cfg(not(unix))]
    let permissions = {
        let mut permissions = std::fs::symlink_metadata(path)
            .expect("an entry")
            .permissions();
        permissions.set_readonly(mode == 1);
        permissions
    };
    std::fs::set_permissions(path, permissions).expect("a mode");
}

/// `path`'s last write put at `modified`.
fn set_modified(path: &Path, modified: SystemTime) {
    #[cfg(unix)]
    let file = std::fs::File::open(path);
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    };
    file.and_then(|file| file.set_modified(modified))
        .expect("a last write");
}

/// A link at `path` naming `to`.
fn link(to: &Path, path: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(to, path).expect("a link");
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(to, path).expect("a link");
}

/// Walks as a tree of their events: each walk is a path from the root, and walks that share a
/// prefix share its nodes.
#[derive(Default)]
struct Tree {
    /// How many walks end here.
    ends: usize,
    /// Each event a walk takes next from here, in the order the walks first take it.
    next: Vec<(Event, Tree)>,
}

impl Tree {
    fn of(walks: &[Vec<Event>]) -> Tree {
        let mut root = Tree::default();
        for walk in walks {
            let mut node = &mut root;
            for event in walk {
                let at = match node.next.iter().position(|(taken, _)| taken == event) {
                    Some(at) => at,
                    None => {
                        node.next.push((*event, Tree::default()));
                        node.next.len() - 1
                    }
                };
                node = &mut node.next[at].1;
            }
            node.ends += 1;
        }
        root
    }
}

/// The walks taken from `start`: every three steps of [`CROSSED`] from a crossed start, each of
/// [`ONCE`] before each of [`CROSSED`] and a sync, and a long walk through every change of the
/// default login with a sync after each.
fn walks_from(start: Start) -> Vec<Vec<Event>> {
    let mut walks: Vec<Vec<Event>> = Vec::new();
    for a in CROSSED
        .into_iter()
        .filter(|_| CROSSED_STARTS.contains(&start))
    {
        for b in CROSSED {
            for c in CROSSED {
                walks.push(vec![a, b, c]);
            }
        }
    }
    for a in ONCE {
        for b in CROSSED {
            walks.push(vec![a, b, Event::Sync]);
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
        walks.push(long);
    }
    walks
}

/// THE POOL KEEPS EVERY LOGIN AND SAYS WHAT IT DID, WHATEVER HAPPENS TO IT: from `start`'s roster,
/// through [`walks_from`] by the real entries: every login that was here still is, a row or its
/// `was` reaches it; at most one row owns a directory; a row whose login names another account is
/// said by the report, and no launch takes it; what a receipt counts is what the file holds; a sync
/// repeated with nothing changed writes nothing; a sync that refuses leaves the roster or names each
/// change; and the report's `at a weekly limit:` is what the writer does. FLOOR: the walks it takes
/// are `walks`, and it ends each of them, so they cannot silently shrink. One pool takes them all
/// as a [`Tree`] ([`Walk::explore`]): a prefix the walks share is taken once, and each walk that
/// parts from it starts from the world it left, laid again from its [`Image`]. The walks run one
/// after another on this thread, the runner's own threads being the only concurrency: their work
/// is file calls, which more threads here only slow down.
///
/// NEGATIVE CONTROL (2026-11-09): with `accounts_pool::check_launch_login_identity` passing every
/// login, every start but [`Start::Twin`] fails with "a launch would take" at the classes. With
/// [`Walk::explore`] taking
/// only the first next event two steps in, `from_two` fails at the walks it ended (87 of 381).
fn walk_law(start: Start, walks: usize) {
    let taken = walks_from(start);
    assert_eq!(taken.len(), walks, "{start:?}: the walks it takes");
    let mut found = Vec::new();
    let walk = Walk::new(start);
    let ended = walk.explore(start, &Tree::of(&taken), &mut Vec::new(), &mut found);
    assert_eq!(ended, walks, "{start:?}: the walks it ended");
    let mut classes = BTreeMap::<String, (usize, String)>::new();
    for (why, at) in found {
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

/// A WORLD LAID AGAIN IS THE ONE ITS IMAGE TOOK: after the default login's bytes and last write
/// changed, new files and a new directory tree came, a file and a directory with what it held went
/// and a directory was superseded, and (on unix) the roster's mode changed and a link came,
/// [`Walk::restore`] puts back each, as read here apart from the images it compares.
///
/// NEGATIVE CONTROL (2026-11-09): with [`scan`] looking into no directory under the root, the
/// restore's own look passes, and this fails at the default login's text. With a restore that
/// removes nothing, the restore fails its own look ("the world laid again is not its image"), here
/// and in each walk ([`walk_law`]).
#[test]
fn a_world_laid_again_is_the_one_its_image_took() {
    let walk = Walk::new(Start::Two);
    let root = walk.pool.tmp.path().to_path_buf();
    let image = walk.image();
    let default = walk.default_file();
    let written = |path: &Path| {
        std::fs::metadata(path)
            .expect("a file")
            .modified()
            .expect("a last write")
    };
    let (login, login_written) = (
        std::fs::read_to_string(&default).expect("the default login"),
        written(&default),
    );
    let work = root.join("logins/codex/work/auth.json");
    let work_login = std::fs::read_to_string(&work).expect("work's login");
    let package = std::fs::read(&walk.pool.old).expect("the old package");
    let came = walk.home().join("came");
    law_auth(&default, FAR, Some(FAR));
    std::fs::create_dir_all(came.join("deeper")).expect("a directory");
    std::fs::write(came.join("deeper/file"), "came").expect("a file");
    std::fs::write(walk.home().join(".codex/came.json"), "{}").expect("a file");
    std::fs::write(root.join("came.txt"), "came").expect("a file");
    std::fs::remove_file(&work).expect("work's login");
    std::fs::remove_dir_all(root.join("store/old")).expect("the old package");
    walk.superseded.borrow_mut().insert(root.join("moved"));
    #[cfg(unix)]
    let roster = {
        use std::os::unix::fs::PermissionsExt as _;
        let roster = walk
            .pool
            .env
            .accounts_roster_for(Cli::Codex)
            .expect("a roster");
        let mode = std::fs::metadata(&roster)
            .expect("the roster")
            .permissions()
            .mode();
        std::fs::set_permissions(&roster, std::fs::Permissions::from_mode(0o604)).expect("a mode");
        std::os::unix::fs::symlink("came.txt", root.join("link")).expect("a link");
        (roster, mode)
    };
    walk.restore(&image);
    assert_eq!(
        std::fs::read_to_string(&default).expect("the default login"),
        login
    );
    assert_eq!(written(&default), login_written);
    assert_eq!(
        std::fs::read_to_string(&work).expect("work's login"),
        work_login
    );
    assert_eq!(
        std::fs::read(&walk.pool.old).expect("the old package"),
        package
    );
    for gone in [
        came,
        walk.home().join(".codex/came.json"),
        root.join("came.txt"),
        root.join("link"),
    ] {
        assert!(
            std::fs::symlink_metadata(&gone).is_err(),
            "{}",
            gone.display()
        );
    }
    assert!(walk.superseded.borrow().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let (roster, mode) = roster;
        assert_eq!(
            std::fs::metadata(&roster)
                .expect("the roster")
                .permissions()
                .mode(),
            mode
        );
    }
}

/// [`walk_law`] from each roster: two rows, three, a hold, a custody row, a twin, and a default
/// row whose managed directory holds another account's login.
mod a_walk_keeps_every_login_and_says_what_it_did {
    use super::{Start, walk_law};

    #[test]
    fn from_two() {
        walk_law(Start::Two, 381);
    }

    #[test]
    fn from_three() {
        walk_law(Start::Three, 38);
    }

    #[test]
    fn from_held() {
        walk_law(Start::Held, 381);
    }

    #[test]
    fn from_custody() {
        walk_law(Start::Custody, 38);
    }

    #[test]
    fn from_twin() {
        walk_law(Start::Twin, 381);
    }

    #[test]
    fn from_occupied() {
        walk_law(Start::Occupied, 38);
    }
}
