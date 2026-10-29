// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm partition`'s accounts plane: account claims (F19) across the same split, run as a model
//! on main's own functions under a simulated clock.
//!
//! A lab instance runs with the harness and its Commons runtime off, so no launch or usage tap
//! claims there. So this plane gives each of two Macs its own replica of the claims table
//! (`accounts/claims/1`) and its own accounts state, and drives main's claim path on them
//! ([`touch_with`], [`State::lease_verdict`]) on a clock of its own, a minute a step, with the
//! lane's islands as who hears whom: a2 is on island A and b2 on island B. While the islands are
//! joined, each minute the replicas exchange their rows (the Commons sync) and each Mac hears the
//! other (its Network State); during the cut nothing crosses. Nothing here is the wire; the rows,
//! the state and the verdicts are the production code, called in main's order: a launch reads the
//! claims and, where nothing holds the account elsewhere, claims it as its session starts
//! (`Seat::claim`); a session's usage tap claims again once [`RECHECK_S`] has passed
//! (`claims::tapped`); and at each turn end the session asks production's turn-end advice
//! (`advise` at `Point::TurnEnd`, under the Mac's `[harness] accounts` mode, with the rows its
//! logins enroll) and leaves the account where it says move or wait (a hold), else it goes on.
//! Each Mac wants this one account the whole run, b2 from the start and a2 from five minutes into
//! the cut. By default no other row is enrolled on either Mac, so a junior has nowhere to move:
//! the pool fully used, when collisions happen.
//!
//! Two scenarios, the two F19 names:
//! - `lease`: b2 uses the shared account on a lease when the cut begins. The cut lasts longer than
//!   a lease outlives its unheard Mac ([`PRESENCE_TTL_S`]), so a2 claims it while cut off, each
//!   side settling what it hears; the heal must leave one user.
//! - `home`: b2 uses its home account (`warm[0]`), a claim no silence lapses.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use aterm_agent::harness::accounts::{self, Advice, Class, Cli, Me, Mode, Point, Roster, State};
use aterm_agent::harness::claims::{
    Act, Book, LeaseKey, PRESENCE_TTL_S, Presence, RECHECK_S, Row, Table, Tabled, Touched, View,
    keys, touch_with,
};

/// One tap a minute.
const TAP_S: i64 = 60;
/// The cut begins four minutes in.
const CUT_AT: i64 = 240;
/// The cut lasts longer than a lease outlives its unheard Mac: 30 minutes.
const CUT_FOR: i64 = 30 * 60;
/// The run ends ten minutes after the heal.
const AFTER_S: i64 = 600;
/// a2 starts wanting the account five minutes into the cut.
const A2_WANTS_AT: i64 = CUT_AT + 300;

/// The two Macs' swarm labels.
const A2: &str = "m2";
const B2: &str = "m4";
/// Their Commons machines.
const A2_MACHINE: &str = "a2a2";
const B2_MACHINE: &str = "b2b2";

/// One scenario's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scenario {
    /// `lease` or `home`.
    pub name: &'static str,
    /// Seconds into the run of each change in who uses the account: (t, users).
    pub users: Vec<(i64, Vec<String>)>,
    /// Each minute a Mac used the account while its own row held no live claim on it: (t, user).
    pub unclaimed: Vec<(i64, String)>,
    /// Seconds into the run when b2 stopped using the account it held at the cut, if it did.
    pub b2_stopped: Option<i64>,
    /// Seconds into the run when a2 first used it, if it did.
    pub a2_started: Option<i64>,
    /// b2's verdict on the account at the run's end (`ok` or the reason it may not).
    pub b2_end: String,
    /// The label of the senior live claim on the account as b2 reads it at the run's end, if any.
    pub holder_end: Option<String>,
}

/// The plane's evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// The scenarios, `lease` then `home`.
    pub scenarios: Vec<Scenario>,
}

/// One named check of the plane.
pub type Check = super::Check;

fn mins(s: i64) -> String {
    format!("{}m{:02}s", s / 60, s % 60)
}

/// Judge the accounts plane.
#[must_use]
pub fn judge(ev: &Evidence) -> Vec<Check> {
    let two_users = |s: &Scenario| -> Vec<String> {
        s.users
            .iter()
            .filter(|(_, u)| u.len() > 1)
            .map(|(t, u)| format!("{} at +{}", u.join(" and "), mins(*t)))
            .collect()
    };
    let mut out = Vec::new();
    let lease = ev.scenarios.iter().find(|s| s.name == "lease");
    out.push(match lease {
        None => Check {
            name: "accounts: a split longer than the lease leaves one user per account",
            pass: false,
            detail: "the lease scenario did not run".to_owned(),
        },
        Some(s) => {
            let last = s.users.last().map(|(_, u)| u.clone()).unwrap_or_default();
            let both = two_users(s);
            let settled_by = CUT_AT + CUT_FOR + RECHECK_S;
            let unsettled = s.users.iter().any(|(t, u)| *t > settled_by && u.len() != 1);
            Check {
                name: "accounts: a split longer than the lease leaves one user per account",
                pass: last.len() == 1 && !unsettled,
                detail: format!(
                    "{} after the heal; b2 stopped at {}, a2 started at {}{}",
                    if last.is_empty() {
                        "nobody".to_owned()
                    } else {
                        last.join(" and ")
                    },
                    s.b2_stopped
                        .map_or("never".to_owned(), |t| format!("+{}", mins(t))),
                    s.a2_started
                        .map_or("never".to_owned(), |t| format!("+{}", mins(t))),
                    if both.is_empty() {
                        String::new()
                    } else {
                        format!("; two users {}", both.join("; "))
                    }
                ),
            }
        }
    });
    out.push(match lease {
        None => Check {
            name: "accounts: the cut-off island kept its leased account through the cut",
            pass: false,
            detail: "the lease scenario did not run".to_owned(),
        },
        Some(s) => Check {
            name: "accounts: the cut-off island kept its leased account through the cut",
            pass: s.b2_stopped.is_none_or(|t| t >= CUT_AT + CUT_FOR),
            detail: s.b2_stopped.map_or_else(
                || "b2 used it throughout".to_owned(),
                |t| {
                    format!(
                        "b2 lost it {} into the cut ({}); at the end it reads {}",
                        mins(t - CUT_AT),
                        if t < CUT_AT + CUT_FOR {
                            "while cut off"
                        } else {
                            "after the heal"
                        },
                        s.b2_end
                    )
                },
            ),
        },
    });
    out.push(match lease {
        None => Check {
            name: "accounts: a Mac claims an account before it uses it",
            pass: false,
            detail: "the lease scenario did not run".to_owned(),
        },
        Some(_) => {
            let unclaimed: Vec<String> = ev
                .scenarios
                .iter()
                .flat_map(|s| {
                    s.unclaimed
                        .iter()
                        .map(move |(t, m)| format!("{m} at +{} ({})", mins(*t), s.name))
                })
                .collect();
            Check {
                name: "accounts: a Mac claims an account before it uses it",
                pass: unclaimed.is_empty(),
                detail: if unclaimed.is_empty() {
                    "every use was under the user's own live claim".to_owned()
                } else {
                    format!("used with no claim of its own: {}", unclaimed.join("; "))
                },
            }
        }
    });
    let home = ev.scenarios.iter().find(|s| s.name == "home");
    out.push(match home {
        None => Check {
            name: "accounts: home use during the split blocks a cold claim elsewhere",
            pass: false,
            detail: "the home scenario did not run".to_owned(),
        },
        Some(s) => {
            let both = two_users(s);
            Check {
                name: "accounts: home use during the split blocks a cold claim elsewhere",
                pass: both.is_empty(),
                detail: if both.is_empty() {
                    "b2 alone used its home account".to_owned()
                } else {
                    format!("a double grant: {}", both.join("; "))
                },
            }
        }
    });
    out
}

/// The report's accounts section.
#[must_use]
pub fn report(ev: &Evidence) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "- a model run on main's account claims, two Macs' table replicas and accounts state, a \
         minute a step on a simulated clock: the cut from +{} for {} (longer than the {}-minute \
         life of an unheard Mac's lease), a2 wanting the account from +{}",
        mins(CUT_AT),
        mins(CUT_FOR),
        PRESENCE_TTL_S / 60,
        mins(A2_WANTS_AT)
    );
    for s in &ev.scenarios {
        let changes: Vec<String> = s
            .users
            .iter()
            .map(|(t, u)| {
                format!(
                    "+{} {}",
                    mins(*t),
                    if u.is_empty() {
                        "nobody".to_owned()
                    } else {
                        u.join("+")
                    }
                )
            })
            .collect();
        let _ = writeln!(
            out,
            "- `{}`: users {}; b2 ends `{}`, the senior claim {}",
            s.name,
            changes.join(", "),
            s.b2_end,
            s.holder_end.as_deref().unwrap_or("none")
        );
    }
    out
}

/// One Mac's replica of the claims table: every row it holds.
struct Replica {
    me: &'static str,
    book: Mutex<Book>,
}

impl Replica {
    fn book(&self) -> Book {
        self.book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Take every row `from` holds, as the Commons sync does.
    fn merge(&self, from: &Self) {
        let rows = from.book();
        self.book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .merge(&rows);
    }
}

impl Table for Replica {
    fn read(&self, _: &std::path::Path) -> Result<Tabled, String> {
        Ok(Tabled {
            me: self.me.to_owned(),
            book: self.book(),
            writable: Ok(()),
        })
    }

    fn write(&self, _: &std::path::Path, row: &Row) -> Result<(), String> {
        self.book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(row.clone());
        Ok(())
    }
}

/// A Mac of the model: its replica, its accounts state, and when it last heard the other Mac.
struct Client {
    table: Replica,
    state: State,
    heard: BTreeMap<String, i64>,
    /// While a session here runs on the account: when its usage tap last claimed.
    tapped: Option<i64>,
    /// Its `[harness] accounts` mode.
    mode: Mode,
    /// The rows whose login is on this Mac.
    enrolled: BTreeSet<String>,
}

impl Client {
    fn new(label: &'static str, machine: &'static str, home: Option<&str>) -> Self {
        let mut state = State {
            swarm_label: Some(label.to_owned()),
            ..State::default()
        };
        if let Some(home) = home {
            state.home_accounts.insert(home.to_owned());
        }
        Self {
            table: Replica {
                me: machine,
                book: Mutex::new(Book::default()),
            },
            state,
            heard: BTreeMap::new(),
            tapped: None,
            mode: Mode::Early,
            enrolled: BTreeSet::new(),
        }
    }

    /// Exchange rows with `other` and hear it at `now`: the islands are joined.
    fn meet(&mut self, other: &mut Self, now: i64) {
        self.table.merge(&other.table);
        other.table.merge(&self.table);
        self.heard.insert(other.table.me.to_owned(), now);
        other.heard.insert(self.table.me.to_owned(), now);
    }

    fn touch(&mut self, act: Act<'_>, now: i64) -> Touched {
        let presence = Presence {
            at: now,
            heard: self.heard.clone(),
        };
        touch_with(
            &self.table,
            std::path::Path::new("/nonexistent/lab-home"),
            Some(presence),
            &mut self.state,
            Cli::Claude,
            &keys(Cli::Claude, &roster()),
            act,
            now,
        )
    }

    /// A launch on `row` (`Seat::claim`): read the claims; where nothing holds it elsewhere, the
    /// session starts and claims it. Whether it started.
    fn launch(&mut self, row: &str, now: i64) -> bool {
        self.touch(Act::Look, now);
        if self.state.lease_verdict(row, now).is_err() {
            return false;
        }
        match self.touch(Act::Claim(row), now) {
            Touched::HeldElsewhere(_) => return false,
            Touched::Unrecorded(_) if !self.state.home_accounts.contains(row) => return false,
            _ => {}
        }
        accounts::note_session(&mut self.state, &sid(row), row, None, now);
        self.tapped = Some(now);
        true
    }

    /// A minute of the session on `row`: its usage tap claims again once [`RECHECK_S`] has passed,
    /// and at its turn end it does what production's advice says: a move or a wait leaves `row`,
    /// anything else goes on. Whether it still runs on `row`.
    fn turn(&mut self, row: &str, now: i64) -> bool {
        let Some(tapped) = self.tapped else {
            return false;
        };
        accounts::note_session(&mut self.state, &sid(row), row, None, now);
        if now - tapped >= RECHECK_S {
            self.touch(Act::Claim(row), now);
            self.tapped = Some(now);
        }
        let me = Me {
            account: Some(row),
            left: None,
            default_home: true,
            stored_login: true,
        };
        let advice = accounts::advise(
            &roster(),
            &self.state,
            &self.enrolled,
            &me,
            &Point::TurnEnd,
            self.mode,
            now,
        );
        if matches!(advice, Advice::Move { .. } | Advice::Wait { .. }) {
            accounts::forget_session(&mut self.state, &sid(row));
            self.tapped = None;
            return false;
        }
        true
    }

    /// Use `row` this minute where it wants it: go on with its session, or launch one.
    fn uses(&mut self, row: &str, wants: bool, now: i64) -> bool {
        if self.tapped.is_some() {
            self.turn(row, now)
        } else {
            wants && self.launch(row, now)
        }
    }

    /// Whether its own row holds a live claim on `row` at `now`.
    fn claims(&self, row: &str, now: i64) -> bool {
        self.table
            .book()
            .row(self.table.me)
            .is_some_and(|mine| mine.until > now && mine.claim(&key(row)).is_some())
    }

    fn verdict(&self, row: &str, now: i64) -> String {
        match self.state.lease_verdict(row, now) {
            Ok(()) => "ok".to_owned(),
            Err(why) => format!("{why:?}"),
        }
    }

    /// The label of the senior live claim on `row` as this Mac reads it at `now`.
    fn holder(&self, row: &str, now: i64) -> Option<String> {
        let view = View {
            me: self.table.me.to_owned(),
            presence: Some(Presence {
                at: now,
                heard: self.heard.clone(),
            }),
        };
        self.table
            .book()
            .senior(&key(row), &view, now)
            .map(|holder| holder.label)
    }
}

fn sid(row: &str) -> String {
    format!("s-{row}")
}

fn email(row: &str) -> String {
    format!("{row}@lab.invalid")
}

/// The roster both Macs share: the shared account and b2's home account.
fn roster() -> Roster {
    let row = |label: &str| accounts::Row {
        label: label.to_owned(),
        class: Class::Shared,
        dir: Some(format!("/nonexistent/lab-home/{label}")),
        email: Some(email(label)),
        ..accounts::Row::default()
    };
    Roster {
        rows: vec![row("shared"), row("home-b")],
    }
}

fn key(row: &str) -> LeaseKey {
    LeaseKey::new("claude", &email(row))
        .unwrap_or_else(|e| unreachable!("a lab account's key: {e}"))
}

/// How a2 is set up: its `[harness] accounts` mode, and the rows its logins enroll.
#[derive(Clone, Debug)]
pub struct A2Setup {
    /// a2's rotation mode.
    pub mode: Mode,
    /// The rows enrolled on a2.
    pub enrolled: Vec<&'static str>,
}

impl Default for A2Setup {
    fn default() -> Self {
        Self {
            mode: Mode::Early,
            enrolled: Vec::new(),
        }
    }
}

/// Run one scenario on `row`, b2's home account where `home`.
fn scenario(name: &'static str, row: &str, home: bool) -> Scenario {
    scenario_with(name, row, home, &A2Setup::default())
}

/// [`scenario`] with a2 set up as `a2_setup` says.
#[must_use]
pub fn scenario_with(name: &'static str, row: &str, home: bool, a2_setup: &A2Setup) -> Scenario {
    let mut a2 = Client::new(A2, A2_MACHINE, None);
    a2.mode = a2_setup.mode;
    a2.enrolled = a2_setup.enrolled.iter().map(|l| (*l).to_owned()).collect();
    let mut b2 = Client::new(B2, B2_MACHINE, home.then_some(row));
    let end = CUT_AT + CUT_FOR + AFTER_S;
    let mut out = Scenario {
        name,
        ..Scenario::default()
    };
    let mut last: Option<Vec<String>> = None;
    let mut t = 0;
    while t <= end {
        if !(CUT_AT..CUT_AT + CUT_FOR).contains(&t) {
            a2.meet(&mut b2, t);
        }
        let b2_uses = b2.uses(row, true, t);
        let a2_uses = a2.uses(row, t >= A2_WANTS_AT, t);
        let mut users = Vec::new();
        if b2_uses {
            users.push("b2".to_owned());
        } else if t >= CUT_AT && out.b2_stopped.is_none() {
            out.b2_stopped = Some(t);
        }
        if a2_uses {
            users.push("a2".to_owned());
            out.a2_started.get_or_insert(t);
        }
        for (uses, mac, who) in [(b2_uses, &b2, "b2"), (a2_uses, &a2, "a2")] {
            if uses && !mac.claims(row, t) {
                out.unclaimed.push((t, who.to_owned()));
            }
        }
        if last.as_ref() != Some(&users) {
            out.users.push((t, users.clone()));
            last = Some(users);
        }
        t += TAP_S;
    }
    out.b2_end = b2.verdict(row, end);
    out.holder_end = b2.holder(row, end);
    out
}

/// Run both scenarios.
#[must_use]
pub fn run() -> Evidence {
    Evidence {
        scenarios: vec![
            scenario("lease", "shared", false),
            scenario("home", "home-b", true),
        ],
    }
}

#[cfg(test)]
#[path = "swarm_partition_accounts_tests.rs"]
mod tests;
