// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! L5 OF THE WINDOW'S LOOK AT THE ACCOUNTS (accounts upkeep on a cause, commit 10d): the host's
//! look ([`accounts_looked`]) takes no clock reading of its own and compares every line's time
//! through the reading it is handed (`aterm_uds::trace::Now`), so a look a trace scope records
//! is kept with the first second that could change a line as its bound. A login coming due is
//! the line placed here: its due line is said, and told, at the second it comes due, and a look a
//! second before it stands until then. Every address is synthetic.

use super::*;
use aterm_uds::trace::{self, Moved, Now};
use std::sync::atomic::AtomicI64;
use std::time::{Duration, Instant};

/// The second the login comes due.
const T: i64 = 1_800_000_000;
const DAY: i64 = 24 * 60 * 60;
/// The one row's label.
const LABEL: &str = "kept";

/// A Mac with one Claude Code login, [`LABEL`], made here in a directory of its own, renewed at a
/// given second and run on by a session here a day before [`T`], and a seat whose clock a test
/// sets.
struct Mac {
    _dir: aterm_tempfile::TempDir,
    roster: PathBuf,
    state: PathBuf,
    home: PathBuf,
    /// The seat's clock ([`AccountSeat::now`]).
    now: AtomicI64,
}

impl Mac {
    /// The Mac, its login renewed at `renewed`.
    fn new(renewed: i64) -> Arc<Self> {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-hh-look-trace-")
            .tempdir()
            .expect("scratch");
        let root = dir.path();
        let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        aterm_uds::atomic_file::write(
            &config.join("aterm.toml"),
            b"[fleet]\nmachine = \"m27\"\n",
            0o600,
        )
        .unwrap();
        let store = accounts::Store::at(&state);
        store.machine_id("0123456789abcdef").unwrap();
        let made = store.logins_dir().join(LABEL);
        std::fs::create_dir_all(&made).unwrap();
        std::fs::write(made.join(".claude.json"), "{}").unwrap();
        let made = made.to_str().unwrap().to_owned();
        let roster = config.join(accounts::ACCOUNTS_FILE);
        let rows = accounts::Roster {
            rows: vec![accounts::Row {
                label: LABEL.to_owned(),
                dir: Some(made.clone()),
                email: Some(format!("{LABEL}@example.test")),
                ..accounts::Row::default()
            }],
        };
        std::fs::write(
            &roster,
            aterm_agent::harness::accounts_sync::format_roster_toml(&rows).unwrap(),
        )
        .unwrap();
        store
            .transact_logins(T - DAY, &mut None, |state, logins| {
                let mut events = accounts::note_renewed(
                    state,
                    logins,
                    LABEL,
                    Some(&made),
                    &made,
                    Some("max"),
                    renewed,
                );
                events.extend(accounts::note_session(state, "s-0", LABEL, None, T - DAY));
                state.sessions.clear();
                Ok(events)
            })
            .unwrap();
        Arc::new(Self {
            _dir: dir,
            roster,
            state,
            home,
            now: AtomicI64::new(T),
        })
    }

    /// The account seat, its clock at `now`.
    fn seat(&self, now: i64) -> AccountSeat {
        AccountSeat {
            roster: Some(self.roster.clone()),
            state: self.state.clone(),
            home: self.home.clone(),
            now,
            ladder: &accounts::LOCK_LADDER,
        }
    }

    /// Hooks whose account seat is this Mac's at its clock ([`Self::now`]), and every note they
    /// hand the window.
    fn hooks(self: &Arc<Self>) -> (Hooks, Arc<Mutex<Vec<HostNote>>>) {
        let world = Arc::new(World::default());
        let mut hooks = hooks(&world, parking_body(&world));
        let mac = Arc::clone(self);
        hooks.acts.account_seat = Arc::new(move || Some(mac.seat(mac.now.load(Ordering::SeqCst))));
        noting(hooks)
    }
}

/// The due line of what the accounts as a whole say, where they list one.
fn due_of(said: Option<&AccountsSaid>) -> Option<Vec<String>> {
    match said? {
        AccountsSaid::Listed { due, .. } => Some(due.clone()),
        _ => None,
    }
}

/// A LOGIN COMING DUE IS TOLD AT THE SECOND IT COMES DUE, AND A LOOK A SECOND BEFORE STANDS UNTIL
/// THEN. The login's forecast end is [`accounts::DUE_S`] past [`T`] (renewed a first lifetime,
/// [`accounts::LIFETIME_START_S`], before that). The Mac's first look draws a machine id for each
/// other CLI's store, which keeps none (`accounts::mint_machine`), once for the process: that look
/// is poisoned. Looked at again two seconds before `T` under a trace scope, as of that second, the
/// look says no due line, and its trace may be kept: it stands at `T - 1` (a keeper would run
/// nothing there) and moves at `T` ([`Moved::Wall`]), its horizon `T` itself. The seat the look
/// is handed reads its own clock a day past `T`, where the row is due: the look compares through
/// the reading it is handed, never through the seat's. Told through the host's hooks, the look at
/// `T - 1` tells a due line with nothing in it, and the look at `T` tells the row due.
///
/// NEGATIVE CONTROLS (2026-11-16):
/// - the due crossing made unrecorded where every line compares it, `recorded_login_state` in
///   aterm-agent's `accounts.rs` (`now.passed(by.saturating_sub(DUE_S))` as
///   `trace::unrecorded(|| now.raw()) >= by.saturating_sub(DUE_S)`), failed `a look a second
///   before the due second stands no longer than it`, left `Ok(())`, right `Err(Wall)`;
/// - [`accounts_looked`] handing [`accounts_said`] the seat's clock (`seat.now` in place of `now`)
///   failed `two seconds before it comes due`, left `Some(["kept"])`, right `Some([])`.
///
/// Each restored, the file touched. Making the due line's own reader unrecorded
/// (`trace::unrecorded(|| accounts::due_logins_here(…))` in `AccountsSaid::listed`) is no control:
/// the none-soon line compares the same crossing (`accounts::none_working_within`, through
/// `accounts::login_state`), and the look stood no longer than it.
#[test]
fn a_login_coming_due_is_told_at_its_due_second_and_a_look_a_second_before_stands_until_it() {
    let mac = Mac::new(T + accounts::DUE_S - accounts::LIFETIME_START_S);
    let switches = switched(&at_wall_only());
    let latches = LoginLatches::default();
    let at = T - 2;
    let seat = mac.seat(T + DAY);
    let look = || {
        trace::read_as_if_later(Duration::from_secs(60), || {
            trace::scope(|| accounts_looked(&switches, &latches, Some((&seat, Now::at(at)))))
        })
    };
    let (_, first) = look();
    assert_eq!(
        first.unkept(),
        Some(trace::Unkept::Poison(
            "a machine id minted from OS entropy".into()
        )),
        "the first look draws the ids the other stores keep none of: {first:?}"
    );
    let (looked, traced) = look();
    assert_eq!(
        due_of(looked.said.as_ref()),
        Some(Vec::new()),
        "two seconds before it comes due: {looked:?}"
    );
    assert_eq!(traced.unkept(), None, "the look may be kept: {traced:?}");
    let mono = Instant::now();
    assert_eq!(
        traced.stands(Now::at(T - 1), mono),
        Ok(()),
        "a second before it comes due, nothing the look compared has moved"
    );
    assert_eq!(
        traced.stands(Now::at(T), mono),
        Err(Moved::Wall),
        "a look a second before the due second stands no longer than it"
    );
    assert_eq!(
        traced.horizon().wall_ns,
        Some(i128::from(T) * 1_000_000_000),
        "the look's horizon is the due second"
    );

    let (hooks, notes) = mac.hooks();
    mac.now.store(T - 1, Ordering::SeqCst);
    accounts_look(&switches, &hooks, &latches, false);
    let told = accounts_told(&notes);
    assert_eq!(
        due_of(told.last()),
        Some(Vec::new()),
        "a second before: {told:?}"
    );
    mac.now.store(T, Ordering::SeqCst);
    accounts_look(&switches, &hooks, &latches, false);
    let told = accounts_told(&notes);
    assert_eq!(
        due_of(told.last()),
        Some(vec![LABEL.to_owned()]),
        "the due line told at the due second: {told:?}"
    );
    assert_eq!(told.len(), 2, "told at each look, each a change: {told:?}");
}
