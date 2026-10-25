// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The roster keeping up with this machine's own logins (the m2 incident of 2026-10-24): a
//! default login the vendor's `/login` moved to another account leaves the row it wrote over on
//! record as signed out and to be signed in, an account no row lists gets a row of its own, and
//! local discovery reads transcripts of any size, line by line, adding what it finds to a fleet
//! discovery made and never to one a vault, a sheet or a URL wrote. Every fixture email is
//! `example.test`.

use std::sync::{Arc, Mutex};

use super::*;

pub(super) const NOW: i64 = 1_792_660_000;

/// A scratch home, state and config directory, `[fleet] machine = "<machine>"`.
pub(super) struct Desk {
    _dir: aterm_tempfile::TempDir,
    pub(super) home: PathBuf,
    pub(super) state: PathBuf,
    pub(super) config: PathBuf,
}

impl Desk {
    pub(super) fn new(machine: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-sync-logins-")
            .tempdir()
            .expect("tempdir");
        let root = dir.path().to_path_buf();
        let (home, config) = (root.join("home"), root.join("config"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("aterm.toml"),
            format!("[fleet]\nmachine = \"{machine}\"\n"),
        )
        .unwrap();
        Self {
            _dir: dir,
            home,
            state: root.join("state"),
            config,
        }
    }

    pub(super) fn roster(&self) -> PathBuf {
        self.config.join(accounts::ACCOUNTS_FILE)
    }

    pub(super) fn fleet_file(&self) -> PathBuf {
        self.config.join(FLEET_ROSTER_FILE)
    }

    pub(super) fn store(&self) -> accounts::Store {
        accounts::Store::at(&self.state)
    }

    /// Claude Code's default login signed in as `email` (its profile, as `/login` leaves it).
    pub(super) fn default_login(&self, email: &str) {
        fs::write(
            self.home.join(".claude.json"),
            format!("{{\"oauthAccount\":{{\"emailAddress\":\"{email}\"}}}}\n"),
        )
        .unwrap();
    }

    pub(super) fn transcript(&self, name: &str) -> PathBuf {
        let project = self.home.join(".claude/projects/-Users-someone-work");
        fs::create_dir_all(&project).unwrap();
        project.join(format!("{name}.jsonl"))
    }

    pub(super) fn rows(&self) -> Roster {
        Roster::load(&self.roster()).expect("the roster admits")
    }
}

/// A transcript's `session_context` line naming `email`, as Claude Code writes it.
fn session_context(email: &str) -> String {
    format!(
        "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"session_context\",\
         \"context\":{{\"userEmail\":\"{email}\",\"cwd\":\"/Users//someone/work\"}}}}}}\n"
    )
}

/// A fleet of `(label, machine, email)` rows.
fn fleet_of(rows: &[(&str, &str, &str)]) -> FleetRoster {
    let text: String = rows
        .iter()
        .map(|(label, machine, email)| {
            format!(
                "[[account]]\nlabel = \"{label}\"\nmachines = [\"{machine}\"]\nemail = \"{email}\"\n\n"
            )
        })
        .collect();
    FleetRoster::from_toml(&text, "fixture").expect("the fixture fleet admits")
}

fn sync(desk: &Desk, fleet: &FleetRoster, machine: &str, default: &str, now: i64) -> Roster {
    persist_accounts_sync(
        &desk.store(),
        &desk.roster(),
        (fleet, OptIn::Named),
        (machine, Some(default)),
        &BTreeMap::new(),
        now,
        (&[], &[]),
    )
    .expect("the sync publishes");
    Roster::load(&desk.roster()).expect("the published roster admits")
}

fn displaced(store: &accounts::Store) -> Vec<accounts::Event> {
    store
        .tail(1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == accounts::Kind::Displaced)
        .collect()
}

/// m2's afternoon: m2-2 held the default login and was pinned; the person's `/login` signed
/// the default login in as m2's account, writing over m2-2's one login. m2 is the default row
/// now, and m2-2 is signed out, unpinned, no longer served, one `displaced` event on record and
/// first in line for the sign-in ladder. CONTROLS: a sync whose default login is the same
/// account writes no event, before the move and after it.
#[test]
fn a_moved_default_login_leaves_the_row_it_wrote_over_signed_out_and_next_to_sign_in() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
    ]);
    let roster = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW);
    assert_eq!(roster.row_for_login(None).unwrap().label, "m2-2");
    assert_eq!(
        store.load(NOW).unwrap().0.account("m2-2").auth,
        accounts::Auth::Ok
    );
    let mut note = None;
    store
        .transact(NOW + 1, &mut note, |state| {
            let enrolled = accounts::enrolled(&roster);
            accounts::pin(state, &roster, &enrolled, "m2-2", NOW + 1).map(|e| vec![e])
        })
        .unwrap();

    let again = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 30);
    assert_eq!(again, roster);
    assert!(
        displaced(&store).is_empty(),
        "CONTROL: the same default login"
    );

    let moved = sync(&desk, &fleet, "m2", "fb@example.test", NOW + 60);
    assert_eq!(moved.row_for_login(None).unwrap().label, "m2");
    assert!(moved.row("m2-2").unwrap().dir.is_some());
    let (state, _) = store.load(NOW + 60).unwrap();
    let (logins, _) = store.load_logins(NOW + 60).unwrap();
    assert_eq!(state.account("m2-2").auth, accounts::Auth::SignedOut);
    assert!(!state.pinned, "the pin on the row written over ends");
    assert_eq!(logins.get("m2-2").unwrap().served_at, None);
    assert_eq!(state.account("m2").auth, accounts::Auth::Ok);
    let events = displaced(&store);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].account, "m2-2");
    assert_eq!(events[0].to.as_deref(), Some("m2"));
    assert_eq!(events[0].why, "default login now m2");
    assert!(
        store
            .tail(1000)
            .unwrap()
            .iter()
            .any(|e| e.kind == accounts::Kind::Unpin && e.account == "m2-2")
    );
    let next = auto_renew_candidates(&moved, &state, &logins, NOW + 60);
    assert!(
        next.iter().any(|row| row.label == "m2-2"),
        "the row written over is signed in by the ladder: {next:?}"
    );

    sync(&desk, &fleet, "m2", "fb@example.test", NOW + 90);
    assert_eq!(displaced(&store).len(), 1, "CONTROL: one event per move");
}

/// The default login holds an account none of this machine's rows lists: it gets a row of its
/// own (labelled as discovery labels, unique in the fleet), never the old default row's label;
/// an account another machine's row lists is listed here by that row. The fleet a source wrote
/// is not added to.
#[test]
fn a_default_login_no_row_lists_gets_a_row_of_its_own() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
        ("m5", "m5", "five@example.test"),
    ]);
    sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW);

    let roster = sync(
        &desk,
        &fleet,
        "m2",
        "andrewyates.m2@pm.example.test",
        NOW + 60,
    );
    let own = roster.row_for_login(None).unwrap();
    assert_eq!(own.label, "m2-3");
    assert_eq!(own.email.as_deref(), Some("andrewyates.m2@pm.example.test"));
    assert_eq!(
        roster.row("m2-2").unwrap().email.as_deref(),
        Some("proton.m2@example.test"),
        "the old label keeps its own account"
    );
    assert!(roster.row("m2-2").unwrap().dir.is_some());
    assert_eq!(
        store.load(NOW + 60).unwrap().0.account("m2-3").auth,
        accounts::Auth::Ok
    );
    assert_eq!(displaced(&store).len(), 1);
    assert_eq!(displaced(&store)[0].to.as_deref(), Some("m2-3"));
    assert!(
        !fs::read_to_string(desk.fleet_file())
            .unwrap()
            .contains("pm.example.test"),
        "a fleet a source wrote is not added to"
    );

    let roster = sync(&desk, &fleet, "m2", "five@example.test", NOW + 120);
    assert_eq!(roster.row_for_login(None).unwrap().label, "m5");
    assert_eq!(roster.rows.len(), 3);
}

/// A row whose login directory holds no login is never on record as working. CONTROL: a row
/// whose directory holds its login keeps its word.
#[test]
fn a_row_whose_directory_holds_no_login_is_not_on_record_as_working() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
        ("m2-3", "m2", "third.m2.3@example.test"),
    ]);
    let roster = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW);
    let made = Path::new(roster.row("m2-3").unwrap().dir.as_deref().unwrap());
    fs::write(made.join(".credentials.json"), "fixture login").unwrap();
    let mut note = None;
    store
        .transact(NOW + 1, &mut note, |state| {
            let mut events = accounts::note_auth(state, "m2", accounts::Auth::Ok, NOW + 1);
            events.extend(accounts::note_auth(
                state,
                "m2-3",
                accounts::Auth::Ok,
                NOW + 1,
            ));
            Ok(events)
        })
        .unwrap();
    sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 60);
    let (state, _) = store.load(NOW + 60).unwrap();
    assert_eq!(state.account("m2").auth, accounts::Auth::SignedOut);
    assert_eq!(state.account("m2-3").auth, accounts::Auth::Ok, "CONTROL");
    assert_eq!(state.account("m2-2").auth, accounts::Auth::Ok, "CONTROL");
}

/// A transcript past the 4 MiB a source file may be is read line by line: its email is found
/// after a line too long to keep, and a line appended later is read without reading the file
/// again from its start; a budget cuts a read short and the next read goes on from there.
#[test]
fn discovery_reads_a_transcript_of_any_size_line_by_line() {
    let desk = Desk::new("m27");
    let path = desk.transcript("long");
    let filler = format!(
        "{{\"type\":\"assistant\",\"message\":{{\"content\":\"{}\"}}}}\n",
        "x".repeat(1000)
    );
    let mut text = session_context("first.m27@example.test");
    text.push_str(&format!(
        "{{\"type\":\"user\",\"content\":\"{}\"}}\n",
        "y".repeat(TRANSCRIPT_LINE_MAX + 1)
    ));
    while text.len() <= SOURCE_TEXT_MAX_BYTES + 512 * 1024 {
        text.push_str(&filler);
    }
    let deep = text.len() as u64;
    text.push_str(&session_context("deep.m27.2@example.test"));
    fs::write(&path, &text).unwrap();
    assert!(
        read_local_source_text(&path).is_err(),
        "a source file this size is refused"
    );

    let scans: TranscriptScans = Mutex::new(BTreeMap::new());
    let paths = [path.clone()];
    let cut = transcript_emails(&paths, &scans, deep - 10);
    assert_eq!(
        cut,
        BTreeSet::from(["first.m27@example.test".to_owned()]),
        "the budget stops before the deep line"
    );
    let all = transcript_emails(&paths, &scans, u64::MAX);
    assert!(all.contains("deep.m27.2@example.test"), "{all:?}");

    let mut appended = fs::OpenOptions::new().append(true).open(&path).unwrap();
    appended
        .write_all(session_context("later.m27.3@example.test").as_bytes())
        .unwrap();
    drop(appended);
    let small = session_context("later.m27.3@example.test").len() as u64;
    let later = transcript_emails(&paths, &scans, small);
    assert!(
        later.contains("later.m27.3@example.test") && later.contains("deep.m27.2@example.test"),
        "an appended line is read from where the last read stopped: {later:?}"
    );

    fs::write(desk.home.join(".claude.json"), "{}\n").unwrap();
    let env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    let local = discover_local_fleet_with_min(&env, 1).expect("discovery finds the accounts");
    let emails: Vec<&str> = local.accounts.iter().map(|a| a.email.as_str()).collect();
    for email in [
        "first.m27@example.test",
        "deep.m27.2@example.test",
        "later.m27.3@example.test",
    ] {
        assert!(emails.contains(&email), "{email}: {emails:?}");
    }
}

/// A line longer than the most a read keeps is passed over whole, and the lines around it are
/// read; the last line is read with or without its newline.
#[test]
fn a_bounded_line_read_passes_over_a_long_line_and_keeps_the_rest() {
    let text = format!("one\n{}\ntwo\nthree", "z".repeat(40));
    let mut reader = io::BufReader::with_capacity(8, text.as_bytes());
    let mut lines = Vec::new();
    let through = each_bounded_line(&mut reader, 16, |line| {
        lines.push(String::from_utf8(line.to_vec()).unwrap());
    })
    .unwrap();
    assert_eq!(lines, ["one", "two", "three"]);
    assert_eq!(through, (text.len() - "three".len()) as u64);
}

/// One window upkeep pass that syncs alone (no renewal) at `now`.
fn sync_pass(desk: &Desk, fx: &UpkeepEffects<'_>, now: i64) {
    upkeep(
        &desk.home,
        &desk.state,
        (&desk.roster(), now, UpkeepStep::Sync),
        fx,
    );
}

pub(super) fn upkeep_effects<'a>(
    fetch: &'a Fetch<'a>,
    spawn: &'a SpawnRenewal,
    cooldown: &'a Mutex<Option<Instant>>,
    rediscovery: &'a Mutex<Rediscovery>,
    guard: &'a Arc<Mutex<AutoRenewGuard>>,
) -> UpkeepEffects<'a> {
    UpkeepEffects {
        fetch,
        spawn_renewal: spawn,
        sync_cooldown: cooldown,
        rediscovery,
        renew_guard: guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: Arc::new(|_: &str| {}),
        run_attempt: None,
    }
}

/// A window's upkeep over a fleet discovery made: an account first seen in a transcript past
/// 4 MiB becomes a row of this machine (fleet and roster), and the default login's move to
/// another row is synced at once, its displaced row on record. CONTROL: a pass with nothing new
/// changes nothing and writes no event.
#[test]
fn a_discovered_fleet_takes_the_accounts_this_machine_shows_later() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\"}}\n",
    )
    .unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Discovered)
    );
    assert_eq!(desk.rows().rows.len(), 2);

    let mut text = String::new();
    while text.len() <= SOURCE_TEXT_MAX_BYTES {
        text.push_str("{\"type\":\"progress\",\"data\":\"..............................\"}\n");
    }
    text.push_str(&session_context("star.m27.3@example.test"));
    fs::write(desk.transcript("big"), text).unwrap();

    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    sync_pass(&desk, &fx, NOW + 60);
    let roster = desk.rows();
    let star = roster
        .rows
        .iter()
        .find(|r| r.email.as_deref() == Some("star.m27.3@example.test"))
        .expect("the transcript's account is a row");
    assert_eq!(star.label, "m27-3");
    assert!(star.dir.is_some());
    let fleet_text = fs::read_to_string(desk.fleet_file()).unwrap();
    assert!(fleet_text.contains("star.m27.3@example.test"));
    assert_eq!(opt_in_of(&fleet_text), Some(OptIn::Discovered));

    let (roster_bytes, ledger) = (fs::read(desk.roster()).unwrap(), desk.store().tail(1000));
    sync_pass(&desk, &fx, NOW + 61);
    assert_eq!(fs::read(desk.roster()).unwrap(), roster_bytes, "CONTROL");
    assert_eq!(desk.store().tail(1000).unwrap(), ledger.unwrap(), "CONTROL");

    desk.default_login("sun.m27.2@example.test");
    sync_pass(&desk, &fx, NOW + 62);
    let roster = desk.rows();
    let own = roster.row_for_login(None).unwrap();
    assert_eq!(own.email.as_deref(), Some("sun.m27.2@example.test"));
    let events = displaced(&desk.store());
    assert_eq!(events.len(), 1, "{events:?}");
    let was = roster.row(&events[0].account).unwrap();
    assert_eq!(was.email.as_deref(), Some("sol-m27@example.test"));
    let (state, _) = desk.store().load(NOW + 62).unwrap();
    assert_eq!(state.account(&was.label).auth, accounts::Auth::SignedOut);
}

/// m2 as it stood on 2026-10-24: discovery wrote its fleet before it recorded `discovered`, so
/// the file says `named` and lists m2's rows alone, each an account m2's own files show (the
/// default login's, and the one its transcripts ran under before the `/login`), and the account
/// it lacked appears only in transcripts past 4 MiB. The upkeep adds it as `m2-3` (labelled
/// from its own email, as a default login no row lists would be); the file keeps the word it
/// records.
#[test]
fn a_named_fleet_of_this_machine_alone_takes_what_discovery_finds() {
    let desk = Desk::new("m2");
    desk.default_login("andrewyates.fb@example.test");
    let fleet = fleet_of(&[
        ("m2", "m2", "andrewyates.fb@example.test"),
        ("m2-2", "m2", "andrewyates.m2@proton.example.test"),
    ]);
    fs::write(desk.fleet_file(), fleet.to_toml_for(Some(OptIn::Named))).unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("named fleet");
    assert_eq!(desk.rows().rows.len(), 2);

    let mut text = session_context("andrewyates.m2@proton.example.test");
    while text.len() <= SOURCE_TEXT_MAX_BYTES {
        text.push_str("{\"type\":\"progress\",\"data\":\"..............................\"}\n");
    }
    text.push_str(&session_context("andrewyates.m2@pm.example.test"));
    fs::write(desk.transcript("tab-1"), text).unwrap();
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    sync_pass(&desk, &fx, NOW + 60);
    let roster = desk.rows();
    assert_eq!(
        roster.row("m2-3").and_then(|r| r.email.as_deref()),
        Some("andrewyates.m2@pm.example.test"),
        "{roster:?}"
    );
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Named),
        "the file keeps its word"
    );
}

/// A list a person handed `accounts sync` that names an account this Mac shows nothing of is
/// theirs: an account found later in this Mac's transcripts is never added to it, and the file
/// keeps its word and its rows. CONTROL: the account the transcripts show is one discovery
/// finds.
#[test]
fn a_named_list_naming_an_account_this_mac_shows_nothing_of_is_never_added_to() {
    let desk = Desk::new("m8");
    desk.default_login("own.m8@example.test");
    let fleet = fleet_of(&[
        ("m8", "m8", "own.m8@example.test"),
        ("m8-2", "m8", "listed.m8.2@example.test"),
    ]);
    let named = fleet.to_toml_for(Some(OptIn::Named));
    fs::write(desk.fleet_file(), &named).unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("named fleet");
    let held = fs::read_to_string(desk.fleet_file()).unwrap();
    fs::write(
        desk.transcript("tab"),
        session_context("found.m8.3@example.test"),
    )
    .unwrap();
    let env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    assert!(
        discover_local_fleet_with_min(&env, 1).is_some_and(|f| f
            .accounts
            .iter()
            .any(|a| a.email == "found.m8.3@example.test")),
        "CONTROL"
    );
    assert!(!init_sync_needed(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW + 60,
        true
    ));
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    sync_pass(&desk, &fx, NOW + 60);
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW + 61).expect("sync");
    assert_eq!(fs::read_to_string(desk.fleet_file()).unwrap(), held);
    assert_eq!(fleet_file_opt_in(&desk.fleet_file()), Some(OptIn::Named));
    assert!(
        !desk
            .rows()
            .rows
            .iter()
            .any(|r| r.email.as_deref() == Some("found.m8.3@example.test"))
    );
}

/// The default login's row that no fleet row lists keeps its label from sync to sync; when the
/// fleet later gives that label to another account, the default login's account takes a new
/// label and its record (its wall, its auth word) goes with it: the other account starts with
/// nothing on record, and no `displaced` event is written, since no sign-in wrote over a login.
/// CONTROL: a sync over the same fleet keeps the label.
#[test]
fn a_label_the_fleet_gives_another_account_takes_none_of_the_default_logins_record() {
    let desk = Desk::new("m8");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m8", "m8", "a.m8@example.test"),
        ("m8-2", "m8", "b.m8.2@example.test"),
    ]);
    let default = "andrewyates.m8@pm.example.test";
    let roster = sync(&desk, &fleet, "m8", default, NOW);
    let own = roster.row_for_login(None).unwrap().label.clone();
    assert_eq!(own, "m8-3");
    store
        .transact(NOW + 1, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                &own,
                "seven_day",
                Some(NOW + 86_400),
                NOW,
                NOW + 1,
            ))
        })
        .unwrap();
    let again = sync(&desk, &fleet, "m8", default, NOW + 30);
    assert_eq!(
        again.row_for_login(None).unwrap().label,
        "m8-3",
        "CONTROL: the label stands"
    );

    let grown = fleet_of(&[
        ("m8", "m8", "a.m8@example.test"),
        ("m8-2", "m8", "b.m8.2@example.test"),
        ("m8-3", "m8", "d.m8.3@example.test"),
    ]);
    let moved = sync(&desk, &grown, "m8", default, NOW + 60);
    let now_own = moved.row_for_login(None).unwrap();
    assert_eq!(now_own.email.as_deref(), Some(default));
    assert_ne!(now_own.label, "m8-3");
    assert_eq!(
        moved.row("m8-3").and_then(|r| r.email.as_deref()),
        Some("d.m8.3@example.test")
    );
    let (state, _) = store.load(NOW + 60).unwrap();
    assert!(
        state
            .account(&now_own.label)
            .limited_until(NOW + 60)
            .is_some(),
        "the wall went with the account"
    );
    assert!(
        state.account("m8-3").limited_until(NOW + 60).is_none(),
        "the account the fleet gave the label to is not walled"
    );
    assert_eq!(state.account("m8-3").auth, accounts::Auth::Unseen);
    assert!(displaced(&store).is_empty(), "{:?}", displaced(&store));
}

/// A window's upkeep over a fleet a vault delivered (`~/.secrets/fleet-accounts.toml`, rows of
/// several machines): an account found in this machine's transcripts is not added to it, and
/// the vault's file is left as it was; a default login it does not list is still the default
/// row of this machine's roster alone.
#[test]
fn a_fleet_a_vault_delivered_is_never_added_to() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    let vault = concat!(
        "[[account]]\nlabel = \"m27\"\nmachines = [\"m27\"]\nemail = \"sol-m27@example.test\"\n\n",
        "[[account]]\nlabel = \"m27-2\"\nmachines = [\"m27\"]\nemail = \"sun.m27.2@example.test\"\n\n",
        "[[account]]\nlabel = \"m28\"\nmachines = [\"m28\"]\nemail = \"m28@example.test\"\n",
    );
    let vault_path = secrets.join(FLEET_ROSTER_FILE);
    fs::write(&vault_path, vault).unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("vault fleet");
    assert_eq!(desk.rows().rows.len(), 2);

    let mut text = String::new();
    while text.len() <= SOURCE_TEXT_MAX_BYTES {
        text.push_str("{\"type\":\"progress\",\"data\":\"..............................\"}\n");
    }
    text.push_str(&session_context("star.m27.3@example.test"));
    fs::write(desk.transcript("big"), text).unwrap();
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    sync_pass(&desk, &fx, NOW + 60);
    assert_eq!(fs::read_to_string(&vault_path).unwrap(), vault);
    assert!(
        !fs::read_to_string(desk.fleet_file())
            .unwrap()
            .contains("star.m27.3@example.test")
    );
    assert!(
        desk.rows()
            .rows
            .iter()
            .all(|r| r.email.as_deref() != Some("star.m27.3@example.test"))
    );

    desk.default_login("visitor@example.test");
    sync_pass(&desk, &fx, NOW + 120);
    let roster = desk.rows();
    let own = roster.row_for_login(None).unwrap();
    assert_eq!(own.email.as_deref(), Some("visitor@example.test"));
    assert_eq!(own.label, "m27-3");
    assert_eq!(fs::read_to_string(&vault_path).unwrap(), vault);
    assert_eq!(displaced(&desk.store()).len(), 1);
}

/// A roster written by hand, with no source, is left as it was when the default login moves.
#[test]
fn a_roster_with_no_source_is_not_synced_for_a_moved_default_login() {
    let desk = Desk::new("m27");
    desk.default_login("visitor@example.test");
    let hand =
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.test\"\n";
    fs::write(desk.roster(), hand).unwrap();
    let rediscovery = Mutex::new(Rediscovery::default());
    assert!(!default_login_unlisted(
        &desk.home,
        &desk.state,
        (&desk.roster(), &desk.config.join("aterm.toml")),
        NOW,
        &rediscovery,
    ));
    assert_eq!(fs::read_to_string(desk.roster()).unwrap(), hand);
}
