// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The roster keeping up with this machine's own logins (the m2 incident of 2026-10-24): a
//! default login the vendor's `/login` moved to another account leaves the row it wrote over on
//! record as signed out and to be signed in, an account no row lists gets a row of its own, and
//! local discovery reads transcripts of any size, line by line, adding what it finds to a fleet
//! discovery made and never to one a vault, a sheet or a URL wrote. Every fixture email is
//! synthetic.

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
        aterm_uds::atomic_file::write(
            &config.join("aterm.toml"),
            format!("[fleet]\nmachine = \"{machine}\"\n").as_bytes(),
            0o600,
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

#[test]
fn machine_identity_is_not_inherited_from_the_default_logins_fleet_row() {
    for machine in ["m30", ""] {
        let desk = Desk::new(machine);
        desk.default_login("sol-m27@example.test");
        let profile = desk.home.join(".claude.json");
        let before = fs::read(&profile).unwrap();
        let fleet = fleet_of(&[
            ("m27", "m27", "sol-m27@example.test"),
            ("m27-2", "m27", "sun-m27@example.test"),
            ("m30", "m30", "moon-m30@example.test"),
        ]);
        fs::write(desk.fleet_file(), fleet.to_toml_for(Some(OptIn::Named))).unwrap();

        let synced = sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW);
        if machine.is_empty() {
            assert!(
                synced.is_err(),
                "a provider login cannot identify this machine: {synced:?}"
            );
            assert!(
                !desk.roster().exists(),
                "no other machine's roster is published"
            );
        } else {
            assert_eq!(synced.expect("explicit local identity").0, machine);
            assert!(desk.rows().row("m27-2").is_none());
        }
        assert_eq!(fs::read(&profile).unwrap(), before);
    }
}

#[test]
fn machine_identity_is_not_inferred_from_discovered_addresses() {
    for machine in ["m30", ""] {
        let desk = Desk::new(machine);
        desk.default_login("sol-m27@example.test");
        fs::write(
            desk.home.join(".claude.json.backup"),
            "{\"oauthAccount\":{\"emailAddress\":\"owner@example.test\"}}\n",
        )
        .unwrap();
        let mut env = quiet_env(
            &desk.home,
            &desk.state,
            &desk.config.join("aterm.toml"),
            NOW,
        );
        let discovered = discover_local_fleet_with_min(&env, 1);
        if machine.is_empty() {
            assert!(
                discovered.is_none(),
                "addresses do not assign a machine: {discovered:?}"
            );
        } else {
            let fleet =
                discovered.expect("the explicitly identified machine can discover accounts");
            assert_eq!(fleet.accounts.len(), 1);
            assert_eq!(fleet.accounts[0].machines, [machine]);
            assert_eq!(fleet.accounts[0].email, "owner@example.test");
        }
        assert!(!desk.fleet_file().exists(), "discovery only observes");
        assert!(!desk.roster().exists(), "discovery only observes");

        // Count only local_shows' bounded local reader. Discovery's later current-default
        // read is independent and remains above, outside this operation's captured inputs.
        let reads = std::cell::RefCell::new(BTreeMap::<PathBuf, usize>::new());
        let read = |path: &Path| {
            *reads.borrow_mut().entry(path.to_owned()).or_default() += 1;
            read_local_source_text(path)
        };
        let shown = local_shows(&env, &read);
        assert!(shown.claude.contains("sol-m27@example.test"));
        assert!(shown.claude.contains("owner@example.test"));
        assert_eq!(
            reads.borrow().get(&desk.home.join(".claude.json.backup")),
            Some(&1)
        );
        assert_eq!(
            reads.borrow().get(&desk.home.join(".claude.json")),
            Some(&1)
        );
        if !machine.is_empty() {
            let primary = desk.home.join(".claude.json");
            desk.default_login("fresh@example.test");
            reads.borrow_mut().clear();
            let fresh = local_shows(&env, &read);
            assert!(fresh.claude.contains("fresh@example.test"));
            assert!(!fresh.claude.contains("sol-m27@example.test"));
            assert_eq!(
                reads.borrow().get(&primary),
                Some(&1),
                "a new call reads fresh inputs"
            );

            // A failed parse is not a successful observation to reuse: the enumerated file
            // can have become valid after that first attempt.
            fs::write(&primary, "{").unwrap();
            reads.borrow_mut().clear();
            let repaired = std::cell::Cell::new(false);
            let recovered = local_shows(&env, &|path| {
                let result = read(path);
                if path.as_os_str() == primary.as_os_str() && !repaired.replace(true) {
                    desk.default_login("recovered@example.test");
                }
                result
            });
            assert!(recovered.claude.contains("recovered@example.test"));
            assert_eq!(reads.borrow().get(&primary), Some(&2));

            let custom = desk.home.join(".claude-custom-oauth.json");
            fs::write(
                &custom,
                r#"{"oauthAccount":{"emailAddress":"custom@example.test"}}"#,
            )
            .unwrap();
            env.claude_custom_oauth = true;
            reads.borrow_mut().clear();
            let custom_seen = local_shows(&env, &read);
            assert!(custom_seen.claude.contains("custom@example.test"));
            assert!(custom_seen.claude.contains("recovered@example.test"));
            assert_eq!(reads.borrow().get(&custom), Some(&1));
            assert_eq!(
                reads.borrow().get(&primary),
                Some(&1),
                "a distinct default path skips no ordinary profile"
            );

            let legacy = desk.home.join(".claude/.config.json");
            fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            fs::write(
                &legacy,
                r#"{"oauthAccount":{"emailAddress":"legacy@example.test"}}"#,
            )
            .unwrap();
            env.claude_custom_oauth = false;
            reads.borrow_mut().clear();
            let legacy_seen = local_shows(&env, &read);
            assert!(legacy_seen.claude.contains("legacy@example.test"));
            assert!(legacy_seen.claude.contains("recovered@example.test"));
            assert_eq!(reads.borrow().get(&legacy), Some(&1));
            assert_eq!(reads.borrow().get(&primary), Some(&1));
        }
    }
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
/// now, and m2-2 is signed out, unpinned, no longer served and one `displaced` event on record.
/// No session here runs on it, so the sign-in ladder leaves it for a person or a session
/// (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4); once a session here runs on it, it is
/// the ladder's. CONTROLS: a sync whose default login is the same account writes no event,
/// before the move and after it.
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
        next.iter().all(|row| row.label != "m2-2"),
        "no session here runs on the row written over: {next:?}"
    );
    let mut running = state.clone();
    let _ = accounts::note_session(&mut running, "s-tab", "m2-2", None, NOW + 60);
    let next = auto_renew_candidates(&moved, &running, &logins, NOW + 60);
    assert!(
        next.iter().any(|row| row.label == "m2-2"),
        "a session here runs on the row written over: {next:?}"
    );

    sync(&desk, &fleet, "m2", "fb@example.test", NOW + 90);
    assert_eq!(displaced(&store).len(), 1, "CONTROL: one event per move");
}

/// A row written over reads signed out whatever its own directory holds: a footprint there is no
/// login (`accounts::login_made`), and its next sign-in says otherwise.
#[test]
fn a_row_written_over_reads_signed_out_whatever_its_own_directory_holds() {
    for footprint in [false, true] {
        let desk = Desk::new("m2");
        let store = desk.store();
        let fleet = fleet_of(&[
            ("m2", "m2", "fb@example.test"),
            ("m2-2", "m2", "proton.m2@example.test"),
        ]);
        sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW);
        if footprint {
            let dir = store.logins_dir().join("m2-2");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".claude.json"), "{}").unwrap();
        }
        let moved = sync(&desk, &fleet, "m2", "fb@example.test", NOW + 60);
        let row = moved.row("m2-2").unwrap();
        assert!(row.dir.is_some());
        assert_eq!(displaced(&store).len(), 1, "footprint={footprint}");
        let (state, _) = store.load(NOW + 60).unwrap();
        assert_eq!(
            accounts::condition(row, &state, true, NOW + 60),
            accounts::Condition::SignedOut,
            "footprint={footprint}"
        );
    }
}

/// m2's morning (2026-10-27): m2-2 was on record as signed out (its own login seen ended) with a
/// failed automatic sign-in, and the default login held m2; a person's `/login` signed the
/// default login in as m2-2's account. m2 is displaced, and m2-2 holds a login made now: Ok,
/// served, not expired, no failed sign-in, no ended login on record, not a candidate of the
/// sign-in ladder, and the sessions that ran on the default login are its. m2, whose login here
/// was written over a minute after sessions here ran on it, is the ladder's renewal (a login
/// this Mac used within the week that has ended, §4.15 rule 4), and a week later it waits for a
/// person. CONTROLS: m2-2's
/// login then seen ended while the default login still names it stays signed out at the next
/// sync (a login merely still matched is not one made now); a row moved onto the default login
/// by a relabel of the same account keeps its sign-out.
#[test]
fn a_default_login_moved_onto_a_signed_out_row_makes_that_rows_login_now() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
    ]);
    let roster = sync(&desk, &fleet, "m2", "fb@example.test", NOW);
    assert_eq!(roster.row_for_login(None).unwrap().label, "m2");
    let own = PathBuf::from(roster.row("m2-2").unwrap().dir.clone().unwrap());
    fs::write(own.join(".credentials.json"), "fixture login").unwrap();
    let mut note = None;
    store
        .transact_logins(NOW + 10, &mut note, |state, logins| {
            let mut events = accounts::note_session(state, "s-tab-1", "m2", None, NOW + 10);
            events.extend(accounts::note_session(
                state,
                "s-tab-2",
                "m2",
                None,
                NOW + 10,
            ));
            events.extend(accounts::note_session(
                state,
                "s-own",
                "m2-2",
                None,
                NOW + 10,
            ));
            events.extend(accounts::note_login_end(state, logins, "m2-2", NOW + 20));
            events.extend(accounts::note_sign_in_attempt(
                logins,
                "m2-2",
                accounts::AttemptOutcome::NoRoute,
                "profile(none) mailbox(none configured)",
                NOW + 30,
            ));
            Ok(events)
        })
        .unwrap();
    let (state, _) = store.load(NOW + 40).unwrap();
    let (logins, _) = store.load_logins(NOW + 40).unwrap();
    let before = Roster::load(&desk.roster()).unwrap();
    assert_eq!(
        accounts::login_state(before.row("m2-2").unwrap(), &logins, &state, NOW + 40),
        accounts::LoginState::Expired,
        "the incident's start: m2-2 expired on record"
    );

    let moved = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 60);
    assert_eq!(moved.row_for_login(None).unwrap().label, "m2-2");
    let (state, _) = store.load(NOW + 60).unwrap();
    let (logins, _) = store.load_logins(NOW + 60).unwrap();
    let row = moved.row("m2-2").unwrap();
    assert_eq!(
        state.account("m2").auth,
        accounts::Auth::SignedOut,
        "m2 displaced"
    );
    assert_eq!(displaced(&store).len(), 1);
    assert_eq!(state.account("m2-2").auth, accounts::Auth::Ok);
    assert_eq!(
        accounts::login_state(row, &logins, &state, NOW + 60),
        accounts::LoginState::Ok,
        "not expired: a turn's login, served now"
    );
    let binding = logins.get("m2-2").unwrap();
    assert_eq!(binding.served_at, Some(NOW + 60));
    assert_eq!(
        binding.ended_at, None,
        "its earlier login's end is no news of this one"
    );
    assert!(
        binding.sign_in_attempt.is_none(),
        "{:?}",
        binding.sign_in_attempt
    );
    let raw = Roster::load(&desk.roster()).unwrap();
    assert!(
        accounts::failing_sign_ins(&moved, &raw, &state, &logins, NOW + 60).is_empty(),
        "no row shows a failed sign-in"
    );
    let next = auto_renew_candidates(&moved, &state, &logins, NOW + 60);
    assert!(
        next.iter().all(|row| row.label != "m2-2"),
        "the ladder does not sign in a login made now: {next:?}"
    );
    assert!(
        next.iter().any(|row| row.label == "m2"),
        "the row written over, which this Mac ran on a minute ago, is the ladder's: {next:?}"
    );
    let week_on = NOW + 10 + accounts::USED_WITHIN_S;
    assert!(
        auto_renew_candidates(&moved, &state, &logins, week_on)
            .iter()
            .all(|row| row.label != "m2"),
        "a week after a session here ran on it, it waits for a person"
    );
    assert_eq!(state.sessions["s-tab-1"].account, "m2-2");
    assert_eq!(state.sessions["s-tab-2"].account, "m2-2");
    assert_eq!(state.sessions["s-own"].account, "m2-2");
    assert!(
        !store
            .tail(1000)
            .unwrap()
            .iter()
            .any(|e| e.kind == accounts::Kind::Move && e.t > NOW + 40),
        "no session moved: the default login did"
    );

    // CONTROL: the default login still names m2-2 and its login is then seen ended.
    let mut note = None;
    store
        .transact_logins(NOW + 70, &mut note, |state, logins| {
            Ok(accounts::note_login_end(state, logins, "m2-2", NOW + 70))
        })
        .unwrap();
    let again = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 90);
    let (state, _) = store.load(NOW + 90).unwrap();
    let (logins, _) = store.load_logins(NOW + 90).unwrap();
    assert_eq!(
        state.account("m2-2").auth,
        accounts::Auth::SignedOut,
        "CONTROL: a login still matched is not one made now"
    );
    assert_eq!(
        accounts::login_state(again.row("m2-2").unwrap(), &logins, &state, NOW + 90),
        accounts::LoginState::Expired
    );
}

/// CONTROL of [`a_default_login_moved_onto_a_signed_out_row_makes_that_rows_login_now`]: a
/// default row relabelled (the same account under a new label) holds the login it held, so its
/// sign-out stands.
#[test]
fn a_relabelled_default_row_keeps_its_sign_out() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let roster = sync(
        &desk,
        &fleet_of(&[("m2", "m2", "fb@example.test")]),
        "m2",
        "fb@example.test",
        NOW,
    );
    assert_eq!(roster.row_for_login(None).unwrap().label, "m2");
    let mut note = None;
    store
        .transact_logins(NOW + 10, &mut note, |state, logins| {
            Ok(accounts::note_login_end(state, logins, "m2", NOW + 10))
        })
        .unwrap();
    let relabelled = sync(
        &desk,
        &fleet_of(&[("m2-main", "m2", "fb@example.test")]),
        "m2",
        "fb@example.test",
        NOW + 60,
    );
    assert_eq!(relabelled.row_for_login(None).unwrap().label, "m2-main");
    let (state, _) = store.load(NOW + 60).unwrap();
    assert_eq!(
        state.account("m2-main").auth,
        accounts::Auth::SignedOut,
        "CONTROL: the login it held, moved with its label"
    );
    assert!(displaced(&store).is_empty());
}

/// CONTROL of [`a_default_login_moved_onto_a_signed_out_row_makes_that_rows_login_now`]: a
/// sign-out on record at or after the sync's own instant came after the default login was made
/// (it was made before the sync saw it), so it is that login's news and stands.
#[test]
fn a_sign_out_on_record_at_the_syncs_instant_stands_on_a_newly_default_row() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
    ]);
    sync(&desk, &fleet, "m2", "fb@example.test", NOW);
    let mut note = None;
    store
        .transact_logins(NOW + 60, &mut note, |state, logins| {
            Ok(accounts::note_login_end(state, logins, "m2-2", NOW + 60))
        })
        .unwrap();
    let moved = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 60);
    assert_eq!(moved.row_for_login(None).unwrap().label, "m2-2");
    let (state, _) = store.load(NOW + 60).unwrap();
    let (logins, _) = store.load_logins(NOW + 60).unwrap();
    assert_eq!(
        state.account("m2-2").auth,
        accounts::Auth::SignedOut,
        "CONTROL: an end no earlier than the sync is the new login's"
    );
    assert_eq!(logins.get("m2-2").and_then(|b| b.ended_at), Some(NOW + 60));
}

/// This Mac on 2026-10-27, after the `/login` that moved the default login onto m2-2: the roster
/// already names m2-2 as the default row (the sync that saw the move ran under an aterm without
/// the repoint), but m2-2 still carries the sign-out its earlier login left on 10-25 and the
/// sessions on the default login are still on record under m2. `sessions` are the default
/// login's sessions on record under m2; the sign-out is written at `signed_out_at`.
fn repointed_with_a_stale_sign_out(desk: &Desk, signed_out_at: i64) -> FleetRoster {
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
    ]);
    sync(desk, &fleet, "m2", "fb@example.test", NOW);
    desk.default_login("proton.m2@example.test");
    let moved = sync(desk, &fleet, "m2", "proton.m2@example.test", NOW + 60);
    assert_eq!(moved.row_for_login(None).unwrap().label, "m2-2");
    assert_eq!(displaced(&store).len(), 1);
    let mut note = None;
    store
        .transact_logins(NOW + 61, &mut note, |state, logins| {
            // What the older aterm left: the sign-out of m2-2's earlier login, its end on record,
            // and the default login's sessions under the label the default login held before.
            let account = state.accounts.entry("m2-2".to_owned()).or_default();
            account.auth = accounts::Auth::SignedOut;
            account.auth_at = Some(signed_out_at);
            if let Some(binding) = logins.labels.get_mut("m2-2") {
                binding.ended_at = Some(signed_out_at);
            }
            let mut events = accounts::note_session(state, "s-tab-1", "m2", None, NOW + 61);
            events.extend(accounts::note_session(
                state,
                "s-tab-2",
                "m2",
                None,
                NOW + 61,
            ));
            Ok(events)
        })
        .unwrap();
    fleet
}

fn checks_signed_in(_: &Path) -> super::super::cli::LoginCheck {
    super::super::cli::LoginCheck::SignedIn
}

fn checks_signed_out(_: &Path) -> super::super::cli::LoginCheck {
    super::super::cli::LoginCheck::SignedOut
}

/// The idempotent heal of [`repointed_with_a_stale_sign_out`]: the sign-out on record is older
/// than the move that put the default login on m2-2, so one read-only check of the default login
/// is due; it answers signed in, and m2-2 holds the login the move made (Ok, served, not expired,
/// its earlier end cleared), with the default login's sessions its. Once healed nothing is due,
/// and a second sync checks nothing. CONTROL: the repoint the sync makes itself is not re-run by
/// a sync whose roster already names m2-2 (the heal is what moves it).
#[test]
fn a_default_row_the_roster_already_named_is_healed_by_one_check() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = repointed_with_a_stale_sign_out(&desk, NOW + 20);
    let roster = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 90);
    let (state, _) = store.load(NOW + 90).unwrap();
    let (logins, _) = store.load_logins(NOW + 90).unwrap();
    assert_eq!(
        accounts::login_state(roster.row("m2-2").unwrap(), &logins, &state, NOW + 90),
        accounts::LoginState::Expired,
        "CONTROL: a sync over a roster that already names m2-2 does not heal it"
    );
    assert_eq!(state.sessions["s-tab-1"].account, "m2");

    let heal = default_login_heal_due(&desk.home, &store, &roster, NOW + 100)
        .expect("the stale sign-out is due one check");
    assert_eq!(
        heal,
        DefaultHeal {
            label: "m2-2".to_owned(),
            expected_identity: "proton.m2@example.test".to_owned(),
            signed_out_at: NOW + 20,
            was: Some("m2".to_owned()),
        }
    );
    let (healed, line) = heal_default_login_now(
        &desk.home,
        &store,
        &desk.roster(),
        NOW + 100,
        checks_signed_in,
    )
    .expect("the check ran");
    assert!(healed, "{line}");
    assert!(
        line.contains("the default login checked signed in"),
        "{line}"
    );
    let (state, _) = store.load(NOW + 100).unwrap();
    let (logins, _) = store.load_logins(NOW + 100).unwrap();
    assert_eq!(state.account("m2-2").auth, accounts::Auth::Ok);
    assert_eq!(
        accounts::login_state(roster.row("m2-2").unwrap(), &logins, &state, NOW + 100),
        accounts::LoginState::Ok
    );
    assert_eq!(logins.get("m2-2").and_then(|b| b.ended_at), None);
    assert_eq!(state.sessions["s-tab-1"].account, "m2-2");
    assert_eq!(state.sessions["s-tab-2"].account, "m2-2");
    assert_eq!(
        state.account("m2").auth,
        accounts::Auth::SignedOut,
        "the row written over stays signed out"
    );
    assert_eq!(
        heal_default_login_now(
            &desk.home,
            &store,
            &desk.roster(),
            NOW + 110,
            checks_signed_in
        ),
        None,
        "once healed nothing is due"
    );
}

/// NEGATIVE of [`a_default_row_the_roster_already_named_is_healed_by_one_check`]: the check says
/// signed out, so the sign-out stands and no session moves; in the window's upkeep the check runs
/// once for that sign-out, whatever the number of passes.
#[test]
fn a_default_row_the_check_finds_signed_out_stays_signed_out_and_is_checked_once() {
    static CHECKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn counted_signed_out(home: &Path) -> super::super::cli::LoginCheck {
        CHECKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        checks_signed_out(home)
    }
    let desk = Desk::new("m2");
    let store = desk.store();
    repointed_with_a_stale_sign_out(&desk, NOW + 20);
    let (healed, line) = heal_default_login_now(
        &desk.home,
        &store,
        &desk.roster(),
        NOW + 100,
        checks_signed_out,
    )
    .expect("the check ran");
    assert!(!healed);
    assert!(line.contains("answered signed out"), "{line}");
    let (state, _) = store.load(NOW + 100).unwrap();
    assert_eq!(state.account("m2-2").auth, accounts::Auth::SignedOut);
    assert_eq!(state.account("m2-2").auth_at, Some(NOW + 20));
    assert_eq!(state.sessions["s-tab-1"].account, "m2");

    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let inline = |work: Box<dyn FnOnce() + Send>| {
        work();
        Ok(())
    };
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let kept = Arc::clone(&lines);
    let mut fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    fx.spawn_vault = &inline;
    fx.default_check = |home, _scope| counted_signed_out(home);
    fx.log = Arc::new(move |line: &str| kept.lock().unwrap().push(line.to_owned()));
    for at in [NOW + 200, NOW + 300, NOW + 400] {
        heal_default_login_in_upkeep(&desk.home, &desk.state, &desk.roster(), at, &fx);
    }
    assert_eq!(CHECKS.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        lines.lock().unwrap().len(),
        1,
        "{:?}",
        lines.lock().unwrap()
    );
    assert_eq!(
        store.load(NOW + 400).unwrap().0.account("m2-2").auth,
        accounts::Auth::SignedOut
    );
}

#[test]
fn default_check_claims_retire_on_unstarted_refused_cancelled_or_unwound_work() {
    for mode in 0..6 {
        let desk = Desk::new("m2");
        repointed_with_a_stale_sign_out(&desk, NOW + 20);
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let queued = Mutex::new(Vec::<Box<dyn FnOnce() + Send>>::new());
        let spawn = |work: Box<dyn FnOnce() + Send>| match mode {
            1 => {
                drop(work);
                Err(io::Error::other("synthetic refusal"))
            }
            3 | 4 => {
                work();
                Ok(())
            }
            _ => {
                queued.lock().unwrap().push(work);
                Ok(())
            }
        };
        let fetch = |_: &Env, _: &str| -> Result<String, String> {
            panic!("default check fetched a roster")
        };
        let renew = |_: Box<dyn FnOnce() + Send>| -> io::Result<()> {
            panic!("default check started a renewal")
        };
        let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
        let mut fx = upkeep_effects(&fetch, &renew, &cooldown, &rediscovery, &guard);
        fx.spawn_vault = &spawn;
        fx.default_check = if mode == 3 {
            |_, _| panic!("synthetic check unwind")
        } else {
            |home, scope| {
                assert!(scope.is_some());
                checks_signed_out(home)
            }
        };
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_scope_effects(scope, fx, |fx| {
                heal_default_login_in_upkeep(
                    &desk.home,
                    &desk.state,
                    &desk.roster(),
                    NOW + 100,
                    fx,
                );
            });
        }));
        assert_eq!(ran.is_err(), mode == 3);
        let pause = lifetime.pause().unwrap();
        if matches!(mode, 0 | 2 | 5) {
            assert_eq!(queued.lock().unwrap().len(), 1);
            assert!(guard.lock().unwrap().default_checked.is_some());
        }
        if mode == 5 {
            guard.lock().unwrap().default_checked = Some(("newer-row".to_owned(), NOW + 50));
        }
        if mode == 2 {
            queued.lock().unwrap().pop().unwrap()();
        } else {
            queued.lock().unwrap().clear();
        }
        let drained = pause.try_wait().expect("all actual work has retired");
        let current = guard.lock().unwrap().default_checked.clone();
        match mode {
            4 => assert!(current.is_some(), "a completed check stays recorded"),
            5 => assert_eq!(current, Some(("newer-row".to_owned(), NOW + 50))),
            _ => assert_eq!(current, None),
        }
        assert!(drained.resume().is_ok());
    }
}

/// CONTROL of [`a_default_row_the_roster_already_named_is_healed_by_one_check`]: a sign-out on
/// record at or after the move that put the default login on m2-2 is that login's own news, so no
/// check is due and it stands, whatever the check would say.
#[test]
fn a_sign_out_written_after_the_move_is_never_checked_away() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let roster = {
        repointed_with_a_stale_sign_out(&desk, NOW + 60);
        desk.rows()
    };
    assert_eq!(
        default_login_heal_due(&desk.home, &store, &roster, NOW + 100),
        None,
        "a sign-out at the move's own instant"
    );
    let desk = Desk::new("m2");
    let store = desk.store();
    repointed_with_a_stale_sign_out(&desk, NOW + 70);
    assert_eq!(
        heal_default_login_now(
            &desk.home,
            &store,
            &desk.roster(),
            NOW + 100,
            checks_signed_in
        ),
        None
    );
    assert_eq!(
        store.load(NOW + 100).unwrap().0.account("m2-2").auth,
        accounts::Auth::SignedOut
    );
}

/// The stored login file the default home keeps for each world of
/// [`the_upkeep_heal_answers_from_the_stored_login_file_and_runs_no_process`]: `None` keeps none
/// (a keychain-held login).
#[cfg(unix)]
fn lay_default_credentials(desk: &Desk, tokens: Option<(&str, &str)>) {
    let Some((access, refresh)) = tokens else {
        return;
    };
    let dir = desk.home.join(".claude");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(".credentials.json"),
        format!(
            "{{\"claudeAiOauth\":{{\"accessToken\":\"{access}\",\"refreshToken\":\"{refresh}\"}}}}"
        ),
    )
    .unwrap();
}

/// A stand-in `claude` where [`resolve_claude_program`] finds it in `desk`'s home
/// (`~/.local/bin/claude`): each run appends one line to the returned file and fails.
#[cfg(unix)]
fn plant_claude_stand_in(desk: &Desk) -> PathBuf {
    let ran = desk.home.join("claude-ran.log");
    let bin = desk.home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let program = bin.join("claude");
    fs::write(
        &program,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", ran.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    ran
}

#[cfg(unix)]
fn runs_noted(ran: &Path) -> usize {
    fs::read_to_string(ran).map_or(0, |text| text.lines().count())
}

/// §4.15 rule 6 for the upkeep's heal of [`repointed_with_a_stale_sign_out`]: the live check
/// ([`default_login_check_live`]) answers from the default home's stored login file alone, so the
/// heal runs no process and notes no probe. A file holding a refresh token heals the row; one
/// holding no token is signed out, and its sign-out stands; no file is no answer, which records
/// nothing. Where the default login is the keychain's (macOS,
/// [`crate::ai::connector::test_on_login_platform`] picks either OS on any), even a file holding
/// a refresh token is not that login and heals nothing. NEGATIVE CONTROL: the planted stand-in
/// is the program the check ran before (`claude auth status` through
/// [`resolve_claude_program`]), and a run of it is noted, so a check that ran it would be seen.
#[cfg(unix)]
#[test]
fn the_upkeep_heal_answers_from_the_stored_login_file_and_runs_no_process() {
    let control = Desk::new("m2");
    let ran = plant_claude_stand_in(&control);
    let program = resolve_claude_program(&control.home);
    let _ = super::super::cli::auth_status_within_for_test(
        program.as_os_str(),
        "",
        &|name| (name == "USER").then(|| "lab".into()),
        Duration::from_secs(60),
    );
    assert_eq!(
        runs_noted(&ran),
        1,
        "CONTROL: the CLI check the heal made before runs the stand-in, and its run is noted"
    );

    for (in_file, tokens, healed_to, words) in [
        (
            true,
            Some(("", "lab-refresh")),
            accounts::Auth::Ok,
            "checked signed in",
        ),
        (
            true,
            Some(("", "")),
            accounts::Auth::SignedOut,
            "answered signed out",
        ),
        (true, None, accounts::Auth::SignedOut, DEFAULT_LOGIN_UNREAD),
        (
            false,
            Some(("", "lab-refresh")),
            accounts::Auth::SignedOut,
            DEFAULT_LOGIN_IN_KEYCHAIN,
        ),
    ] {
        let desk = Desk::new("m2");
        let store = desk.store();
        repointed_with_a_stale_sign_out(&desk, NOW + 20);
        lay_default_credentials(&desk, tokens);
        let ran = plant_claude_stand_in(&desk);
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("no fetch in a test: {source}"))
        };
        let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
        let inline = |work: Box<dyn FnOnce() + Send>| {
            work();
            Ok(())
        };
        let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let kept = Arc::clone(&lines);
        let mut fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
        fx.spawn_vault = &inline;
        fx.default_check = |home, _scope| default_login_check_live(home);
        fx.log = Arc::new(move |line: &str| kept.lock().unwrap().push(line.to_owned()));
        let ((), probes) = crate::ai::connector::test_on_login_platform(in_file, || {
            super::super::provider_read::captured(|| {
                heal_default_login_in_upkeep(
                    &desk.home,
                    &desk.state,
                    &desk.roster(),
                    NOW + 100,
                    &fx,
                );
            })
        });
        let lines = lines.lock().unwrap().clone();
        assert_eq!(lines.len(), 1, "{in_file} {tokens:?}: {lines:?}");
        assert!(lines[0].contains(words), "{in_file} {tokens:?}: {lines:?}");
        assert_eq!(runs_noted(&ran), 0, "{tokens:?}: the heal ran no process");
        assert!(probes.is_empty(), "{tokens:?}: no probe: {probes:?}");
        let (state, _) = store.load(NOW + 100).unwrap();
        assert_eq!(state.account("m2-2").auth, healed_to, "{tokens:?}");
        if healed_to == accounts::Auth::SignedOut {
            assert_eq!(state.account("m2-2").auth_at, Some(NOW + 20), "{tokens:?}");
        }
    }
}

/// The default login's move takes `active` with it: m2 was the active account (pinned) when a
/// `/login` moved the default login onto m2-2, which holds a login made now; `active` names m2-2,
/// proven, with no canary and nothing to go back to, and the listing's `active` mark is m2-2's.
/// CONTROL: an `active` that names a third row is left where it is.
#[test]
fn a_moved_default_login_takes_active_with_it() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
        ("m2-3", "m2", "third@example.test"),
    ]);
    let roster = sync(&desk, &fleet, "m2", "fb@example.test", NOW);
    let mut note = None;
    store
        .transact(NOW + 1, &mut note, |state| {
            let enrolled = accounts::enrolled(&roster);
            accounts::pin(state, &roster, &enrolled, "m2", NOW + 1).map(|e| vec![e])
        })
        .unwrap();
    sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 60);
    let (state, _) = store.load(NOW + 60).unwrap();
    assert_eq!(state.active.as_deref(), Some("m2-2"));
    assert!(state.proven && state.canary.is_none() && !state.pinned && state.undo.is_empty());

    // CONTROL: active on a third row stays there across the move back.
    let mut note = None;
    store
        .transact(NOW + 61, &mut note, |state| {
            state.active = Some("m2-3".to_owned());
            Ok(Vec::new())
        })
        .unwrap();
    sync(&desk, &fleet, "m2", "fb@example.test", NOW + 120);
    let (state, _) = store.load(NOW + 120).unwrap();
    assert_eq!(state.active.as_deref(), Some("m2-3"));
}

/// A row that becomes the default login while it holds a renewal directory bound to it keeps
/// what is on record of it: it launches from that directory, whose login the vendor's sign-in on
/// the default login never touched, so its sign-out stands (no heal is due either). The sessions
/// that ran on the default login, and `active`, are its account's now. CONTROL: the same move onto
/// an unbound row makes its login now.
#[test]
fn a_bound_row_the_default_login_moves_onto_keeps_its_record_and_takes_the_sessions() {
    let desk = Desk::new("m2");
    let store = desk.store();
    let fleet = fleet_of(&[
        ("m2", "m2", "fb@example.test"),
        ("m2-2", "m2", "proton.m2@example.test"),
    ]);
    let roster = sync(&desk, &fleet, "m2", "fb@example.test", NOW);
    let own = roster.row("m2-2").unwrap().dir.clone().unwrap();
    let staging = store.claim_staging_dir("m2-2").unwrap();
    fs::write(staging.join(".credentials.json"), "fixture login").unwrap();
    let staging_text = staging.to_str().unwrap().to_owned();
    let mut note = None;
    store
        .transact_logins(NOW + 10, &mut note, |state, logins| {
            state.active = Some("m2".to_owned());
            let mut events = accounts::note_renewed(
                state,
                logins,
                "m2-2",
                Some(&own),
                &staging_text,
                None,
                NOW + 10,
            );
            events.extend(accounts::note_session(
                state,
                "s-tab-1",
                "m2",
                None,
                NOW + 11,
            ));
            events.extend(accounts::note_login_end(state, logins, "m2-2", NOW + 20));
            Ok(events)
        })
        .unwrap();
    let moved = sync(&desk, &fleet, "m2", "proton.m2@example.test", NOW + 60);
    assert_eq!(moved.row_for_login(None).unwrap().label, "m2-2");
    let (state, _) = store.load(NOW + 60).unwrap();
    let (logins, _) = store.load_logins(NOW + 60).unwrap();
    assert_eq!(
        logins.get("m2-2").and_then(|b| b.dir.as_deref()),
        Some(staging_text.as_str())
    );
    assert_eq!(
        state.account("m2-2").auth,
        accounts::Auth::SignedOut,
        "its bound login's end stands"
    );
    assert_eq!(state.sessions["s-tab-1"].account, "m2-2");
    assert_eq!(state.active.as_deref(), Some("m2-2"));
    assert_eq!(
        default_login_heal_due(&desk.home, &store, &moved, NOW + 100),
        None,
        "no heal is due for a bound row"
    );
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
    spawn: &'a SpawnRenewal<'a>,
    cooldown: &'a Mutex<Option<Instant>>,
    rediscovery: &'a Mutex<Rediscovery>,
    guard: &'a Arc<Mutex<AutoRenewGuard>>,
) -> UpkeepEffects<'a> {
    UpkeepEffects {
        scope: None,
        fetch,
        spawn_renewal: spawn,
        sync_cooldown: cooldown,
        rediscovery,
        renew_guard: guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
    }
}

/// A ROSTER A PERSON MADE KEEPS ITS LABELS THROUGH THE FIRST DISCOVERY: every row keeps its label,
/// directory and order, a row with no address stays, and only an account no row holds joins, under
/// a label no row has. A login that names another address than its row (`old-work`, and an alias
/// with no `identity_email`) keeps its directory and is recorded as a wrong-account login: no row
/// is minted for it. CONTROL: with no roster, discovery labels what it finds from the machine.
#[test]
fn discovery_keeps_a_persons_rows_and_mints_none_for_their_logins() {
    let desk = Desk::new("m27");
    desk.default_login("main@example.test");
    let dir = |name: &str, names: Option<&str>| {
        let dir = desk.store().logins_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".credentials.json"), "opaque fixture").unwrap();
        if let Some(names) = names {
            fs::write(
                dir.join(".claude.json"),
                format!("{{\"oauthAccount\":{{\"emailAddress\":\"{names}\"}}}}\n"),
            )
            .unwrap();
        }
        dir.to_str().unwrap().to_owned()
    };
    let rows = vec![
        Row {
            label: "main".into(),
            email: Some("main@example.test".into()),
            ..Row::default()
        },
        Row {
            label: "old-work".into(),
            dir: Some(dir("old-work", Some("old-work@example.test"))),
            email: Some("work@example.test".into()),
            class: Class::Pool,
            ..Row::default()
        },
        Row {
            label: "alias".into(),
            dir: Some(dir("alias", Some("real@example.test"))),
            email: Some("alias@example.test".into()),
            ..Row::default()
        },
        Row {
            label: "legacy".into(),
            dir: Some(dir("legacy", None)),
            ..Row::default()
        },
    ];
    fs::write(
        desk.roster(),
        format_roster_toml(&Roster { rows: rows.clone() }).unwrap(),
    )
    .unwrap();
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"new.m27.2@example.test\"}}\n",
    )
    .unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Discovered)
    );
    let synced = desk.rows();
    for row in &rows {
        let kept = synced
            .row(&row.label)
            .unwrap_or_else(|| panic!("`{}` kept: {synced:?}", row.label));
        assert_eq!(kept.dir, row.dir, "{}", row.label);
        assert_eq!(kept.email, row.email, "{}", row.label);
        assert_eq!(kept.class, row.class, "{}", row.label);
    }
    let added: Vec<&Row> = synced
        .rows
        .iter()
        .filter(|row| !rows.iter().any(|kept| kept.label == row.label))
        .collect();
    assert_eq!(added.len(), 1, "{synced:?}");
    assert_eq!(added[0].email.as_deref(), Some("new.m27.2@example.test"));
    let (logins, _) = desk.store().load_logins(NOW).unwrap();
    for label in ["old-work", "alias"] {
        assert!(
            logins
                .get(label)
                .is_some_and(|login| login.wrong_at.is_some()),
            "`{label}`'s login names another account"
        );
    }
    assert!(
        logins
            .get("main")
            .is_none_or(|login| login.wrong_at.is_none()),
        "`main`'s names its own"
    );

    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\"}}\n",
    )
    .unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
    let rows = desk.rows().rows;
    assert_eq!(rows.len(), 2, "CONTROL");
    assert!(
        rows.iter().all(|row| row.label.starts_with("m27")),
        "CONTROL: {rows:?}"
    );
}

/// A ROSTER A PERSON MADE KEEPS ITS SEQUENCE, AND A ROW THEY RENAMED, THROUGH DISCOVERY: the rows stay
/// in the file's order, a sync with the old label still in the discovered fleet keeps the new one and
/// lists the account once. CONTROL: a row the discovery adds later follows them.
#[test]
fn discovery_keeps_a_rosters_sequence_and_a_renamed_row() {
    let desk = Desk::new("m27");
    desk.default_login("main@example.com");
    let dir = |name: &str| {
        let dir = desk.store().logins_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".credentials.json"), "opaque fixture").unwrap();
        dir.to_str().unwrap().to_owned()
    };
    let row = |label: &str, email: &str| Row {
        label: label.into(),
        dir: Some(dir(label)),
        email: Some(email.into()),
        ..Row::default()
    };
    let mut rows = vec![
        Row {
            label: "main".into(),
            email: Some("main@example.com".into()),
            ..Row::default()
        },
        row("zeta", "zeta@example.com"),
        row("alpha", "alpha@example.com"),
    ];
    let save = |rows: &[Row]| {
        fs::write(
            desk.roster(),
            format_roster_toml(&Roster {
                rows: rows.to_vec(),
            })
            .unwrap(),
        )
        .unwrap();
    };
    save(&rows);
    let labels = || -> Vec<String> {
        desk.rows()
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect()
    };
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Discovered)
    );
    assert_eq!(labels(), ["main", "zeta", "alpha"], "the file's sequence");

    rows[1].label = "renamed".into();
    save(&rows);
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW)
        .expect("the discovered fleet's old label is the renamed row");
    assert_eq!(
        labels(),
        ["main", "renamed", "alpha"],
        "one account, one row"
    );

    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"new.m27.2@example.com\"}}\n",
    )
    .unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("a new account");
    let now = labels();
    assert_eq!(now.len(), 4, "CONTROL: {now:?}");
    assert_eq!(now[..3], ["main", "renamed", "alpha"], "CONTROL: {now:?}");
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
        None,
    ));
    assert_eq!(fs::read_to_string(desk.roster()).unwrap(), hand);
}

/// A RENAMED ROW FOLLOWS THE DEFAULT LOGIN: discovery still lists the account under its old label,
/// and the row a person renamed is that account, so the login moves to it. So does the row that was
/// on the default login, which leaves it for a directory of its own. No sync is refused, and no two
/// rows are on that login.
#[test]
fn a_renamed_row_follows_the_default_login() {
    for rename_default in [false, true] {
        let desk = Desk::new("m27");
        desk.default_login("main@example.com");
        let dir = |name: &str| {
            let dir = desk.store().logins_dir().join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(".credentials.json"), "opaque fixture").unwrap();
            dir.to_str().unwrap().to_owned()
        };
        let mut rows = vec![
            Row {
                label: "main".into(),
                email: Some("main@example.com".into()),
                ..Row::default()
            },
            Row {
                label: "zeta".into(),
                dir: Some(dir("zeta")),
                email: Some("zeta@example.com".into()),
                ..Row::default()
            },
        ];
        let save = |rows: &[Row]| {
            fs::write(
                desk.roster(),
                format_roster_toml(&Roster {
                    rows: rows.to_vec(),
                })
                .unwrap(),
            )
            .unwrap();
        };
        save(&rows);
        sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
        let renamed = if rename_default { "owner" } else { "renamed" };
        rows[usize::from(!rename_default)].label = renamed.into();
        save(&rows);
        sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("renamed");
        desk.default_login("zeta@example.com");
        sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("followed");
        let raw = Roster::load(&desk.roster()).unwrap();
        let on_login: Vec<&str> = raw
            .rows
            .iter()
            .filter(|row| row.dir.is_none())
            .map(|row| row.label.as_str())
            .collect();
        let zeta = if rename_default { "zeta" } else { "renamed" };
        assert_eq!(on_login, [zeta], "{raw:?}");
        assert!(raw.row(renamed).is_some(), "{raw:?}");
    }
}
