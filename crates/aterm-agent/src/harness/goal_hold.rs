// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ONE RECORD OF A CODEX GOAL ATERM PAUSED (the owner's decision of
//! 2026-09-28, answered to "A Codex in goal mode starts its next turn about
//! 10 ms after the last one ends, so it never gives aterm a pause to upgrade
//! in. How should aterm upgrade such a tab?": "Pause the goal briefly
//! (Recommended) — once the upgrade is due, aterm types `/goal pause` (or
//! presses Esc the instant a new goal turn starts, before it has done
//! anything), moves Codex onto the new build, then types `/goal resume`. The
//! goal carries on where it was; no running tool call is ever cut off.").
//!
//! Two of aterm's lanes pause a Codex goal: the LIVE UPGRADE, for the move
//! ([`super::upgrade_codex::goal_step`]), and the supervisor's SAVE-THEN-WAIT
//! SWITCH (`supervise::policy::turn_end`), while Codex runs on a cheaper
//! model. ONE FILE per tab says which of them holds that tab's goal paused,
//! and what it still owes: `<aterm state>/drive/<sid>.goal-hold.json`, beside
//! the tab's loop's ledger — the one place both reach (the upgrade by its
//! `Opts::aterm_state`, the loop by its ledger's path, [`path_beside`]). An
//! owner CLAIMS it before its key goes ([`claim`]: refused while the other
//! owner's hold is open), and every edge of its hold is written there BEFORE
//! the key that makes it: a host that restarts between the pause and the
//! resume reads what it owes and resumes the goal ONCE — never twice (a
//! resume already typed is `Resuming`, verified on the footer before it is
//! ever typed again), and never not at all. The switch's own ledger rows stay
//! its durable record (`approvals::open_wind_down`); its claim here is kept in
//! step with them by its loop, which is how the upgrade learns the goal is
//! the switch's ([`sync_switch`]), and how the switch learns it is the
//! upgrade's ([`held_by_upgrade`]).
//!
//! Nothing here reads a screen or types a key: the lanes decide, and this is
//! their shared memory.

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

/// The lane that paused the goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The live upgrade, for its move (`upgrade_codex::goal_step`).
    Upgrade,
    /// The save-then-wait switch, while the session waits on its own model.
    Switch,
}

impl Owner {
    /// The record's word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Owner::Upgrade => "upgrade",
            Owner::Switch => "switch",
        }
    }

    fn parse(word: &str) -> Option<Owner> {
        match word {
            "upgrade" => Some(Owner::Upgrade),
            "switch" => Some(Owner::Switch),
            _ => None,
        }
    }
}

/// How the pause was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// `/goal pause` typed: Codex takes it while a turn runs, and it stops
    /// the goal's NEXT turn, never the running one (the upgrade's evidence,
    /// `upgrade_codex` module header, "THE GOAL PAUSE").
    Typed,
    /// Esc, at a goal turn's HEAD — its rollout holds nothing of the turn's
    /// own work yet — which interrupts that turn and pauses the goal in the
    /// same instant (measured 2026-09-28 in the owner's rollout: the Esc of
    /// 15:01:13Z wrote `turn_aborted` and `thread_goal_updated … paused`).
    Esc,
}

impl How {
    fn word(self) -> &'static str {
        match self {
            How::Typed => "typed",
            How::Esc => "esc",
        }
    }
}

/// Where a hold stands. The first three OWE a resume ([`Stage::owes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The pause is made (written before its key), and has not been seen on
    /// the footer yet.
    Pausing,
    /// The pause is seen on the footer (`Goal paused (/goal resume)`): the
    /// goal is the owner's to resume, once.
    Paused,
    /// The resume is made (written before its key), and has not been seen on
    /// the footer yet: never made again until the footer says it did not
    /// land.
    Resuming,
    /// Resumed by its owner: nothing owed.
    Resumed,
    /// No longer the owner's to resume ([`Hold::why`]): a person resumed it
    /// or changed it, or the pause never took.
    Released,
}

impl Stage {
    /// Whether the hold still owes the goal its resume.
    #[must_use]
    pub fn owes(self) -> bool {
        matches!(self, Stage::Pausing | Stage::Paused | Stage::Resuming)
    }

    /// The record's word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Stage::Pausing => "pausing",
            Stage::Paused => "paused",
            Stage::Resuming => "resuming",
            Stage::Resumed => "resumed",
            Stage::Released => "released",
        }
    }

    fn parse(word: &str) -> Option<Stage> {
        Some(match word {
            "pausing" => Stage::Pausing,
            "paused" => Stage::Paused,
            "resuming" => Stage::Resuming,
            "resumed" => Stage::Resumed,
            "released" => Stage::Released,
            _ => return None,
        })
    }
}

/// ONE TAB'S HOLD of its Codex goal (module header).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    /// Whose it is.
    pub owner: Owner,
    /// Where it stands.
    pub stage: Stage,
    /// How the pause was made (the last way tried).
    pub how: How,
    /// The Codex TUI the pause went into; `0` where the owner did not name
    /// one (the switch's claim).
    pub pid: u32,
    /// When the pause was FIRST made (unix seconds).
    pub at: u64,
    /// When the latest way of pausing was tried ([`Hold::how`]): the typed
    /// pause, then the Esc.
    pub tried_at: u64,
    /// When the pause was first seen on the footer; `0`: not yet.
    pub took_at: u64,
    /// When the latest resume was made; `0`: none yet.
    pub resume_at: u64,
    /// How many resumes were made.
    pub resumes: u32,
    /// The build the move was for (the upgrade's), or empty.
    pub to: String,
    /// The Codex the upgrade RELAUNCHED for the move, once it found it
    /// (`upgrade_codex_drive`'s `resume_on_relaunch`); `0`: no relaunch yet.
    /// The one Codex whose paused-goal box the loop's approval policy may
    /// answer for this hold ([`box_is_upgrades`]): a box any other Codex in
    /// the tab draws — a person's own `codex resume` — is theirs.
    pub relaunched: u32,
    /// Why it ended, once it has: `moved` (resumed after the move),
    /// `abandoned:<why>` (resumed without it), `by-hand` (a person resumed
    /// it), `changed` (the goal is no longer paused and no longer pursued),
    /// `pause-refused` (the pause never showed), `switch` (the switch's hold
    /// ended with its switch).
    pub why: String,
    /// The notes already said about this hold, each once.
    pub said: Vec<String>,
}

impl Hold {
    /// A hold of `owner`'s, its pause made now (`at`) into `pid` by `how`,
    /// for the build `to`: [`Stage::Pausing`].
    #[must_use]
    pub fn pausing(owner: Owner, how: How, pid: u32, to: &str, at: u64) -> Hold {
        Hold {
            owner,
            stage: Stage::Pausing,
            how,
            pid,
            at,
            tried_at: at,
            took_at: 0,
            resume_at: 0,
            resumes: 0,
            to: to.to_string(),
            relaunched: 0,
            why: String::new(),
            said: Vec::new(),
        }
    }

    /// Whether this hold still owes the goal its resume ([`Stage::owes`]).
    #[must_use]
    pub fn owes(&self) -> bool {
        self.stage.owes()
    }

    /// Whether the note `what` was said already.
    #[must_use]
    pub fn said(&self, what: &str) -> bool {
        self.said.iter().any(|s| s == what)
    }

    /// Record the note `what` as said; `true` the first time.
    pub fn say(&mut self, what: &str) -> bool {
        if self.said(what) {
            return false;
        }
        self.said.push(what.to_string());
        true
    }

    /// When the hold last moved: its pause, its take or its resume.
    #[must_use]
    pub fn last_at(&self) -> u64 {
        self.at
            .max(self.tried_at)
            .max(self.took_at)
            .max(self.resume_at)
    }

    fn to_json(&self) -> String {
        let mut o = Map::new();
        for (k, v) in [
            ("hold", self.stage.word()),
            ("owner", self.owner.word()),
            ("how", self.how.word()),
            ("to", self.to.as_str()),
            ("why", self.why.as_str()),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        for (k, v) in [
            ("pid", u64::from(self.pid)),
            ("at", self.at),
            ("tried_at", self.tried_at),
            ("took_at", self.took_at),
            ("resume_at", self.resume_at),
            ("resumes", u64::from(self.resumes)),
            ("relaunched", u64::from(self.relaunched)),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        o.insert(
            "said".into(),
            Value::Array(self.said.iter().map(|s| Value::from(s.as_str())).collect()),
        );
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    fn from_json(text: &str) -> Option<Hold> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Some(Hold {
            stage: Stage::parse(&s("hold"))?,
            owner: Owner::parse(&s("owner"))?,
            how: if s("how") == "esc" {
                How::Esc
            } else {
                How::Typed
            },
            pid: u32::try_from(n("pid")).unwrap_or(0),
            at: n("at"),
            tried_at: n("tried_at").max(n("at")),
            took_at: n("took_at"),
            resume_at: n("resume_at"),
            resumes: u32::try_from(n("resumes")).unwrap_or(u32::MAX),
            to: s("to"),
            relaunched: u32::try_from(n("relaunched")).unwrap_or(0),
            why: s("why"),
            said: v
                .get("said")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// The record of tab `sid` under the aterm state root `root`: beside the
/// tab's loop's ledger (`approvals::ledger_under`), `<sid>.goal-hold.json`.
#[must_use]
pub fn path(root: &Path, sid: &str) -> PathBuf {
    path_beside(&crate::supervise::approvals::ledger_under(root, Some(sid)))
}

/// The record beside a loop's ledger `ledger` (`<dir>/<sid>.jsonl` →
/// `<dir>/<sid>.goal-hold.json`): how the loop, which knows its ledger's
/// path and not the root, reaches the same file as [`path`].
#[must_use]
pub fn path_beside(ledger: &Path) -> PathBuf {
    let stem = ledger
        .file_stem()
        .map_or_else(|| "self".into(), |s| s.to_string_lossy().into_owned());
    ledger.with_file_name(format!("{stem}.goal-hold.json"))
}

/// The hold recorded at `path`; `None` where none is, or the file is no
/// hold this reader was written for.
#[must_use]
pub fn read(path: &Path) -> Option<Hold> {
    Hold::from_json(&std::fs::read_to_string(path).ok()?)
}

/// Write `hold` at `path`, whole (a temporary file renamed over it).
///
/// # Errors
/// The directory or the file could not be written.
pub fn write(path: &Path, hold: &Hold) -> std::io::Result<()> {
    super::upgrade_models::write_atomic(path, &hold.to_json())
}

/// CLAIM the goal for `hold`'s owner, before its key goes: written unless an
/// OPEN hold of the OTHER owner stands there (`Err(that owner)`) — the one
/// rule that keeps the two lanes from both holding a goal.
///
/// # Errors
/// The other owner's hold is open (`Ok(Err(owner))`), or the record could not
/// be written (`Err`).
pub fn claim(path: &Path, hold: &Hold) -> std::io::Result<Result<(), Owner>> {
    if let Some(open) = read(path).filter(|h| h.owes() && h.owner != hold.owner) {
        return Ok(Err(open.owner));
    }
    write(path, hold).map(Ok)
}

/// Whether the UPGRADE holds the goal of the tab whose record is at `path`:
/// its hold still owes the goal its resume. What the switch reads before it
/// claims the goal for itself (the switch never pauses, stops or resumes a
/// goal the upgrade paused).
#[must_use]
pub fn held_by_upgrade(path: &Path) -> bool {
    read(path).is_some_and(|h| h.owner == Owner::Upgrade && h.owes())
}

/// Whether the paused-goal box on the tab whose record is at `path` is the
/// UPGRADE'S TO ANSWER — the one box the loop's approval policy presses for
/// it (`goal-resume@v1`): the upgrade's hold still owes the goal its resume,
/// it RELAUNCHED a Codex for the move ([`Hold::relaunched`]), that Codex
/// still leads its terminal (`leads`: alive, and its group its terminal's
/// foreground — the kernel's word), and its box was not left to a person at
/// the keys (`upgrade_codex::BOX_THEIRS`). Every other box — the one a
/// person's own `codex resume` draws in the tab (another Codex), the box of a
/// Codex restarted by hand over aterm's relaunch, a box a person was left —
/// is theirs (the review of 2026-09-28: armed by the hold alone, the policy
/// pressed `Resume goal` on any paused-goal box in the tab, a goal aterm
/// never paused included).
#[must_use]
pub fn box_is_upgrades(path: &Path, leads: impl Fn(u32) -> bool) -> bool {
    read(path).is_some_and(|h| {
        h.owner == Owner::Upgrade
            && h.owes()
            && h.relaunched != 0
            && !h.said(super::upgrade_codex::BOX_THEIRS)
            && leads(h.relaunched)
    })
}

/// THE SWITCH'S CLAIM KEPT IN STEP with its own state, at `now`: `paused` —
/// the switch paused the goal and owes it its resume at the reset
/// (`WindDown::goal_paused`) — writes its hold where none is open (the
/// upgrade's open hold is left as it stands, and `false` says the switch does
/// not hold the goal); not `paused` ends an open hold of the switch's
/// ([`Stage::Released`], `switch`). What it wrote, or `None` where it wrote
/// nothing (nothing changed, or the upgrade holds the goal).
pub fn sync_switch(path: &Path, paused: bool, how: How, now: u64) -> Option<Hold> {
    let open = read(path).filter(Hold::owes);
    let hold = match (paused, open) {
        (true, Some(h)) if h.owner == Owner::Upgrade => return None,
        (true, Some(_)) => return None,
        (true, None) => Hold {
            stage: Stage::Paused,
            took_at: now,
            ..Hold::pausing(Owner::Switch, how, 0, "", now)
        },
        (false, Some(h)) if h.owner == Owner::Switch => Hold {
            stage: Stage::Released,
            why: "switch".to_string(),
            ..h
        },
        (false, _) => return None,
    };
    write(path, &hold).ok()?;
    Some(hold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aterm-goal-hold-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch");
        d
    }

    /// The record reads back whole, and sits beside the tab's loop ledger —
    /// the one file the loop's path and the upgrade's root both name.
    /// NEGATIVE CONTROL: a file that is no hold (an upgrade state's shape)
    /// reads as none.
    #[test]
    fn a_hold_reads_back_whole_beside_the_loops_ledger() {
        let root = scratch("whole");
        let at = path(&root, "s-0123");
        assert_eq!(at, root.join("drive").join("s-0123.goal-hold.json"));
        assert_eq!(
            path_beside(&crate::supervise::approvals::ledger_under(
                &root,
                Some("s-0123")
            )),
            at
        );
        let mut h = Hold::pausing(Owner::Upgrade, How::Typed, 4242, "0.158.0", 1_000);
        h.took_at = 1_010;
        assert!(h.say("abandoned"));
        assert!(!h.say("abandoned"), "once");
        write(&at, &h).expect("write");
        assert_eq!(read(&at), Some(h));
        std::fs::write(&at, r#"{"phase":"pending","from":"0.157.1"}"#).expect("other");
        assert_eq!(read(&at), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ONE OWNER AT A TIME: the upgrade's claim stands while the switch's
    /// hold is open, and the switch's while the upgrade's is — each refused
    /// with the other's name; an ended hold is no obstacle. The switch's sync
    /// leaves the upgrade's open hold as it stands. NEGATIVE CONTROL: the
    /// same owner claims over its own open hold (a new way tried).
    #[test]
    fn one_owner_holds_a_goal_at_a_time() {
        let root = scratch("owner");
        let at = path(&root, "s-9");
        let up = Hold::pausing(Owner::Upgrade, How::Typed, 7, "0.158.0", 100);
        assert_eq!(claim(&at, &up).expect("io"), Ok(()));
        assert!(held_by_upgrade(&at));
        let sw = Hold::pausing(Owner::Switch, How::Esc, 0, "", 110);
        assert_eq!(claim(&at, &sw).expect("io"), Err(Owner::Upgrade));
        assert_eq!(sync_switch(&at, true, How::Esc, 110), None);
        assert_eq!(read(&at).map(|h| h.owner), Some(Owner::Upgrade));
        let again = Hold {
            how: How::Esc,
            ..up.clone()
        };
        assert_eq!(claim(&at, &again).expect("io"), Ok(()), "its own");
        write(
            &at,
            &Hold {
                stage: Stage::Resumed,
                why: "moved".into(),
                ..up
            },
        )
        .expect("write");
        assert!(!held_by_upgrade(&at));
        let claimed = sync_switch(&at, true, How::Typed, 200).expect("claimed");
        assert_eq!(
            (claimed.owner, claimed.stage),
            (Owner::Switch, Stage::Paused)
        );
        assert_eq!(claim(&at, &sw).expect("io"), Ok(()), "its own");
        let upgrade = Hold::pausing(Owner::Upgrade, How::Typed, 7, "0.158.0", 210);
        assert_eq!(claim(&at, &upgrade).expect("io"), Err(Owner::Switch));
        let ended = sync_switch(&at, false, How::Typed, 300).expect("ended");
        assert_eq!(
            (ended.stage, ended.why.as_str()),
            (Stage::Released, "switch")
        );
        assert_eq!(
            sync_switch(&at, false, How::Typed, 301),
            None,
            "nothing new"
        );
        assert_eq!(claim(&at, &upgrade).expect("io"), Ok(()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE BOX IS THE UPGRADE'S only while its hold owes the resume, names
    /// the Codex it relaunched, that Codex still leads its terminal, and the
    /// box was not left to a person (the review of 2026-09-28: the loop's
    /// `goal-resume@v1` was armed by the hold alone). NEGATIVE CONTROLS, each
    /// alone: no relaunch yet (a person's own `codex resume` before it),
    /// the relaunch no longer leading (a Codex the person restarted by hand
    /// in its place), the box left to a person (`BOX_THEIRS`), the hold no
    /// longer owed, the hold the switch's.
    #[test]
    fn the_box_is_the_upgrades_only_on_its_own_relaunch() {
        let root = scratch("box");
        let at = path(&root, "s-7");
        let leads = |pid: u32| pid == 5151;
        let paused = Hold {
            stage: Stage::Paused,
            took_at: 110,
            ..Hold::pausing(Owner::Upgrade, How::Typed, 4242, "0.158.0", 100)
        };
        write(&at, &paused).expect("write");
        assert!(!box_is_upgrades(&at, leads), "no relaunch yet");
        let relaunched = Hold {
            relaunched: 5151,
            ..paused.clone()
        };
        write(&at, &relaunched).expect("write");
        assert_eq!(read(&at), Some(relaunched.clone()), "reads back");
        assert!(box_is_upgrades(&at, leads));
        assert!(!box_is_upgrades(&at, |_| false), "no longer leading");
        let mut theirs = relaunched.clone();
        theirs.say(super::super::upgrade_codex::BOX_THEIRS);
        write(&at, &theirs).expect("write");
        assert!(!box_is_upgrades(&at, leads), "left to a person");
        write(
            &at,
            &Hold {
                stage: Stage::Released,
                why: "by-hand".into(),
                ..relaunched.clone()
            },
        )
        .expect("write");
        assert!(!box_is_upgrades(&at, leads), "owes nothing");
        write(
            &at,
            &Hold {
                owner: Owner::Switch,
                ..relaunched
            },
        )
        .expect("write");
        assert!(!box_is_upgrades(&at, leads), "the switch's");
        let _ = std::fs::remove_dir_all(&root);
    }
}
