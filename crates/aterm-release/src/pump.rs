// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release pump's decisions: `targo --unverified ship release --pump`.
//!
//! The pump is a loop over the release's existing verbs on a cut host. Each cycle it reads what
//! is true now ([`observe`]: origin/main, this clone's release journal, the repair record), says
//! one thing to do ([`decide`]) and does it ([`cycle`]): open the next version, start or adopt
//! its line, resume this clone's line, carry a repair worker's fix, publish a PREPARED release
//! where the checkout's `publish/config.sh` directs it, or wait. Nothing here runs a release:
//! the verbs are run by the [`World`], which the `aterm-release` binary implements over the
//! cutter and a test implements as a table.
//!
//! What it never does, by construction ([`Verb`] has no such arm): `--forget`, `--bump`,
//! `--supersede`, `--abandon`. It starts a line only for a version that has no fleet line, at
//! one frozen commit. A fleet line this clone holds no journal of is another holder's: the pump
//! waits and names that holder, and takes the line over (through the release's own selection,
//! which exits 4 beside a live holder) only when it was started with `--adopt`. A red is never
//! run again: only a commit its repair worker names and the line admits moves a red line, as a
//! hop. A verb's run that completes no row for longer than the fleet claim's own progress bound
//! while its processes do nothing is stalled: it is said, and a run that prepares is
//! interrupted so that its journal resumes it ([`attend`]).

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Seconds between two looks while nothing is to be done.
pub const POLL_SECS: u64 = 60;
/// The longest wait between two tries of a resumable stop (exit 3).
pub const RESUME_CAP_SECS: u64 = 30 * 60;
/// The wait between two tries beside another live holder of the line (exit 4): each try costs
/// a launch, and a holder is adoptable only once its progress has expired.
pub const HOLDER_SECS: u64 = 10 * 60;
/// The longest wait between two tries of a refusal no repair worker is on, once the journal or
/// main has changed.
pub const REFUSED_CAP_SECS: u64 = 60 * 60;
/// How long a repair worker's report may stay unreadable before its red line is said to be
/// stopped, a session's to read.
pub const UNREAD_STOP_SECS: u64 = 30 * 60;
/// The fewest seconds without a completed row before a run may be stalled: the fleet claim's
/// own floor for a holder's progress (`max(30 min, twice the step's recorded p95)`).
pub const STALL_FLOOR_SECS: u64 = 30 * 60;
/// How far main's tip may be past the commit that opened a version before a line of it that
/// has no fleet line starts at a newer commit: about two hours of main.
pub const STALE_BASE_COMMITS: u64 = 36;

/// `publish/config.sh`'s key that directs the pump to publish each PREPARED release itself.
pub const PUBLISH_KEY: &str = "PUMP_PUBLISH_DEFAULT";
/// `publish/config.sh`'s key that holds the Linux declaration of the lines the pump starts.
pub const LINUX_KEY: &str = "PUMP_LINUX_DEFAULT";
/// The pump's memory, in the clone's release directory beside its journal.
pub const MEMORY_FILE: &str = "pump.toml";

/// The value of `key` in a `publish/config.sh`: its flat `KEY="value"` line, as the publication
/// engine reads the file (never sourced, no double quote inside, nothing after the closing one).
#[must_use]
pub fn config_value<'c>(config: &'c str, key: &str) -> Option<&'c str> {
    config.lines().find_map(|line| {
        line.strip_prefix(key)?
            .strip_prefix("=\"")?
            .strip_suffix('"')
            .filter(|value| !value.contains('"'))
    })
}

/// A release's final line among what a run printed: its last line that is neither blank,
/// indented detail nor a `release-event:` record.
#[must_use]
pub fn final_line<'l>(lines: impl IntoIterator<Item = &'l str>) -> &'l str {
    lines
        .into_iter()
        .filter(|line| {
            !line.trim().is_empty()
                && !line.starts_with(char::is_whitespace)
                && !line.starts_with("release-event:")
        })
        .last()
        .unwrap_or("")
}

/// What origin/main says now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Main {
    /// Its tip, full.
    pub tip: String,
    /// The product version the tip declares.
    pub version: String,
    /// That version's tag is on origin: it is cut.
    pub published: bool,
    /// The tip holds that version's handoff fixtures, as the next cut's gate reads them.
    pub fixtures: bool,
    /// The commit of main that opened `version` (the bump), where main's history names one.
    pub opened: Option<String>,
    /// How many commits the tip is past `opened`.
    pub behind: u64,
    /// The head of the fleet line of `version`, where origin holds its claim.
    pub line: Option<String>,
    /// Who holds that line, as its claim names them.
    pub holder: Option<String>,
}

/// This clone's release journal, as far as the pump reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Journal {
    pub version: String,
    /// R, the head of its line.
    pub head: String,
    /// Everything is done for R.
    pub finished: bool,
    /// `--prepare` stopped PREPARED and the version is not cut.
    pub prepared: bool,
    /// Epoch seconds that prepare stopped.
    #[serde(default)]
    pub prepared_at: u64,
    /// The version's tag is on origin and this clone holds its cut claim: only its after-cut
    /// and check remain.
    pub published: bool,
    /// The version's tag is on origin and this clone claimed no cut of it: another clone cut
    /// the line, and this journal can only be forgotten.
    #[serde(default)]
    pub lost: bool,
}

/// What a repair worker's report says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// Still on it.
    Working,
    /// It ended naming this landed commit (`FIX <sha>`).
    Fix(String),
    /// It ended without a fix: its own reason (`NO-FIX <why>`), or that it named none.
    NoFix(String),
    /// Its state could not be read: why.
    Unread(String),
}

/// The repair worker a red of the target's inputs opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair {
    pub sid: String,
    /// R, the commit whose red opened it.
    pub commit: String,
    /// Epoch seconds that red stopped its cut.
    pub at: u64,
    pub report: Report,
    /// For [`Report::Fix`]: whether the line admits that commit as a hop, else why not. With no
    /// line here, whether a line that starts at main's tip is past it: the tip holds the fix
    /// and the red commit does not.
    pub admitted: Option<Result<(), String>>,
    /// For [`Report::Fix`] with no line here: main's tip holds the fix.
    pub landed: bool,
}

/// A repair worker as the repair record and its own report give it ([`World::repair`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub sid: String,
    /// R, the commit whose red opened it.
    pub commit: String,
    /// Epoch seconds that red stopped its cut.
    pub at: u64,
    pub report: Report,
}

/// One look at the world.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seen {
    pub now: u64,
    pub main: Main,
    pub journal: Option<Journal>,
    /// Another live release run of this clone: who.
    pub holder: Option<String>,
    /// The repair worker on the target's inputs ([`target`]).
    pub repair: Option<Repair>,
    /// The repair worker a red of opening the version after main's opened ([`World::opening`]).
    pub opening: Option<Opened>,
    /// `publish/config.sh` on main says `PUMP_PUBLISH_DEFAULT="1"`.
    pub publish: bool,
    /// The pump was started with `--adopt`: it may take over a fleet line another holder
    /// started.
    pub adopt: bool,
    /// The newest judged pass that descends from the unfinished line's head.
    pub green: Option<String>,
    /// The newest judged pass that holds the commit that opened main's version, read once main
    /// is more than [`STALE_BASE_COMMITS`] past it.
    pub base_green: Option<String>,
}

/// One release verb the pump runs. There is no arm for `--forget`, `--bump`, `--supersede` or
/// `--abandon`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verb {
    /// `--open-next`.
    OpenNext,
    /// `--prepare --commit <base>`: start the version's line at this frozen commit.
    Start(String),
    /// `--prepare`, with no journal here: the release's own selection adopts the fleet line.
    Adopt,
    /// `--prepare`: resume this clone's line through READY.
    Prepare,
    /// The bare run: publish, cut, after-cut and check of this clone's journal.
    Finish,
    /// `--prepare --fix <commit>`: carry the worker's fix onto this clone's line.
    Fix(String),
}

impl Verb {
    /// The `aterm-release` arguments that run it; `linux` is the cut's Linux declaration, which
    /// only a verb that can start a line carries (a journal keeps its own).
    #[must_use]
    pub fn args(&self, linux: &[String]) -> Vec<String> {
        let mut args = vec!["release".to_string()];
        let words: &[&str] = match self {
            Self::OpenNext => &["--open-next"],
            Self::Start(_) | Self::Adopt | Self::Prepare | Self::Fix(_) => &["--prepare"],
            Self::Finish => &[],
        };
        args.extend(words.iter().map(ToString::to_string));
        match self {
            Self::Start(base) => args.extend(["--commit".to_string(), base.clone()]),
            Self::Fix(fix) => args.extend(["--fix".to_string(), fix.clone()]),
            _ => {}
        }
        if matches!(self, Self::Start(_) | Self::Adopt) {
            args.extend(linux.iter().cloned());
        }
        args
    }

    /// [`Verb::args`] and the flags the pump was started with, each only where the verb takes
    /// it: `--open-next` writes main alone and takes no release credential.
    #[must_use]
    pub fn argv(&self, linux: &[String], handed: &Handed) -> Vec<String> {
        let mut args = self.args(linux);
        if let Some(engine) = &handed.engine {
            args.extend(["--engine".to_string(), engine.clone()]);
        }
        if let Some(credentials) = &handed.credentials
            && !matches!(self, Self::OpenNext)
        {
            args.extend(["--release-credentials".to_string(), credentials.clone()]);
        }
        if handed.verbose {
            args.push("--verbose".into());
        }
        args
    }
}

/// The flags the pump was started with that it hands on to the verbs it runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Handed {
    /// `--engine DIRECTORY`.
    pub engine: Option<String>,
    /// `--release-credentials PROFILE`.
    pub credentials: Option<String>,
    /// `--verbose`.
    pub verbose: bool,
}

/// How a verb's run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// 0 released, prepared or opened · 1 refused · 2 usage · 3 a resumable stop · 4 the line
    /// is cut elsewhere · 130 interrupted.
    pub code: i32,
    /// Its final line.
    pub last: String,
}

/// The last verb the pump ran, kept across restarts so a restarted pump repeats nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub verb: Verb,
    /// What it was run for ([`Wanted::key`]).
    pub key: String,
    pub code: i32,
    pub last: String,
    /// Epoch seconds it ended.
    pub at: u64,
    /// main's tip and this clone's journal as the run left them.
    pub tip: String,
    #[serde(default)]
    pub journal: Option<Journal>,
    /// How many runs for the same key ended with this code before it.
    #[serde(default)]
    pub repeats: u32,
}

/// A repair worker whose report could not be read, and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unread {
    pub sid: String,
    /// Epoch seconds of the first look in a row that could not read it.
    pub since: u64,
}

/// What the pump remembers between cycles.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default)]
    pub last: Option<Attempt>,
    /// The repair worker whose report every look since `since` failed to read.
    #[serde(default)]
    pub unread: Option<Unread>,
}

/// What one cycle does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run {
        verb: Verb,
        key: String,
        why: String,
    },
    Wait {
        why: String,
        secs: u64,
    },
}

/// What the world's state alone asks for, before the last attempt is weighed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wanted {
    Run {
        verb: Verb,
        key: String,
        why: String,
    },
    /// Nothing to run whatever was tried.
    Hold(String),
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// The inputs a repair worker would be on: this clone's unfinished line's version and head,
/// else the version main declares at whatever commit a start of it went red (`None`: no line
/// exists, so each start judges the commit it was given).
#[must_use]
pub fn target(main: &Main, journal: Option<&Journal>) -> (String, Option<String>) {
    match journal.filter(|j| !j.finished) {
        Some(j) => (j.version.clone(), Some(j.head.clone())),
        None => (main.version.clone(), None),
    }
}

/// The frozen commit a line of main's version starts at when the version has no fleet line:
/// the commit that opened the version while main is within [`STALE_BASE_COMMITS`] of it (the
/// cadence case: the line starts on the open-next commit at once); past that, the newest judged
/// pass that holds the opening commit, else the tip as it was read. Never "main", which moves.
fn base(seen: &Seen) -> String {
    let main = &seen.main;
    match &main.opened {
        Some(opened) if main.behind <= STALE_BASE_COMMITS => opened.clone(),
        _ => seen.base_green.clone().unwrap_or_else(|| main.tip.clone()),
    }
}

fn candidate(seen: &Seen) -> String {
    seen.green.as_ref().map_or_else(
        || {
            "no judged pass descends from the line's head, so there is no supersede candidate"
                .into()
        },
        |green| {
            format!(
                "supersede candidate, a session's decision: --commit {green} --supersede (the \
                 newest judged pass that descends from the line's head)"
            )
        },
    )
}

/// What a red line's repair worker lets the pump do.
fn repaired(
    seen: &Seen,
    memory: &Memory,
    repair: &Repair,
    line: &str,
    fix: impl Fn(&str) -> Wanted,
) -> Wanted {
    let stopped =
        |why: String| Wanted::Hold(format!("{line} is stopped: {why}; {}", candidate(seen)));
    match (&repair.report, &repair.admitted) {
        (Report::Working, _) => Wanted::Hold(format!(
            "{line} is red and repair worker {} is on it",
            repair.sid
        )),
        (Report::Fix(sha), Some(Ok(()))) => fix(sha),
        (Report::Fix(sha), Some(Err(why))) => stopped(format!(
            "repair worker {} named {}, which the line does not admit ({why})",
            repair.sid,
            short(sha)
        )),
        (Report::Fix(sha), None) => Wanted::Hold(format!(
            "{line} is red; whether the line admits {} is not read yet",
            short(sha)
        )),
        (Report::NoFix(why), _) => stopped(format!(
            "repair worker {} ended without a fix ({why})",
            repair.sid
        )),
        (Report::Unread(why), _) => {
            let unread = memory
                .unread
                .as_ref()
                .filter(|unread| unread.sid == repair.sid)
                .map_or(0, |unread| seen.now.saturating_sub(unread.since));
            if unread >= UNREAD_STOP_SECS {
                stopped(format!(
                    "repair worker {}'s report has not been readable for {} min ({why}); a \
                     session reads it (aterm ctl @{} result) and carries its fix with --fix",
                    repair.sid,
                    unread / 60,
                    repair.sid
                ))
            } else {
                Wanted::Hold(format!(
                    "{line} is red and repair worker {}'s report cannot be read ({why})",
                    repair.sid
                ))
            }
        }
    }
}

fn wanted(seen: &Seen, memory: &Memory) -> Wanted {
    let main = &seen.main;
    // §3.5 step 1. Opening the next version needs only main: this clone's unfinished after-cut
    // does not hold it.
    if main.published && main.fixtures {
        // A red of the opening has one worker, whatever tip each try was on; while it works
        // nothing is tried again.
        if let Some(opening) = &seen.opening
            && opening.report == Report::Working
        {
            return Wanted::Hold(format!(
                "opening the version after {} went red and repair worker {} is on it",
                main.version, opening.sid
            ));
        }
        return Wanted::Run {
            verb: Verb::OpenNext,
            key: format!("open-after {}", main.version),
            why: format!(
                "main declares {}, which is cut, and its tip {} holds its handoff fixtures",
                main.version,
                short(&main.tip)
            ),
        };
    }
    if let Some(j) = seen.journal.as_ref().filter(|j| !j.finished) {
        let line = format!("line v{} at {}", j.version, short(&j.head));
        if j.lost {
            return Wanted::Hold(format!(
                "v{} is cut (its tag is on origin) and this clone claimed no cut of it: another \
                 clone took and cut the line, so this journal at {} resumes nothing and no line \
                 starts here while it stands; a session ends it with ship release --forget",
                j.version,
                short(&j.head)
            ));
        }
        if j.published {
            return Wanted::Run {
                verb: Verb::Finish,
                key: format!("after-cut {} {}", j.version, j.head),
                why: format!(
                    "v{} is cut and this clone's journal owes its after-cut",
                    j.version
                ),
            };
        }
        // A red of this head governs before PREPARED does: the journal stays PREPARED through a
        // red of the publish, and a red is never run again. A worker opened before the prepare
        // stopped is of a red that prepare has since passed, unless the pump's own publish of
        // this head then ended refused: a red on inputs that already have a worker opens none,
        // so that older worker is the one on it.
        let publish = format!("publish {} {}", j.version, j.head);
        let publish_refused = memory
            .last
            .as_ref()
            .is_some_and(|last| last.key == publish && last.code == 1);
        let red = seen
            .repair
            .as_ref()
            .filter(|repair| !j.prepared || repair.at >= j.prepared_at || publish_refused);
        if let Some(repair) = red {
            return repaired(seen, memory, repair, &line, |sha| Wanted::Run {
                verb: Verb::Fix(sha.to_string()),
                key: format!("fix {} {} {sha}", j.version, j.head),
                why: format!(
                    "repair worker {} named {}, which the line admits",
                    repair.sid,
                    short(sha)
                ),
            });
        }
        if j.prepared {
            return if seen.publish {
                Wanted::Run {
                    verb: Verb::Finish,
                    key: publish,
                    why: format!(
                        "{line} is PREPARED and publish/config.sh says PUMP_PUBLISH_DEFAULT=\"1\""
                    ),
                }
            } else {
                Wanted::Hold(format!(
                    "{line} is PREPARED; publish/config.sh does not say \
                     PUMP_PUBLISH_DEFAULT=\"1\", so a session publishes it (the bare ship \
                     release)"
                ))
            };
        }
        return Wanted::Run {
            verb: Verb::Prepare,
            key: format!("prepare {} {}", j.version, j.head),
            why: format!("{line} is this clone's unfinished line"),
        };
    }
    if main.published {
        return Wanted::Hold(format!(
            "main declares {}, which is cut, and its tip {} does not hold its handoff fixtures: \
             the clone that cut it lands them",
            main.version,
            short(&main.tip)
        ));
    }
    if let Some(head) = &main.line {
        if !seen.adopt {
            return Wanted::Hold(format!(
                "v{} has a fleet line at {} held by {}, and this clone holds no journal of it: \
                 the pump starts no second line and takes over another holder's only when \
                 started with --adopt; it opens the next version once this one is cut",
                main.version,
                short(head),
                main.holder
                    .as_deref()
                    .unwrap_or("a holder its claim does not name")
            ));
        }
        return Wanted::Run {
            verb: Verb::Adopt,
            key: format!("adopt {}", main.version),
            why: format!(
                "v{} has a fleet line and no journal here, and the pump was started with \
                 --adopt: the release's selection adopts it or says who holds it",
                main.version
            ),
        };
    }
    // One key per version and per worker whose fix a start is past, never per tip: main moves
    // about 18 times an hour, and a refused start backs off across those tips.
    let start = |at: String, after: Option<&str>, why: String| Wanted::Run {
        verb: Verb::Start(at),
        key: match after {
            Some(sid) => format!("start {} after {sid}", main.version),
            None => format!("start {}", main.version),
        },
        why,
    };
    match &seen.repair {
        // No line exists to carry a fix onto: a red of a line's first commit is repaired on
        // main, and the line starts at a commit of main that holds the fix. The newest red of
        // the version governs, so a start that goes red again waits on its own worker.
        Some(repair) => {
            let line = format!("v{} at {}", main.version, short(&repair.commit));
            repaired(seen, memory, repair, &line, |sha| {
                if repair.landed {
                    start(
                        main.tip.clone(),
                        Some(&repair.sid),
                        format!(
                            "{line} was red before it had a line; repair worker {} named {}, \
                             which main's tip {} holds, so the line starts there",
                            repair.sid,
                            short(sha),
                            short(&main.tip)
                        ),
                    )
                } else {
                    Wanted::Hold(format!(
                        "{line} is red; repair worker {} named {}, which main's tip {} does not \
                         hold yet, so no line starts",
                        repair.sid,
                        short(sha),
                        short(&main.tip)
                    ))
                }
            })
        }
        None => start(
            base(seen),
            None,
            format!(
                "main declares {}, which no release has used, and it has no fleet line",
                main.version
            ),
        ),
    }
}

fn doubling(first: u64, repeats: u32, cap: u64) -> u64 {
    first.saturating_mul(1u64 << repeats.min(16)).min(cap)
}

/// What to do now. Pure: the same look and memory give the same decision.
#[must_use]
pub fn decide(seen: &Seen, memory: &Memory) -> Decision {
    if let Some(holder) = &seen.holder {
        return Decision::Wait {
            why: format!("another release run holds this clone ({holder})"),
            secs: POLL_SECS,
        };
    }
    let (verb, key, why) = match wanted(seen, memory) {
        Wanted::Hold(why) => {
            return Decision::Wait {
                why,
                secs: POLL_SECS,
            };
        }
        Wanted::Run { verb, key, why } => (verb, key, why),
    };
    let Some(last) = memory.last.as_ref().filter(|last| last.key == key) else {
        return Decision::Run { verb, key, why };
    };
    let since = seen.now.saturating_sub(last.at);
    let after = |secs: u64, what: String| {
        if since >= secs {
            Decision::Run {
                verb: verb.clone(),
                key: key.clone(),
                why: format!("{why}; {what}, {since} s ago"),
            }
        } else {
            Decision::Wait {
                why: format!("{what}; trying again in {} s", secs - since),
                secs: (secs - since).min(POLL_SECS),
            }
        }
    };
    match last.code {
        // It ended well and the world still asks for the same thing: what it changed is not
        // readable yet (a ref a fetch has not brought), so look again rather than run again.
        0 => Decision::Wait {
            why: format!(
                "the last run for this ended 0 ({}) and nothing it changed is read yet",
                last.last
            ),
            secs: POLL_SECS,
        },
        3 | 130 => after(
            doubling(POLL_SECS, last.repeats, RESUME_CAP_SECS),
            format!(
                "the last run could not run or was interrupted (exit {})",
                last.code
            ),
        ),
        4 => after(
            HOLDER_SECS,
            "another live holder cuts this line (exit 4); no second line is started".to_string(),
        ),
        _ => {
            let changed = last.tip != seen.main.tip || last.journal != seen.journal;
            if changed {
                after(
                    doubling(5 * POLL_SECS, last.repeats, REFUSED_CAP_SECS),
                    format!(
                        "the last run was refused with no repair worker on it (exit {}) and the \
                         journal or main has changed since",
                        last.code
                    ),
                )
            } else {
                Decision::Wait {
                    why: format!(
                        "stopped: the last run was refused with no repair worker on it (exit \
                         {}: {}); nothing starts until the journal or main changes",
                        last.code, last.last
                    ),
                    secs: POLL_SECS,
                }
            }
        }
    }
}

/// What the pump reads and does. The binary implements it over the cutter.
pub trait World {
    /// Epoch seconds.
    fn now(&mut self) -> u64;
    /// origin/main, refreshed.
    /// # Errors
    /// Origin did not answer, or its tip does not read.
    fn main(&mut self) -> Result<Main, String>;
    /// This clone's release journal.
    /// # Errors
    /// A journal that does not read.
    fn journal(&mut self) -> Result<Option<Journal>, String>;
    /// Another live release run of this clone.
    fn holder(&mut self) -> Option<String>;
    /// `PUMP_PUBLISH_DEFAULT="1"` in main's `publish/config.sh`.
    fn publishes(&mut self) -> bool;
    /// The pump was started with `--adopt`.
    fn adopts(&mut self) -> bool;
    /// The repair worker a red of opening the version after `from` opened, and what it
    /// reports. One worker per opened version, whatever tip each try was on.
    fn opening(&mut self, from: &str) -> Option<Opened>;
    /// Whether `tip`'s history holds `commit`.
    fn holds(&mut self, commit: &str, tip: &str) -> bool;
    /// The repair worker a red of `version` opened, and what it reports: the one for a red at
    /// exactly `commit`, or with none named, the newest of the version.
    fn repair(&mut self, version: &str, commit: Option<&str>) -> Option<Opened>;
    /// Whether this clone's line admits `fix` as a hop.
    /// # Errors
    /// Why the line refuses it.
    fn admits(&mut self, fix: &str) -> Result<(), String>;
    /// The newest judged pass that descends from `head`.
    fn green(&mut self, head: &str) -> Option<String>;
    /// Run one verb to its end, attended ([`attend`]).
    fn run(&mut self, verb: &Verb) -> Ran;
    /// Wait `secs`; false once the pump is asked to stop.
    fn wait(&mut self, secs: u64) -> bool;
    fn say(&mut self, line: &str);
    /// Keep `memory` for the next start.
    fn remember(&mut self, memory: &Memory);
}

/// One look ([`Seen`]): main and the journal first, then the repair worker of what they make
/// the target, and for a fix it names, whether the unfinished line admits it, or with no line
/// here, whether main's tip holds it.
/// # Errors
/// What could not be read.
pub fn observe(world: &mut dyn World) -> Result<Seen, String> {
    let main = world.main()?;
    let journal = world.journal()?;
    let unfinished = journal.as_ref().filter(|j| !j.finished);
    let (version, commit) = target(&main, journal.as_ref());
    let repair = world.repair(&version, commit.as_deref()).map(|opened| {
        let (admitted, landed) = match (&opened.report, unfinished) {
            (Report::Fix(fix), Some(_)) => (Some(world.admits(fix)), false),
            // With no line here the fix is not carried: the line starts at a commit of main
            // that holds it, which the commit that went red must not.
            (Report::Fix(fix), None) => (
                Some(if world.holds(fix, &opened.commit) {
                    Err("the commit that went red already holds it".to_string())
                } else {
                    Ok(())
                }),
                world.holds(fix, &main.tip),
            ),
            _ => (None, false),
        };
        Repair {
            sid: opened.sid,
            commit: opened.commit,
            at: opened.at,
            report: opened.report,
            admitted,
            landed,
        }
    });
    let opening = if main.published && main.fixtures {
        world.opening(&main.version)
    } else {
        None
    };
    let green = match unfinished {
        Some(j) if !j.published && !j.lost && repair.is_some() => world.green(&j.head),
        _ => None,
    };
    let base_green = match (&main.opened, unfinished) {
        (Some(opened), None)
            if !main.published && main.line.is_none() && main.behind > STALE_BASE_COMMITS =>
        {
            world.green(opened)
        }
        _ => None,
    };
    Ok(Seen {
        now: world.now(),
        holder: world.holder(),
        publish: world.publishes(),
        adopt: world.adopts(),
        main,
        journal,
        repair,
        opening,
        green,
        base_green,
    })
}

/// One cycle: look, decide, and either run the verb and remember how it ended, or say why
/// not. Returns the seconds to wait before the next cycle (0 after a run).
pub fn cycle(world: &mut dyn World, memory: &mut Memory, said: &mut String) -> u64 {
    let seen = match observe(world) {
        Ok(seen) => seen,
        Err(why) => {
            say_once(
                world,
                said,
                &format!("pump: cannot read the world ({why}); looking again"),
            );
            return POLL_SECS;
        }
    };
    // How long a worker's report has been unreadable is the pump's own memory of its looks.
    let unread = match &seen.repair {
        Some(Repair {
            sid,
            report: Report::Unread(_),
            ..
        }) => Some(Unread {
            sid: sid.clone(),
            since: memory
                .unread
                .as_ref()
                .filter(|unread| unread.sid == *sid)
                .map_or(seen.now, |unread| unread.since),
        }),
        _ => None,
    };
    if memory.unread != unread {
        memory.unread = unread;
        world.remember(memory);
    }
    match decide(&seen, memory) {
        Decision::Wait { why, secs } => {
            say_once(world, said, &format!("pump: waiting — {why}"));
            secs
        }
        Decision::Run { verb, key, why } => {
            said.clear();
            let mut line = format!("pump: {why}; running");
            for word in verb.args(&[]) {
                let _ = write!(line, " {word}");
            }
            world.say(&line);
            let ran = world.run(&verb);
            world.say(&format!("pump: exit {} — {}", ran.code, ran.last));
            let repeats = memory
                .last
                .as_ref()
                .filter(|last| last.key == key && last.code == ran.code)
                .map_or(0, |last| last.repeats + 1);
            // The run's own view of main and the journal is what a later change is measured
            // against; a read that fails keeps what was seen before it ran.
            let tip = world.main().map_or(seen.main.tip, |main| main.tip);
            let journal = world.journal().unwrap_or(seen.journal);
            memory.last = Some(Attempt {
                verb,
                key,
                code: ran.code,
                last: ran.last,
                at: world.now(),
                tip,
                journal,
                repeats,
            });
            world.remember(memory);
            0
        }
    }
}

/// A wait is said when its reason changes, not once a minute.
fn say_once(world: &mut dyn World, said: &mut String, line: &str) {
    if said != line {
        world.say(line);
        line.clone_into(said);
    }
}

/// The loop: cycles until the world's wait says to stop, or after `limit` cycles.
pub fn pump(world: &mut dyn World, mut memory: Memory, limit: Option<usize>) {
    let mut said = String::new();
    let mut cycles = 0usize;
    loop {
        let secs = cycle(world, &mut memory, &mut said);
        cycles += 1;
        if limit.is_some_and(|limit| cycles >= limit) || !world.wait(secs) {
            return;
        }
    }
}

/// The newest completed row of a live release run, as the holder of its fleet line records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Epoch seconds a row last completed, or the step began.
    pub at: u64,
    /// The step's recorded p95, 0 where its history has none.
    pub p95_s: u64,
    /// That row or step boundary, as the holder names it.
    pub boundary: String,
}

/// How long a run may complete no row before it is stalled: the fleet claim's own rule for a
/// holder's progress, `max(30 min, twice the step's recorded p95)`.
#[must_use]
pub fn stall_bound(p95_s: u64) -> u64 {
    STALL_FLOOR_SECS.max(p95_s.saturating_mul(2))
}

/// One verb's run while the pump attends it.
pub trait Running {
    /// Epoch seconds.
    fn now(&mut self) -> u64;
    /// Its exit code, once it ended.
    fn ended(&mut self) -> Option<i32>;
    /// The newest completed row of the run, where it holds a fleet line and records one.
    fn progress(&mut self) -> Option<Progress>;
    /// For how many seconds the run's process tree has made no progress (it and what it runs
    /// only kept their books: `aterm_toolchain::stall`); `None` where that is not read.
    fn idle(&mut self) -> Option<u64>;
    /// The pump itself was asked to stop.
    fn stop_asked(&mut self) -> bool;
    /// Ask the run to end at its next boundary (SIGTERM), from which its journal resumes.
    fn interrupt(&mut self);
    /// Wait a moment before the next look.
    fn pause(&mut self);
    fn say(&mut self, line: &str);
}

/// Attend a verb's run to its end: its exit code. A stop asked of the pump is passed on once.
///
/// A run is stalled on two readings together, neither alone: its newest completed row is older
/// than [`stall_bound`], and its process tree has made no progress for that long. A row can
/// legitimately take longer than the bound (a slow Mac's test stage completed none for 108
/// minutes of 0.199.0 while its compilers and tests ran), and an idle moment is nothing; a run
/// that completes no row while its processes do nothing for the whole bound is the wait that
/// never ends (the same line's twelve hours waiting for memory it could not be admitted).
///
/// A stall is said once per row. With `interrupts` the run is then interrupted, once per row,
/// so that it ends INTERRUPTED and the pump resumes its journal with backoff: what the steps
/// before it and its compilers produced is kept, so a resume is never further back. Without it
/// (the publish, whose waits on a person or on a notary are long by nature) it is only said.
/// A run that records no row (it holds no fleet line: `--open-next`, a refusal before the
/// claim) or whose tree is not read is only waited on.
pub fn attend(run: &mut dyn Running, interrupts: bool) -> i32 {
    let mut passed_on = false;
    let mut judged = None;
    loop {
        if let Some(code) = run.ended() {
            return code;
        }
        if !passed_on && run.stop_asked() {
            run.interrupt();
            passed_on = true;
        }
        if !passed_on
            && let Some(progress) = run.progress()
            && judged != Some(progress.at)
        {
            let quiet = run.now().saturating_sub(progress.at);
            let bound = stall_bound(progress.p95_s);
            if quiet > bound
                && let Some(idle) = run.idle()
                && idle >= bound
            {
                judged = Some(progress.at);
                run.say(&format!(
                    "pump: STALLED — the run has completed no row for {} min (last: {}; its \
                     bound is {} min) and its processes have made no progress for {} min; {}",
                    quiet / 60,
                    progress.boundary,
                    bound / 60,
                    idle / 60,
                    if interrupts {
                        "interrupting it, and its journal resumes it"
                    } else {
                        "a publish is not interrupted: a session looks"
                    }
                ));
                if interrupts {
                    run.interrupt();
                }
            }
        }
        run.pause();
    }
}

#[cfg(test)]
#[path = "pump_tests.rs"]
mod tests;
