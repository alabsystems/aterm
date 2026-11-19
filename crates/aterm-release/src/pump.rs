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
//! `--abandon`. It runs `--supersede` only as a line's one re-base ([`Verb::Rebase`]): where
//! this clone's line must re-qualify anyway (a run of it went red, or a repair worker's fix is
//! accepted), onto the newest judged pass that descends from its head ([`Seen::green`]) and
//! only where that pass can take the line ([`Seen::onto`]), once per line ([`Journal::rebased`],
//! [`Memory::rebased`]); every later red goes back to hops. A hop and a re-base cost the same
//! re-qualification, but a hop leaves out every fix landed after the base. It starts a line
//! only for a version that has no fleet line, at one frozen commit. A fleet line this clone
//! holds no journal of is another holder's: the pump
//! waits and names that holder, and takes the line over (through the release's own selection)
//! only when it was started with `--adopt`, and then only once the line's record says it can be
//! taken (the holder's lease has ended unrenewed, or its Mac proves its process dead:
//! [`Main::held`]) and no live pump on the holder's Mac holds the line between its runs
//! ([`HolderPump`]). A standby leaves opening a version and starting its line to the primary for
//! as long as a live primary's child may take to reach the claim ([`deferred`]). Nothing that
//! moves this clone's line (a fix carried, a re-base) runs beside another live holder of it, and a
//! line another pump took while this one was away (the claim's holder is no child of this pump) is
//! that pump's ([`taken`]). Between its runs the pump renews the lease of the claim its own child
//! holds ([`renewal`]), so a peer that cannot ask this Mac still reads the line as held while the
//! pump lives. A journal main's supersession retired is stranded: only a session's `--forget`
//! ends it ([`Journal::retired`]). A run that still ends beside a holder (exit 4) is not run again
//! while the record says the line is held. A red is never run again. The verbs the pump runs hand their
//! repair to their caller ([`caller_owned`]: the pump opens no session), and the pump holds the
//! red line's version and head ([`Red`]) until a session moves it; a commit a repair worker of
//! an older record names, and the line admits, is still carried as a hop. Opening the next
//! version waits out the pause main's `publish/config.sh` asks ([`EVERY_KEY`]). A verb's run
//! that completes no row for longer than the fleet claim's own progress bound while its
//! processes do nothing, or for the claim's ceiling whatever they do, is stalled: it is said, and
//! every run but a publish is interrupted so that its journal resumes it ([`attend`]). What the pump
//! does now, and since when, is kept in its memory ([`Doing`]) for `ship release status` to read,
//! and each cycle it publishes what it does for a fleet survey ([`Fact`], `pump=`), so a peer
//! tells a live pump from a dead or stalled one by the age of its last look at the world: a run
//! that has stalled no longer refreshes it, and neither does a cycle that cannot read the world.
//! A look whose read of the fleet's claim is older than a poll decides nothing ([`observe`]).

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Seconds between two looks while nothing is to be done.
pub const POLL_SECS: u64 = 60;
/// The longest wait between two tries of a resumable stop (exit 3).
pub const RESUME_CAP_SECS: u64 = 30 * 60;
/// The longest wait between two tries after a run ended beside another holder of the line
/// (exit 4) whose record no longer keeps a contender off: the run and the record disagreed, so
/// the tries back off. While the record says the line is held nothing is tried at all.
pub const HOLDER_SECS: u64 = 10 * 60;
/// The longest wait between two tries of a refusal no repair worker is on, once the journal or
/// main has changed: six hours. main moves about twice a minute, so "changed" holds at nearly
/// every look, and a write refused for a cause no landing clears (a damaged write journal, a
/// credential) would otherwise be tried every hour for good.
pub const REFUSED_CAP_SECS: u64 = 6 * 60 * 60;
/// How long a repair worker's report may stay unreadable before its red line is said to be
/// stopped, a session's to read.
pub const UNREAD_STOP_SECS: u64 = 30 * 60;
/// The fewest seconds without a completed row before a run may be stalled: the fleet claim's
/// own floor for a holder's progress (`max(30 min, twice the step's recorded p95)`).
pub const STALL_FLOOR_SECS: u64 = 30 * 60;
/// How far main's tip may be past the commit that opened a version before a line of it that
/// has no fleet line starts at a newer commit: about two hours of main.
pub const STALE_BASE_COMMITS: u64 = 36;
/// The oldest a live pump's fact may be ([`Fact`]): a pump writes it at every look, at most
/// [`POLL_SECS`] apart while it waits, and twice a minute while it attends a run, so a fact ten
/// looks old is of a pump that is dead or stalled.
pub const PUMP_STALE_SECS: u64 = 10 * POLL_SECS;
/// How long a pump whose process ended still holds the line its fact says it held: launchd brings
/// a pump that died back at once (`KeepAlive`), and `--pump --install` replaces it, so the line
/// stays that pump's while its successor starts, which says what it holds before its first look
/// ([`pump`]). Past this a dead pump is gone.
pub const RESTART_GRACE_SECS: u64 = 3 * POLL_SECS;
/// The longest a verb's run may complete no row, whatever its processes do: the fleet claim's own
/// bound past which a holder renews nothing (`PULSE_ALONE_S`, [`aterm_ctl::release_holder::MAX_LEASE_S`]),
/// so a tree that spins without finishing anything loses the line's claim and the pump's fact
/// alike ([`attend`]).
pub const RUN_ALONE_SECS: u64 = aterm_ctl::release_holder::MAX_LEASE_S;
/// How many of the release children it ran the pump remembers ([`Memory::ran`]): enough for every
/// child of one line, the resumes a backoff repeats included.
pub const RAN_KEPT: usize = 64;
/// How long a standby (`--adopt`) leaves opening a version and starting its line to the primary
/// ([`deferred`]): the floor bound the primary's own [`attend`] gives a child that records no row,
/// so a live primary's start has reached the claim (its commit gates, its frozen cutter's build,
/// its exec) or been judged stalled before the standby starts beside it.
pub const STANDBY_DEFER_SECS: u64 = STALL_FLOOR_SECS;
/// How close to its end the pump renews the lease of the claim its own child holds between its
/// runs ([`renewal`]): half the shortest lease, so a renewal that fails is tried at every look for
/// a quarter of an hour before the lease can lapse.
pub const RENEW_AHEAD_SECS: u64 = STALL_FLOOR_SECS / 2;

/// The environment variable the pump sets, to `1`, on the child that runs a line's re-base
/// ([`Verb::Rebase`]) and on no other: under it the release's `--supersede` takes the line's claim
/// only from a holder a contender could take it from, judged at the compare-and-swap itself, so a
/// holder that took the line after the pump looked keeps it and the re-base ends exit 4, unspent.
pub const REBASE_ENV: &str = "ATERM_RELEASE_PUMP_REBASE";

/// `publish/config.sh`'s key that directs the pump to publish each PREPARED release itself.
pub const PUBLISH_KEY: &str = "PUMP_PUBLISH_DEFAULT";
/// `publish/config.sh`'s key that holds the Linux declaration of the lines the pump starts.
pub const LINUX_KEY: &str = "PUMP_LINUX_DEFAULT";
/// `publish/config.sh`'s key that holds the pause between two versions: the pump runs no
/// `--open-next` sooner than this many seconds after the commit that opened the cut version.
pub const EVERY_KEY: &str = "PUMP_OPEN_EVERY_DEFAULT";
/// The pump's memory, in the clone's release directory beside its journal.
pub const MEMORY_FILE: &str = "pump.toml";

/// What main's `publish/config.sh` says of the pause between two versions ([`EVERY_KEY`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Every {
    /// It sets none: the next version opens once the last is cut and its fixtures are on main.
    #[default]
    Unset,
    /// No `--open-next` sooner than this many seconds after the commit that opened the cut
    /// version.
    Secs(u64),
    /// Its line does not read as a whole number of seconds: why. Nothing opens while it stands.
    Unread(String),
}

/// [`EVERY_KEY`] in `config`, read as the publication engine reads the file
/// ([`config_value`]). A line that names the key and does not read is [`Every::Unread`], never
/// [`Every::Unset`]: a typo must not take the pause away.
#[must_use]
pub fn open_every(config: &str) -> Every {
    match config_value(config, EVERY_KEY) {
        Some(value) => value.parse().map_or_else(
            |_| {
                Every::Unread(format!(
                    "{EVERY_KEY}=\"{value}\" is not a whole number of seconds"
                ))
            },
            Every::Secs,
        ),
        None if config
            .lines()
            .any(|line| line.starts_with(&format!("{EVERY_KEY}="))) =>
        {
            Every::Unread(format!(
                "its {EVERY_KEY} line is not one flat KEY=\"value\""
            ))
        }
        None => Every::Unset,
    }
}

/// The input hash a cutter's caller-owned repair line names (`repair::Opened::lines`): the run
/// ended at a failing gate or build of R's source, and its repair is its caller's. Every cutter
/// since 2ab8f243280 prints it where no pump asked it for a worker
/// ([`crate::repair::PUMP_ENV`]).
#[must_use]
pub fn caller_owned(line: &str) -> Option<&str> {
    let (hash, _) = line
        .strip_prefix("repair: caller-owned repair (input hash ")?
        .split_once(')')?;
    (!hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(hash)
}

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
    /// The commit of main that opened `version` (the bump), where main's history names one,
    /// cut or not.
    pub opened: Option<String>,
    /// When `opened` was committed, epoch seconds: what the pause before the next version is
    /// measured from ([`EVERY_KEY`]).
    pub opened_at: Option<u64>,
    /// How many commits the tip is past `opened`, while `version` is not cut.
    pub behind: u64,
    /// The head of the fleet line of `version`, where origin holds its claim.
    pub line: Option<String>,
    /// Who holds that line, as its claim names them.
    pub holder: Option<String>,
    /// That holder keeps a contender on this Mac off the line now: its lease is unexpired and
    /// its Mac does not prove its process dead. `None` once the line can be taken.
    pub held: Option<Held>,
    /// Epoch seconds that holder's lease runs until unless renewed, whether or not it still keeps
    /// a contender off: how long a standby waits on a holder whose pump it cannot read.
    pub lease_until: Option<u64>,
    /// That lease had run out when main was read, by the claim's own rule: a renewal older than a
    /// lease, or one so far ahead of this clock that no sane clock wrote it, whose `lease_until`
    /// is still that far ahead.
    pub lease_ended: bool,
    /// That holder is on another Mac than this one, by the fleet node its claim records.
    pub elsewhere: bool,
    /// That holder's process, as its claim records it: whether it is a child this clone's pump
    /// ran ([`Memory::ran`]) tells this pump's line from another's on the same Mac.
    pub holder_id: Option<Process>,
    /// main records a supersession of `version` (`Aterm-Line-Supersession`): read by the same
    /// scan of main's history that applies it to the line ([`Main::line`]), so it costs no read of
    /// its own.
    pub superseded: bool,
    /// Epoch seconds the line's claim was read, by this Mac's clock: what the age of a look is
    /// measured from ([`observe`]), since a look decides on that claim.
    pub read_at: u64,
}

/// Why a release started here would end beside the line's holder (exit 4), read from the line's
/// record alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// Epoch seconds the holder's lease runs until, unless it renews it first.
    pub until: u64,
    /// What a contender is told now: the line a run would end with.
    pub refusal: String,
}

impl Held {
    /// What a wait on this holder says once, beside its reason.
    fn note(&self, now: u64) -> String {
        format!(
            "its lease runs until {} unless it renews it, {} s from now; a run started now \
             would end: {}",
            self.until,
            self.until.saturating_sub(now),
            self.refusal
        )
    }
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
    /// The line has had its one re-base: main records a supersession of its version
    /// (`Aterm-Line-Supersession`), this pump's re-base or a session's, and every clone reads it
    /// there, a standby that adopts the line included. Every later red goes back to hops. Taken
    /// from [`Main::superseded`] where main declares the journal's version.
    #[serde(default)]
    pub rebased: bool,
    /// `candidate.toml` holds a supersession of this line whose write is not decided: only the
    /// same `--commit <onto> --supersede` decides it, and nothing else runs.
    #[serde(default)]
    pub undecided: Option<Undecided>,
    /// The head the fleet's claim records for this line where it is not R: a hop carried here whose
    /// record merge has not landed (a pending line) is claimed at the newest head of the line main
    /// retains, which is what the claim publishes, and a supersession left undecided at the head it
    /// retires until main records it. The pump says it holds the line at this head ([`Fact`]), so a
    /// standby matches it to the fleet's line while the hop or the supersession is decided. A hop
    /// recorded on main before the claim's next step boundary wrote it is claimed at the head the
    /// claim still records, read in the same look ([`World::journal`]).
    #[serde(default)]
    pub recorded: Option<String>,
    /// main records a supersession of this journal's version onto another base than its line's:
    /// another holder re-based the line (a standby that took it, a session), and every run of this
    /// journal is refused ("this journal's line was superseded"). The pump never runs `--forget`,
    /// which alone ends it, so the journal is stranded: nothing of it runs here and the line is not
    /// this clone's ([`owns`]).
    #[serde(default)]
    pub retired: bool,
}

impl Journal {
    /// The head the fleet's claim records for this line: [`Journal::recorded`], else R.
    #[must_use]
    pub fn claimed(&self) -> &str {
        self.recorded.as_deref().unwrap_or(&self.head)
    }

    /// The head a re-base of this line retires: the head of the supersession `candidate.toml` holds
    /// undecided, which a re-run of the same re-base decides, else R.
    #[must_use]
    pub fn retires(&self) -> &str {
        self.undecided
            .as_ref()
            .map_or(&self.head, |pending| &pending.from)
    }

    /// The line as the pump's fact names it: `v<version>@<the head the claim records>`.
    fn fact_line(&self) -> String {
        format!("v{}@{}", self.version, short(self.claimed()))
    }
}

/// A supersession `candidate.toml` holds undecided ([`Journal::undecided`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Undecided {
    /// The head of the fleet line it retires.
    pub from: String,
    /// The base it re-bases the line onto.
    pub onto: String,
}

/// What a line's holder's Mac answered of one release pump it runs (one `release_pump` row, one
/// per clone's pump), as a standby reads it before it adopts ([`World::holder_pump`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpRow {
    /// Its process runs, as that Mac's kernel judged it; `None` where it did not say.
    pub alive: Option<bool>,
    /// The line it reads: `v<version>@<head>`, else `v<version>`.
    pub line: String,
    /// What it does: the verb it runs ([`Verb::word`]), or `wait`.
    pub step: String,
    /// Its clone holds an unfinished journal of that line; `None` where the row does not say.
    pub own: Option<bool>,
    /// How long before the answer its last look was, as that Mac measured it.
    pub age: u64,
}

/// Whether a live pump holds a fleet line between its runs. The holder a claim names is the
/// release process the pump runs, which is not running between those runs, so the claim alone
/// frees the line at every pause of a live pump (a red it holds for a session, a backoff,
/// PREPARED); a standby judges the pump on the holder's Mac too ([`holder_pump`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderPump {
    /// A pump there holds the line's journal, its process is not proven dead, and its last look
    /// was this many seconds ago, within [`PUMP_STALE_SECS`]: the line is that pump's.
    Live { age: u64 },
    /// A pump there held the line's journal and its process ended; its last look was this many
    /// seconds ago, within [`RESTART_GRACE_SECS`]: it may be restarting, and the line is still
    /// that pump's.
    Restarting { age: u64 },
    /// No live pump there holds the line: why.
    Gone(String),
    /// Its Mac's answer says neither: why (it did not answer, or its aterm predates the fact).
    Unknown(String),
}

/// What `answer`, a line's holder's Mac's word on the pumps it runs, says of whether a live pump
/// holds that fleet line of `version` at `head` ([`HolderPump`]). A pump holds it only where its
/// fact says it holds the line's journal of exactly that head (one that runs a verb may be moving
/// the line, so any head of the version is its own until the run ends), its process is not
/// proven dead and its fact is no older than [`PUMP_STALE_SECS`]; a pump whose process ended
/// holding it is [`HolderPump::Restarting`] for [`RESTART_GRACE_SECS`]. A live, fresh, waiting
/// pump whose journal of the version is at another head is unknown, never gone: a fact written
/// without a read of the claim (a run's end, a start, a look that could not read origin) names its
/// journal's own head, which the claim may not record yet, and a stranded journal of a line another
/// holder moved is waited out by the lease and [`PUMP_STALE_SECS`] as an unreadable pump is. One
/// such pump holds the line whatever another pump there says (a `--once` of another clone, a dead
/// one): each clone's pump answers a row of its own. No answer, no row and a row that does not say
/// whether it holds the journal are unknown, never gone; a whole list (this Mac's own records)
/// with no such pump is gone.
#[must_use]
pub fn holder_pump(
    answer: Result<Option<Vec<PumpRow>>, String>,
    version: &str,
    head: &str,
) -> HolderPump {
    let rows = match answer {
        Err(why) => {
            return HolderPump::Unknown(format!("its Mac's answer could not be read ({why})"));
        }
        Ok(None) => {
            return HolderPump::Unknown(
                "its Mac's answer carries no pump fact: it runs no pump, or its aterm predates \
                 the fact"
                    .into(),
            );
        }
        Ok(Some(rows)) => rows,
    };
    let verdicts: Vec<HolderPump> = rows
        .iter()
        .map(|row| one_pump(row, version, head))
        .collect();
    let live = verdicts
        .iter()
        .filter_map(|verdict| match verdict {
            HolderPump::Live { age } => Some(*age),
            _ => None,
        })
        .min();
    if let Some(age) = live {
        return HolderPump::Live { age };
    }
    if let Some(restarting) = verdicts
        .iter()
        .find(|verdict| matches!(verdict, HolderPump::Restarting { .. }))
    {
        return restarting.clone();
    }
    if let Some(unknown) = verdicts
        .iter()
        .find(|verdict| matches!(verdict, HolderPump::Unknown(_)))
    {
        return unknown.clone();
    }
    let gone: Vec<String> = verdicts
        .into_iter()
        .filter_map(|verdict| match verdict {
            HolderPump::Gone(why) => Some(why),
            _ => None,
        })
        .collect();
    HolderPump::Gone(if gone.is_empty() {
        "no pump runs there".into()
    } else {
        gone.join("; ")
    })
}

/// What one pump's row says of the fleet line of `version` at `head` ([`holder_pump`]).
fn one_pump(row: &PumpRow, version: &str, head: &str) -> HolderPump {
    let fleet = format!("v{version}@{}", short(head));
    let of_version =
        row.line == format!("v{version}") || row.line.starts_with(&format!("v{version}@"));
    let of_line = row.line == fleet || (row.step != "wait" && of_version);
    match row.own {
        None => HolderPump::Unknown(
            "its pump's fact does not say whether it holds the line's journal".into(),
        ),
        Some(false) => HolderPump::Gone(format!(
            "the pump there holds no journal of v{version} (it reads {})",
            row.line
        )),
        Some(true) if !of_version => HolderPump::Gone(format!(
            "the pump there holds a journal of {}, not of the fleet line {fleet}",
            row.line
        )),
        Some(true) if row.alive == Some(false) && row.age > RESTART_GRACE_SECS => {
            HolderPump::Gone(format!(
                "the pump there is dead (its last look was {} s ago)",
                row.age
            ))
        }
        Some(true) if row.age > PUMP_STALE_SECS => {
            HolderPump::Gone(format!("the pump there has not looked for {} s", row.age))
        }
        Some(true) if !of_line => HolderPump::Unknown(format!(
            "the pump there holds a journal of {}, not of the fleet line {fleet}: its fact may \
             name a head the claim does not record yet, or a line another holder moved",
            row.line
        )),
        Some(true) if row.alive == Some(false) => HolderPump::Restarting { age: row.age },
        Some(true) => HolderPump::Live { age: row.age },
    }
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
    /// The pause `publish/config.sh` on main asks between two versions.
    pub every: Every,
    /// The pump was started with `--adopt`: it may take over a fleet line another holder
    /// started.
    pub adopt: bool,
    /// The newest judged pass that descends from the unfinished line's head, read where the line
    /// must re-qualify anyway (a repair worker is on its red, or the pump holds its red).
    pub green: Option<String>,
    /// Whether `green` can take the line in its one re-base, read only for a line that has not
    /// re-based: it declares the line's version, holds the fix a repair worker named, and carries
    /// commits past the head in the app's cone that no hop of the line carries (else a hop
    /// carries the same); else why not.
    pub onto: Option<Result<(), String>>,
    /// Whether a live pump on the fleet line's holder's Mac holds the line, read only where the
    /// record keeps no contender off and either a standby would otherwise adopt it (with
    /// `--adopt`, no journal here) or this clone's unfinished line is claimed by a holder that is
    /// no child of this pump (another pump took it while this one was away: [`taken`]).
    pub holder_pump: Option<HolderPump>,
    /// The newest judged pass that holds the commit that opened main's version, read once main
    /// is more than [`STALE_BASE_COMMITS`] past it.
    pub base_green: Option<String>,
    /// The fleet line's claim names a holder that is no child this clone's pump ran: on another
    /// Mac, or on this one another clone's run or a session's. Its holder's Mac is asked after its
    /// pumps ([`taken`]); a child of this pump's own is this pump's line.
    pub foreign: bool,
}

/// Why a line re-qualifies anyway: the one reason the pump re-bases it ([`Verb::Rebase`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Requalify {
    /// A repair worker named this landed fix: the line re-qualifies with it whether it is
    /// carried as a hop or a re-base holds it.
    Fix(String),
    /// A run of the line ended red: its caller-owned repair's input hash, or a repair worker's
    /// own word that it has no fix.
    Red(String),
}

impl Requalify {
    /// The reason as the pump says it.
    fn said(&self) -> String {
        match self {
            Self::Fix(sha) => format!("requalify: repair fix {} is accepted", short(sha)),
            Self::Red(why) => format!("requalify: the line went red ({why})"),
        }
    }
}

/// One release verb the pump runs. There is no arm for `--forget`, `--bump` or `--abandon`, and
/// `--supersede` is only [`Verb::Rebase`]'s.
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
    /// `--prepare --commit <onto> --supersede`: this clone's line re-bases, once, onto the newest
    /// judged pass that descends from its head, for the typed reason it must re-qualify anyway.
    Rebase { onto: String, requalify: Requalify },
}

impl Verb {
    /// The verb's one word, as a survey shows what the pump runs ([`Fact::step`]).
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::OpenNext => "open-next",
            Self::Start(_) => "start",
            Self::Adopt => "adopt",
            Self::Prepare => "prepare",
            Self::Finish => "finish",
            Self::Fix(_) => "fix",
            Self::Rebase { .. } => "rebase",
        }
    }

    /// The `aterm-release` arguments that run it; `linux` is the Linux declaration this run is
    /// handed ([`declaration`]), which every verb of a line takes and `--open-next` does not.
    #[must_use]
    pub fn args(&self, linux: &[String]) -> Vec<String> {
        let mut args = vec!["release".to_string()];
        let words: &[&str] = match self {
            Self::OpenNext => &["--open-next"],
            Self::Start(_) | Self::Adopt | Self::Prepare | Self::Fix(_) | Self::Rebase { .. } => {
                &["--prepare"]
            }
            Self::Finish => &[],
        };
        args.extend(words.iter().map(ToString::to_string));
        match self {
            Self::Start(base) => args.extend(["--commit".to_string(), base.clone()]),
            Self::Fix(fix) => args.extend(["--fix".to_string(), fix.clone()]),
            Self::Rebase { onto, .. } => args.extend([
                "--commit".to_string(),
                onto.clone(),
                "--supersede".to_string(),
            ]),
            _ => {}
        }
        if !matches!(self, Self::OpenNext) {
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

/// The Linux declaration `verb`'s run is handed, of the pump's own (`pump`: its flags, else
/// main's `PUMP_LINUX_DEFAULT`, else why it has none). A verb that starts or adopts a line
/// always carries it. A verb that resumes this clone's line carries it only where the journal
/// holds none (`journal_declares`): a journal's own declaration wins, and a flag would replace
/// it. A journal can hold none where the line was adopted inside a resume, whose run was handed
/// nothing to record (0.200.0, 2026-11-12: refused "the cut must declare Linux" each cycle).
/// A re-base is a resume in this: a supersession at the same version keeps the journal's own.
/// # Errors
/// The pump has no declaration for a run that starts, adopts or prepares a line that has none:
/// such a run can only be refused, after its first step's checks. The bare run of a journal
/// with none goes on without one, and the release says what it needs.
pub fn declaration(
    verb: &Verb,
    journal_declares: bool,
    pump: Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    match verb {
        Verb::OpenNext => Ok(Vec::new()),
        Verb::Start(_) | Verb::Adopt => pump,
        Verb::Prepare | Verb::Fix(_) | Verb::Rebase { .. } | Verb::Finish if journal_declares => {
            Ok(Vec::new())
        }
        Verb::Prepare | Verb::Fix(_) | Verb::Rebase { .. } => pump,
        Verb::Finish => Ok(pump.unwrap_or_default()),
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
    /// The input hash its caller-owned repair line named ([`caller_owned`]): the run ended at a
    /// red of R's source.
    pub red: Option<String>,
    /// The release process the run was, where its kernel identity was read: the holder its fleet
    /// claim names if it took the line (the release re-execs its frozen cutter in place).
    pub child: Option<Process>,
    /// A release process ran: false where the pump started none (the verb's run has no Linux
    /// declaration to be handed, or its child could not be spawned), which decided nothing.
    pub started: bool,
}

/// A process as a fleet claim records its holder: its pid, its Mac's boot and its start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    pub pid: u32,
    pub boot: String,
    pub start: u64,
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

impl Attempt {
    /// Whether this run already tried what `verb` under `key` would now do, with this clone's
    /// journal reading `journal`: it was run for the same thing, or it is the same verb, was
    /// refused, and left the journal exactly as it reads now. A refused resume that moved the
    /// journal's head (it adopted the fleet's line, 0.200.0) left another key behind it, and
    /// the same command on the journal it left can only be refused the same way. A run that
    /// ended any other way is of its own key alone: one that ended 0 and left work for the same
    /// verb (the publish, then the after-cut) is followed at once.
    #[must_use]
    pub fn tried(&self, verb: &Verb, key: &str, journal: Option<&Journal>) -> bool {
        let refused = !matches!(self.code, 0 | 3 | 4 | 130);
        self.key == key
            || (refused
                && self.verb == *verb
                && journal.is_some()
                && self.journal.as_ref() == journal)
    }
}

/// A repair worker whose report could not be read, and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unread {
    pub sid: String,
    /// Epoch seconds of the first look in a row that could not read it.
    pub since: u64,
}

/// A red the pump's run of a line ended at: the cutter handed its repair to its caller
/// ([`caller_owned`]), and no session is opened for it. R is frozen and a FAIL is never
/// re-rolled, so running the line again can only repeat the red (or its whole qualification):
/// the pump holds the version and head ([`Red::holds`]) until a session moves the line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Red {
    pub version: String,
    /// R: the head of the unfinished line the run left, else the commit a start named.
    #[serde(default)]
    pub head: Option<String>,
    /// The run left this clone a journal of the line, which a session's `--forget` ends, and
    /// the hold with it. A start that left none holds every start of the version.
    #[serde(default)]
    pub journaled: bool,
    /// The input hash the cutter named.
    pub hash: String,
    /// Epoch seconds the run ended.
    pub at: u64,
    /// Its final line.
    pub last: String,
}

impl Red {
    /// Whether it holds this clone's unfinished line `j`: the same version and head, and no
    /// prepare has passed since it (a session accepted or repaired it at that head). A session
    /// that carries a fix or supersedes the line moves the head, and the hold with it.
    #[must_use]
    pub fn holds(&self, j: &Journal) -> bool {
        self.version == j.version
            && self.head.as_deref() == Some(j.head.as_str())
            && (!j.prepared || self.at >= j.prepared_at)
    }
}

/// What the pump is doing now. It is kept in its memory each time it changes, so that a reader
/// that is not the pump (`ship release status`) says why it waits, since when, and whether
/// that pump still runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doing {
    /// The pump's process.
    pub pid: u32,
    /// That process's start, as `ps -o lstart=` reads it: a reader tells it from a later process
    /// given the same pid.
    pub started: String,
    /// What it waits for, or the verb it runs and why, as it said it.
    pub reason: String,
    /// Epoch seconds that reason began.
    pub since: u64,
}

/// The one re-base the pump ran of a line ([`Verb::Rebase`]), kept whatever it decided: one that
/// recorded its supersession is read from main too ([`Journal::rebased`]), and one refused before
/// it recorded anything still spends the line's re-base in this clone, so its next red here goes
/// back to hops rather than to a second try. That spend is this clone's memory alone: main holds
/// no record of it, so another clone that takes the line (a standby, once this pump is dead or
/// stalled) may try the line's re-base once itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebased {
    pub version: String,
    /// The head the line left.
    pub from: String,
    /// The judged pass it re-based onto.
    pub onto: String,
    pub requalify: Requalify,
    /// How the run ended, and when (epoch seconds).
    pub code: i32,
    pub at: u64,
}

/// What the pump remembers between cycles.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default)]
    pub last: Option<Attempt>,
    /// The repair worker whose report every look since `since` failed to read.
    #[serde(default)]
    pub unread: Option<Unread>,
    /// The newest red a run of a line ended at.
    #[serde(default)]
    pub red: Option<Red>,
    /// What it does now.
    #[serde(default)]
    pub doing: Option<Doing>,
    /// The newest line re-base it ran.
    #[serde(default)]
    pub rebased: Option<Rebased>,
    /// The fleet line whose holder's pump every look since `since` could not read.
    #[serde(default)]
    pub pump_unread: Option<PumpUnread>,
    /// What a standby wants to open or start, and since when ([`deferred`]).
    #[serde(default)]
    pub due: Option<Due>,
    /// The release children this clone's pump ran, newest last, at most [`RAN_KEPT`]: a fleet
    /// claim whose holder is none of them is another's ([`Seen::foreign`]).
    #[serde(default)]
    pub ran: Vec<Process>,
}

/// A fleet line whose holder's pump could not be read ([`HolderPump::Unknown`]) at every look
/// since `since`, epoch seconds, the looks at the pump's pace: one read that fails (a loaded Mac's
/// three-second survey, a moment's network) takes no line ([`unread_wait`]), and neither do two
/// with a sleep, a restart or an outage of this pump's own between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PumpUnread {
    /// The line, `v<version>@<head>` ([`pump_line`]).
    pub line: String,
    pub since: u64,
    /// Epoch seconds of the latest of those looks: a look more than [`UNREAD_GAP_SECS`] after it
    /// starts the count again.
    #[serde(default)]
    pub last: u64,
}

/// The longest two looks that count toward a holder's pump's unreadable time may be apart: two
/// poll intervals. Further apart, this pump was asleep, cut off or restarting, and looked at
/// nothing between them.
pub const UNREAD_GAP_SECS: u64 = 2 * POLL_SECS;

/// What a standby wants to run that a live primary runs at its first look ([`deferred`]): the key
/// of the open or the start, and the epoch seconds of the first look in a row that wanted it. The
/// looks count only in a row at the pump's pace, as [`PumpUnread`]'s do: a look more than
/// [`UNREAD_GAP_SECS`] after the last one, a cycle that could not read the world, and a pump's
/// start each begin the count again, so a standby back from a sleep or an outage it shared with
/// the primary waits the whole bound again while the primary, at its first look, runs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Due {
    pub key: String,
    pub since: u64,
    /// Epoch seconds of the latest of those looks.
    #[serde(default)]
    pub last: u64,
}

/// The fleet line `main` reads, as [`PumpUnread`] keys it.
fn pump_line(main: &Main) -> String {
    format!(
        "v{}@{}",
        main.version,
        main.line.as_deref().map_or("", short)
    )
}

/// Whether this clone's line `j` has had its one re-base: main records a supersession of its
/// version, or this pump ran one of it.
fn rebased(memory: &Memory, j: &Journal) -> bool {
    j.rebased
        || memory
            .rebased
            .as_ref()
            .is_some_and(|rebased| rebased.version == j.version)
}

/// What the pump publishes each cycle for a fleet survey (`pump=`, `aterm_ctl::release_pump`):
/// the line it reads and who holds it, what it does and why. A reader judges the pump by the age
/// of its newest `at`: a pump looks at least once a minute while it waits, and refreshes `at`
/// while a verb it runs is attended, so an old `at` is a dead or stalled pump.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fact {
    /// The fleet line's holder, as its claim names them; empty for none.
    pub holder: String,
    /// The line: `v<version>@<head>`, else `v<version>` where it has none.
    pub line: String,
    /// What the pump does: the verb it runs ([`Verb::word`]) or `wait`.
    pub step: String,
    /// This clone holds an unfinished journal of the line: the line is this pump's between its
    /// runs, while the release process its claim names is not running. A standby reads it before
    /// it adopts ([`HolderPump`]).
    pub own: bool,
    /// Epoch seconds of the pump's last look that read the world (this cycle's, where it read
    /// it), or of a run's end: a pump that can no longer read the world ages out like a dead one.
    pub at: u64,
    /// The cycle's decision, as the pump said it.
    pub decision: String,
}

impl Fact {
    /// The fact of one cycle that read `seen` and decided `step` for `decision`. The line is not
    /// this clone's while another pump holds it ([`taken`]). A journal's line is named at the head
    /// the fleet's claim records for it ([`Journal::claimed`]).
    fn of(seen: &Seen, step: &str, decision: &str) -> Self {
        let main = &seen.main;
        let line = match (seen.journal.as_ref().filter(|j| !j.finished), &main.line) {
            (Some(j), _) => j.fact_line(),
            (None, Some(head)) => format!("v{}@{}", main.version, short(head)),
            (None, None) => format!("v{}", main.version),
        };
        Self {
            holder: main.holder.clone().unwrap_or_default(),
            line,
            step: step.to_string(),
            own: owns(seen.journal.as_ref()) && !yields(seen),
            at: seen.now,
            decision: decision.to_string(),
        }
    }

    /// The fact of a cycle that runs `verb` for `decision`. A run that leaves this clone a journal
    /// of the line (a start, an adoption) holds the line from its start, as a resume does, so a
    /// standby that reads the pump while the run's child is not yet, or no longer, the claim's live
    /// holder takes nothing from it. A start names the commit its line starts at.
    fn of_run(seen: &Seen, verb: &Verb, decision: &str) -> Self {
        let mut fact = Self::of(seen, verb.word(), decision);
        match verb {
            Verb::Start(base) => {
                fact.line = format!("v{}@{}", seen.main.version, short(base));
                fact.own = true;
            }
            Verb::Adopt => fact.own = true,
            _ => {}
        }
        fact
    }

    /// The fact the pump publishes the moment `run`'s verb ended, before its reads of origin: the
    /// line and whether it is this clone's from the journal the run left here (`left`, read with
    /// no network), so the record says what the run left from the first second its child no
    /// longer holds the claim.
    fn left(run: &Self, left: Option<&Journal>, now: u64, decision: &str) -> Self {
        Self {
            holder: run.holder.clone(),
            line: left
                .filter(|j| !j.finished)
                .map_or_else(|| run.line.clone(), Journal::fact_line),
            step: "wait".into(),
            own: owns(left),
            at: now,
            decision: decision.to_string(),
        }
    }
}

/// Whether `journal` makes its line this clone's: unfinished, not of a version another clone cut,
/// and not retired by main's supersession of its version ([`Fact::own`]).
fn owns(journal: Option<&Journal>) -> bool {
    journal.is_some_and(|j| !j.finished && !j.lost && !j.retired)
}

/// Whether this clone's unfinished line is another pump's now ([`taken`]): its claim's holder is no
/// child of this pump ([`Seen::foreign`]), and a pump on the holder's Mac holds the line's journal,
/// live or restarting.
fn yields(seen: &Seen) -> bool {
    seen.foreign
        && seen
            .journal
            .as_ref()
            .is_some_and(|j| owns(Some(j)) && j.version == seen.main.version)
        && matches!(
            seen.holder_pump,
            Some(HolderPump::Live { .. } | HolderPump::Restarting { .. })
        )
}

impl Fact {
    /// The fact of a cycle that has not read the world, `why` (origin did not answer, or the pump
    /// has just started): the line and whether it is this clone's are read from the journal as
    /// this clone's own files hold it ([`World::journal_here`], no network), so a pump that holds
    /// a line keeps saying so while origin does not answer and while it starts, and a standby does
    /// not take that line from it meanwhile. Its time is `looked`, that of the pump's last look
    /// that read the world as its survey record holds it ([`World::looked`]), never now: a pump
    /// that keeps failing to read the world, or keeps dying before its first look, ages out after
    /// [`PUMP_STALE_SECS`] as one that stopped looking does.
    fn unread(journal: Option<&Journal>, looked: u64, why: &str) -> Self {
        Self {
            line: journal
                .filter(|j| !j.finished)
                .map(Journal::fact_line)
                .unwrap_or_default(),
            step: "wait".into(),
            own: owns(journal),
            at: looked,
            decision: why.to_string(),
            ..Self::default()
        }
    }
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
        /// The reason, said when it changes.
        why: String,
        /// What goes with the reason where it is said and is no reason to say it again: a
        /// countdown, a lease's end. Empty for none.
        note: String,
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
    /// Nothing to run whatever was tried, with what is said beside the reason
    /// ([`Decision::Wait`]'s note).
    Held(String, String),
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

/// What a line the pump holds says of a supersession onto the newest judged pass past its head:
/// a session's decision once the line has had its re-base (`rebased`) or where that pass cannot
/// take it, and the pump's own only once and only at a re-qualify ([`rebase`]).
fn candidate(seen: &Seen, rebased: bool) -> String {
    match (&seen.green, &seen.onto) {
        (None, _) => "no judged pass descends from the line's head, so there is no re-base or \
                      supersede candidate"
            .into(),
        (Some(green), _) if rebased => format!(
            "the line has had its one re-base, so every later red goes back to hops; a \
             supersession onto {green} (the newest judged pass that descends from its head) is \
             a session's decision: --commit {green} --supersede"
        ),
        (Some(green), Some(Err(why))) => format!(
            "the newest judged pass that descends from the line's head, {green}, cannot take a \
             re-base ({why}); a supersession onto it is a session's decision: --commit {green} \
             --supersede"
        ),
        (Some(green), _) => format!(
            "supersede candidate, a session's decision: --commit {green} --supersede (the \
             newest judged pass that descends from the line's head)"
        ),
    }
}

/// Another process holds this clone's line `j` live: its fleet claim keeps a contender off now,
/// and it is no run of this clone ([`decide`] has already waited on one of those). Nothing that
/// moves the line runs beside it: a hop's run would only end beside it (exit 4), and so would the
/// pump's re-base, whose `--supersede` admits the holder as a contender does ([`REBASE_ENV`]);
/// only a session's `--supersede` takes a line whatever its holder does. Its wait names the
/// holder, with its lease's end.
fn beside(seen: &Seen, j: &Journal, line: &str) -> Option<Wanted> {
    let main = &seen.main;
    let held = main.held.as_ref().filter(|_| main.version == j.version)?;
    Some(Wanted::Held(
        format!(
            "{line} is held now by {}, a process that is no run of this clone: neither a fix nor \
             a re-base runs beside a live holder, and the pump looks again once its lease ends \
             unrenewed or its Mac proves its process dead",
            main.holder
                .as_deref()
                .unwrap_or("a holder its claim does not name")
        ),
        held.note(seen.now),
    ))
}

/// Whether a holder's pump that cannot be read is still waited on, and what goes with that wait:
/// until the holder's lease has ended by the claim's own rule ([`Main::lease_ended`]) and that
/// pump has been unreadable at every look for [`PUMP_STALE_SECS`] ([`Memory::pump_unread`]), what
/// a readable pump's fact may age before it is stale. One read that fails (a loaded Mac's
/// three-second survey, a moment's network) takes no line from a pump that holds it.
fn unread_wait(seen: &Seen, memory: &Memory) -> Option<String> {
    let main = &seen.main;
    if let Some(until) = main
        .lease_until
        .filter(|until| !main.lease_ended && seen.now <= *until)
    {
        return Some(format!(
            "its lease runs until {until}, {} s from now",
            until - seen.now
        ));
    }
    let line = pump_line(main);
    let unread = memory
        .pump_unread
        .as_ref()
        .filter(|unread| unread.line == line)
        .map_or(0, |unread| seen.now.saturating_sub(unread.since));
    (unread < PUMP_STALE_SECS).then(|| {
        format!(
            "its lease has ended; its pump has been unreadable for {unread} s of the \
             {PUMP_STALE_SECS} s that free the line"
        )
    })
}

/// Another pump holds this clone's unfinished line `j` now: the claim's holder is no child this
/// pump ran ([`Seen::foreign`]: a standby on another Mac, or another clone's pump on this one,
/// took the line while this pump was asleep, cut off or dead), the record keeps no contender off,
/// and a pump on the holder's Mac holds the line's journal ([`HolderPump`]; this Mac's records are
/// read with this pump's own left out). The claim's holder alone tells the two pumps apart. The
/// line is that pump's: nothing of it runs here (no prepare, fix, re-base, supersession or
/// publish), which would take it back at each pause of that pump's runs, and this pump's fact
/// says the line is not its own ([`yields`]). A pump there that cannot be read is waited out as
/// a standby waits one out ([`unread_wait`]). Once that pump is gone this clone's journal resumes
/// only where the fleet line still resumes from it: a line that pump re-based retired this journal
/// ([`Journal::retired`]), and one it hopped past an undecided supersession of this clone's holds
/// it ([`undecided`]); either is a session's to end, never this pump's.
fn taken(seen: &Seen, memory: &Memory, j: &Journal, line: &str) -> Option<Wanted> {
    let main = &seen.main;
    if !seen.foreign || main.version != j.version {
        return None;
    }
    let holder = main
        .holder
        .as_deref()
        .unwrap_or("a holder its claim does not name");
    let taken = format!(
        "{line} was taken by {holder}, no run of this pump, whose release process is not running"
    );
    match seen.holder_pump.as_ref()? {
        HolderPump::Live { age } => Some(Wanted::Held(
            format!(
                "{taken}, and the pump there holds the line's journal and is live: the line is \
                 that pump's, and this clone runs nothing of it while that pump holds it; a \
                 session ends this journal with ship release --forget"
            ),
            format!("its last look was {age} s ago"),
        )),
        HolderPump::Restarting { age } => Some(Wanted::Held(
            format!(
                "{taken}, and the pump there held the line's journal when its process ended: it \
                 may be restarting, so this clone runs nothing of the line for \
                 {RESTART_GRACE_SECS} s after its last look"
            ),
            format!("its last look was {age} s ago"),
        )),
        HolderPump::Unknown(why) => unread_wait(seen, memory).map(|note| {
            Wanted::Held(
                format!(
                    "{taken}, and whether the pump there holds the line cannot be read ({why}): \
                     this clone runs nothing of it until that holder's lease ends unrenewed and \
                     its pump has been unreadable for {PUMP_STALE_SECS} s"
                ),
                note,
            )
        }),
        HolderPump::Gone(_) => None,
    }
}

/// The re-base of this clone's line `j` for `requalify`, where it is due: the line has not
/// re-based, the newest judged pass that descends from its head can take it ([`Seen::onto`]),
/// and the fleet line is this journal's, at its head. Beside another live holder of the line it
/// is a wait that names that holder ([`beside`]); a fleet line another holder has moved since is
/// not this clone's to retire, and no re-base is due.
fn rebase(seen: &Seen, memory: &Memory, j: &Journal, requalify: Requalify) -> Option<Wanted> {
    if rebased(memory, j) {
        return None;
    }
    let green = seen.green.as_ref()?;
    seen.onto.as_ref()?.as_ref().ok()?;
    if let Some(hold) = beside(
        seen,
        j,
        &format!("line v{} at {}", j.version, short(&j.head)),
    ) {
        return Some(hold);
    }
    if seen.main.version != j.version || seen.main.line.as_deref() != Some(j.head.as_str()) {
        return None;
    }
    Some(Wanted::Run {
        why: format!(
            "{}: line v{} at {} must re-qualify anyway, so it re-bases once onto {}, the newest \
             judged pass that descends from its head, which holds every fix landed since its \
             base; every later red goes back to hops",
            requalify.said(),
            j.version,
            short(&j.head),
            short(green)
        ),
        verb: Verb::Rebase {
            onto: green.clone(),
            requalify,
        },
        key: format!("rebase {} {}", j.version, j.head),
    })
}

/// A supersession of this clone's line that `candidate.toml` holds undecided. It is the pump's
/// own only where its last run was a re-base onto the same commit that left it: the journal that
/// run left held this very supersession undecided, or that run was the re-base of the head the
/// supersession retires and ended without deciding anything (exit 3, a write whose result is
/// unknown, which the read of the journal after it may miss in the same outage; or 130). The same
/// command decides it. Any other is a session's, a session's run of the command the pump named
/// after its own re-base was spent (refused) included. Nothing runs
/// beside another live holder of the line ([`beside`]), nor once the fleet line is at neither the
/// head it retires nor its replacement (another holder moved it).
fn undecided(seen: &Seen, memory: &Memory, j: &Journal, pending: &Undecided) -> Wanted {
    let onto = pending.onto.as_str();
    let line = format!("line v{} at {}", j.version, short(&j.head));
    if let Some(hold) = beside(seen, j, &line) {
        return hold;
    }
    let main = &seen.main;
    if main.version != j.version
        || !main
            .line
            .as_deref()
            .is_some_and(|head| head == pending.from || head == onto)
    {
        return Wanted::Hold(format!(
            "candidate.toml holds an undecided supersession of line v{} from {} onto {}, and the \
             fleet line is now at {}: another holder moved it, so a session decides the \
             supersession (ship release --commit {onto} --supersede) or forgets it (--forget)",
            j.version,
            short(&pending.from),
            short(onto),
            main.line.as_deref().map_or("none", short)
        ));
    }
    match memory.last.as_ref().filter(|last| {
        matches!(&last.verb, Verb::Rebase { onto: o, .. } if o == onto)
            && (last
                .journal
                .as_ref()
                .and_then(|left| left.undecided.as_ref())
                == Some(pending)
                || (matches!(last.code, 3 | 130)
                    && last.key == format!("rebase {} {}", j.version, pending.from)))
    }) {
        Some(last) => Wanted::Run {
            verb: last.verb.clone(),
            key: last.key.clone(),
            why: format!(
                "line v{}'s re-base onto {} stopped before its supersession was decided \
                 (candidate.toml holds it): the same command decides it",
                j.version,
                short(onto)
            ),
        },
        None => Wanted::Hold(format!(
            "candidate.toml holds an undecided supersession of line v{} onto {}, which is a \
             session's to decide: ship release --commit {onto} --supersede",
            j.version,
            short(onto)
        )),
    }
}

/// What a red line's repair worker lets the pump do. A worker that stops the line hands it to
/// `rebase` first: the line's one re-base, where it is due.
fn repaired(
    seen: &Seen,
    memory: &Memory,
    repair: &Repair,
    (line, rebased): (&str, bool),
    fix: impl Fn(&str) -> Wanted,
    rebase: impl Fn(Requalify) -> Option<Wanted>,
) -> Wanted {
    let stopped = |requalify: Requalify, why: String| {
        rebase(requalify).unwrap_or_else(|| {
            Wanted::Hold(format!(
                "{line} is stopped: {why}; {}",
                candidate(seen, rebased)
            ))
        })
    };
    match (&repair.report, &repair.admitted) {
        (Report::Working, _) => Wanted::Hold(format!(
            "{line} is red and repair worker {} is on it",
            repair.sid
        )),
        (Report::Fix(sha), Some(Ok(()))) => fix(sha),
        (Report::Fix(sha), Some(Err(why))) => stopped(
            Requalify::Fix(sha.clone()),
            format!(
                "repair worker {} named {}, which the line does not admit ({why})",
                repair.sid,
                short(sha)
            ),
        ),
        (Report::Fix(sha), None) => Wanted::Hold(format!(
            "{line} is red; whether the line admits {} is not read yet",
            short(sha)
        )),
        (Report::NoFix(why), _) => stopped(
            Requalify::Red(format!("repair worker {} has no fix", repair.sid)),
            format!("repair worker {} ended without a fix ({why})", repair.sid),
        ),
        (Report::Unread(why), _) => {
            let unread = memory
                .unread
                .as_ref()
                .filter(|unread| unread.sid == repair.sid)
                .map_or(0, |unread| seen.now.saturating_sub(unread.since));
            if unread >= UNREAD_STOP_SECS {
                stopped(
                    Requalify::Red(format!("repair worker {}'s report is unread", repair.sid)),
                    format!(
                        "repair worker {}'s report has not been readable for {} min ({why}); a \
                         session reads it (aterm ctl @{} result) and carries its fix with --fix",
                        repair.sid,
                        unread / 60,
                        repair.sid
                    ),
                )
            } else {
                Wanted::Hold(format!(
                    "{line} is red and repair worker {}'s report cannot be read ({why})",
                    repair.sid
                ))
            }
        }
    }
}

/// A journal of a version another clone cut holds this clone: it resumes nothing, and nothing
/// starts or opens here while it stands.
fn lost(j: &Journal) -> Wanted {
    Wanted::Hold(format!(
        "v{} is cut (its tag is on origin) and this clone claimed no cut of it: another clone \
         took and cut the line, so this journal at {} resumes nothing and no line starts or \
         opens here while it stands; a session ends it with ship release --forget",
        j.version,
        short(&j.head)
    ))
}

/// A journal main's supersession retired holds this clone ([`Journal::retired`]): every run of it
/// is refused, and only a session's `--forget` ends it, which the pump never runs. It is not this
/// clone's line, and the pump does not take the fleet line that replaced it: a pump takes another
/// holder's line only with `--adopt`, and then only with no journal here.
fn stranded(j: &Journal) -> Wanted {
    Wanted::Hold(format!(
        "line v{} at {} was superseded: main records a supersession of v{} onto another base \
         (another holder re-based the line), so every run of this journal is refused; the pump \
         never runs --forget, so the journal is stranded here: a session ends it with ship \
         release --forget, and a pump started with --adopt then takes the fleet line over once \
         its holder is gone",
        j.version,
        short(&j.head),
        j.version
    ))
}

/// The release child of this pump's own whose fleet claim of this clone's line is due a renewal
/// between the pump's runs ([`World::renew`]): the claim's holder is a child this pump ran
/// ([`Memory::ran`]), no other release run of this clone is live, the line is this clone's (an
/// unfinished journal of main's version that is neither cut, lost nor retired), and its lease runs
/// out within [`RENEW_AHEAD_SECS`] (or has run out). The claim names a release process, which is
/// not running between the pump's runs, and a reader that cannot ask its Mac (aterm not running
/// there, the LAN cut off) judges the line by its lease alone: renewed by the pump that holds the
/// line, the lease lapses only once that pump has died, slept or lost origin, never at every
/// pause of a live one.
fn renewal<'s>(seen: &'s Seen, memory: &Memory) -> Option<&'s Process> {
    let main = &seen.main;
    let holder = main
        .holder_id
        .as_ref()
        .filter(|holder| memory.ran.contains(holder))?;
    let mine = seen
        .journal
        .as_ref()
        .is_some_and(|j| owns(Some(j)) && !j.published && j.version == main.version);
    let due = main
        .lease_until
        .is_some_and(|until| until.saturating_sub(seen.now) <= RENEW_AHEAD_SECS);
    (mine && due && !main.published && seen.holder.is_none()).then_some(holder)
}

/// The pause main asks before the next version opens ([`EVERY_KEY`]), measured from the commit
/// that opened the cut version: a hold until it has passed, or while the key does not read.
/// Where main's history names no opening commit the pause cannot be measured, and the version
/// opens: the next version's own opening commit is the pump's, and is measured.
fn paused(seen: &Seen) -> Option<Wanted> {
    let main = &seen.main;
    match &seen.every {
        Every::Unset => None,
        Every::Unread(why) => Some(Wanted::Hold(format!(
            "main declares {}, which is cut, and publish/config.sh's pause between versions \
             does not read ({why}): the pump opens no version until it does",
            main.version
        ))),
        Every::Secs(every) => {
            let at = main.opened_at?;
            let next = at.saturating_add(*every);
            (seen.now < next).then(|| {
                Wanted::Held(
                    format!(
                        "main declares {}, which is cut; {} opened it at {}, and \
                         publish/config.sh's {EVERY_KEY}=\"{every}\" opens the next version no \
                         sooner than {}",
                        main.version,
                        main.opened.as_deref().map_or("its opening commit", short),
                        aterm_types::rfc3339::format_rfc3339(at),
                        aterm_types::rfc3339::format_rfc3339(next)
                    ),
                    format!("{} s from now", next - seen.now),
                )
            })
        }
    }
}

/// The repair worker that governs this clone's unfinished line `j`, and whether the line must
/// re-qualify for it ([`wanted`]). A worker opened before the prepare stopped is of a red that
/// prepare has since passed, unless the pump's own publish of this head then ended refused: a red
/// on inputs that already have a worker opens none, so that older worker is the one on it. Such a
/// worker makes the line re-qualify (and re-base where that is due) only where that refused
/// publish was itself a red of this head ([`Red::holds`]); a refusal that is no red (a notary's, a
/// signing step's) re-bases nothing, and the worker's fix is carried as a hop.
fn red_worker<'s>(seen: &'s Seen, memory: &Memory, j: &Journal) -> Option<(&'s Repair, bool)> {
    let publish = format!("publish {} {}", j.version, j.head);
    let publish_refused = memory
        .last
        .as_ref()
        .is_some_and(|last| last.key == publish && last.code == 1);
    let repair = seen
        .repair
        .as_ref()
        .filter(|repair| !j.prepared || repair.at >= j.prepared_at || publish_refused)?;
    let requalifies = !j.prepared
        || repair.at >= j.prepared_at
        || memory.red.as_ref().is_some_and(|red| red.holds(j));
    Some((repair, requalifies))
}

/// Whether `verb` opens a version or starts its line: what a live primary runs at its first look.
const fn opens(verb: &Verb) -> bool {
    matches!(verb, Verb::OpenNext | Verb::Start(_))
}

/// A standby (`--adopt`) leaves opening a version and starting its line to the primary: it runs
/// either only once it has wanted the same one at every look in a row for [`STANDBY_DEFER_SECS`]
/// ([`Memory::due`]). A live primary runs it at its first look, and its child reaches the claim
/// within that bound (a version's line has no claim, and so no holder's pump to read, until it
/// does), so the primary does it first and holds the line its start takes. A standby that won that
/// race would hold a line no primary without `--adopt` could ever take back.
fn deferred(seen: &Seen, memory: &Memory, verb: &Verb, key: &str) -> Option<Decision> {
    if !seen.adopt || !opens(verb) {
        return None;
    }
    let due = memory
        .due
        .as_ref()
        .filter(|due| due.key == key)
        .map_or(0, |due| seen.now.saturating_sub(due.since));
    (due < STANDBY_DEFER_SECS).then(|| Decision::Wait {
        why: format!(
            "this pump is a standby (--adopt): it leaves {} to the primary, and runs it only once \
             it has been due at every look for {STANDBY_DEFER_SECS} s with no primary doing it",
            if matches!(verb, Verb::OpenNext) {
                "opening the next version"
            } else {
                "starting the version's line"
            }
        ),
        note: format!("{} s from now", STANDBY_DEFER_SECS - due),
        secs: POLL_SECS,
    })
}

fn wanted(seen: &Seen, memory: &Memory) -> Wanted {
    let main = &seen.main;
    let unfinished = seen.journal.as_ref().filter(|j| !j.finished);
    // §3.5 step 1. Opening the next version needs only main: this clone's unfinished after-cut
    // does not hold it. A journal another clone's cut left behind does: a version opened here
    // could never start while it stands, and the cutter that cut it opens the next.
    if main.published && main.fixtures {
        if let Some(j) = unfinished.filter(|j| j.lost) {
            return lost(j);
        }
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
        if let Some(pause) = paused(seen) {
            return pause;
        }
        let unmeasured = if matches!(seen.every, Every::Secs(_)) && main.opened_at.is_none() {
            format!(
                "; main's history names no commit that opened {}, so the pause {EVERY_KEY} asks \
                 is not measured",
                main.version
            )
        } else {
            String::new()
        };
        return Wanted::Run {
            verb: Verb::OpenNext,
            key: format!("open-after {}", main.version),
            why: format!(
                "main declares {}, which is cut, and its tip {} holds its handoff \
                 fixtures{unmeasured}",
                main.version,
                short(&main.tip)
            ),
        };
    }
    if let Some(j) = unfinished {
        let line = format!("line v{} at {}", j.version, short(&j.head));
        if j.lost {
            return lost(j);
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
        // A line another holder re-based no longer resumes from this journal.
        if j.retired {
            return stranded(j);
        }
        // A line another pump took while this one was away is that pump's.
        if let Some(hold) = taken(seen, memory, j, &line) {
            return hold;
        }
        // A supersession left undecided holds the line: candidate.toml refuses every other run.
        if let Some(pending) = &j.undecided {
            return undecided(seen, memory, j, pending);
        }
        let rebased = rebased(memory, j);
        // A red of this head governs before PREPARED does: the journal stays PREPARED through a
        // red of the publish, and a red is never run again. The line must re-qualify then, so its
        // one re-base goes before a hop where it is due ([`rebase`]).
        let publish = format!("publish {} {}", j.version, j.head);
        if let Some((repair, requalifies)) = red_worker(seen, memory, j) {
            let rebase_due = |requalify: Requalify| {
                requalifies
                    .then(|| rebase(seen, memory, j, requalify))
                    .flatten()
            };
            return repaired(
                seen,
                memory,
                repair,
                (&line, rebased),
                |sha| {
                    rebase_due(Requalify::Fix(sha.to_string())).unwrap_or_else(|| {
                        beside(seen, j, &line).unwrap_or_else(|| Wanted::Run {
                            verb: Verb::Fix(sha.to_string()),
                            key: format!("fix {} {} {sha}", j.version, j.head),
                            why: format!(
                                "repair worker {} named {}, which the line admits",
                                repair.sid,
                                short(sha)
                            ),
                        })
                    })
                },
                rebase_due,
            );
        }
        // A red the pump's own run met, under the same rule: the line re-bases once where that
        // is due, else it holds this head until a session moves the line, and costs nothing
        // while it stands.
        if let Some(red) = memory.red.as_ref().filter(|red| red.holds(j)) {
            if let Some(rebase) = rebase(
                seen,
                memory,
                j,
                Requalify::Red(format!("caller-owned repair, input hash {}", red.hash)),
            ) {
                return rebase;
            }
            return Wanted::Hold(format!(
                "{line} is red: its run ended at a failing gate or build of R's source, whose \
                 repair is a session's (caller-owned repair, input hash {}; {}), and a red is \
                 never run again: a session lands the fix and carries it (ship release \
                 --prepare --fix <commit>), supersedes the line (--commit <rev> --supersede) or \
                 forgets it (--forget); {}",
                red.hash,
                red.last,
                candidate(seen, rebased)
            ));
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
        // §3.5: whether the holder is dead or stalled is read from the line's record before
        // anything is run. A run beside a live holder ends exit 4 and holds nothing.
        if let Some(held) = &main.held {
            return Wanted::Held(
                format!(
                    "v{} has a fleet line at {} held by {}, and this clone holds no journal of \
                     it: the pump runs nothing beside a holder whose lease is live, and takes \
                     the line over once that lease ends unrenewed or its Mac proves its \
                     process dead",
                    main.version,
                    short(head),
                    main.holder
                        .as_deref()
                        .unwrap_or("a holder its claim does not name")
                ),
                held.note(seen.now),
            );
        }
        // The holder a claim names is the release process a pump runs, which is not running
        // between that pump's runs: the standby judges the pump on the holder's Mac too, and
        // waits out a pump it cannot read until the holder's lease ends unrenewed.
        let holder = main
            .holder
            .as_deref()
            .unwrap_or("a holder its claim does not name");
        let pump = match &seen.holder_pump {
            Some(HolderPump::Live { age }) => {
                return Wanted::Held(
                    format!(
                        "v{} has a fleet line at {} held by {holder}, whose release process is \
                         not running, but the pump on its Mac holds the line's journal and is \
                         live: the line is that pump's between its runs, and the standby takes \
                         it only from a pump whose process is dead or that has not looked for \
                         {PUMP_STALE_SECS} s",
                        main.version,
                        short(head)
                    ),
                    format!("its last look was {age} s ago"),
                );
            }
            Some(HolderPump::Restarting { age }) => {
                return Wanted::Held(
                    format!(
                        "v{} has a fleet line at {} held by {holder}, whose release process is \
                         not running, and the pump on its Mac held the line's journal when its \
                         process ended: it may be restarting (launchd brings it back, a \
                         re-install replaces it), so the standby takes the line only \
                         {RESTART_GRACE_SECS} s after that pump's last look",
                        main.version,
                        short(head)
                    ),
                    format!("its last look was {age} s ago"),
                );
            }
            Some(HolderPump::Unknown(why)) => match unread_wait(seen, memory) {
                Some(note) => {
                    return Wanted::Held(
                        format!(
                            "v{} has a fleet line at {} held by {holder}, whose release process \
                             is not running; whether a pump on its Mac holds the line between its \
                             runs cannot be read ({why}), so the standby takes it only once its \
                             lease ends unrenewed and that pump has been unreadable for \
                             {PUMP_STALE_SECS} s",
                            main.version,
                            short(head)
                        ),
                        note,
                    );
                }
                None => format!(
                    "; its holder's lease has ended and its pump has been unreadable for \
                     {PUMP_STALE_SECS} s ({why})"
                ),
            },
            Some(HolderPump::Gone(why)) => format!("; no live pump holds it ({why})"),
            None => String::new(),
        };
        return Wanted::Run {
            verb: Verb::Adopt,
            key: format!("adopt {}", main.version),
            why: format!(
                "v{} has a fleet line and no journal here, and the pump was started with \
                 --adopt: the release's selection adopts it or says who holds it{pump}",
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
            repaired(
                seen,
                memory,
                repair,
                (&line, false),
                |sha| {
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
                            "{line} is red; repair worker {} named {}, which main's tip {} does \
                             not hold yet, so no line starts",
                            repair.sid,
                            short(sha),
                            short(&main.tip)
                        ))
                    }
                },
                // No line exists to re-base: the line starts at a commit of main.
                |_| None,
            )
        }
        // A start of the version that went red and left no journal, with no worker to say main
        // now holds a fix: a start at another commit is a guess that costs a whole
        // qualification, so none is made until a session starts the line or the version
        // changes. A red line a session forgot holds nothing.
        None => match memory
            .red
            .as_ref()
            .filter(|red| red.version == main.version && !red.journaled)
        {
            Some(red) => Wanted::Hold(format!(
                "v{} went red at {} before it had a line here (caller-owned repair, input hash \
                 {}; {}): the pump starts no line of it again; a session lands the fix and \
                 starts the line (ship release --prepare --commit <rev>)",
                main.version,
                red.head.as_deref().map_or("its start", short),
                red.hash,
                red.last
            )),
            None => start(
                base(seen),
                None,
                format!(
                    "main declares {}, which no release has used, and it has no fleet line",
                    main.version
                ),
            ),
        },
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
            note: String::new(),
            secs: POLL_SECS,
        };
    }
    let (verb, key, why) = match wanted(seen, memory) {
        Wanted::Hold(why) => {
            return Decision::Wait {
                why,
                note: String::new(),
                secs: POLL_SECS,
            };
        }
        Wanted::Held(why, note) => {
            return Decision::Wait {
                why,
                note,
                secs: POLL_SECS,
            };
        }
        Wanted::Run { verb, key, why } => (verb, key, why),
    };
    if let Some(wait) = deferred(seen, memory, &verb, &key) {
        return wait;
    }
    let Some(last) = memory
        .last
        .as_ref()
        .filter(|last| last.tried(&verb, &key, seen.journal.as_ref()))
    else {
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
                why: what,
                note: format!("trying again in {} s", secs - since),
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
            note: String::new(),
            secs: POLL_SECS,
        },
        3 | 130 => after(
            doubling(POLL_SECS, last.repeats, RESUME_CAP_SECS),
            format!(
                "the last run could not run or was interrupted (exit {})",
                last.code
            ),
        ),
        // The run was told who holds the line and until when. Nothing is run again while the
        // line's record, read each cycle, still keeps a contender off: a run beside a live
        // holder only ends exit 4 again.
        4 => match &seen.main.held {
            Some(held) => Decision::Wait {
                why: "the last run ended beside another holder of this line (exit 4), which \
                      still holds it: nothing is run again until its lease ends unrenewed, its \
                      Mac proves its process dead or the line changes hands"
                    .to_string(),
                note: held.note(seen.now),
                secs: POLL_SECS,
            },
            None => after(
                doubling(POLL_SECS, last.repeats, HOLDER_SECS),
                "the last run ended beside another holder of this line (exit 4), and the \
                 line's record no longer keeps a contender off"
                    .to_string(),
            ),
        },
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
                    note: String::new(),
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
    /// This clone's release journal as its own files hold it, read with no network (no tag
    /// lookup on origin, no fleet read): what the pump's fact says of its line while origin does
    /// not answer, before its first look, and the moment a run ends ([`Fact::unread`],
    /// [`Fact::left`]). A journal another clone's cut left behind reads as unfinished here, and
    /// one that does not read as none.
    fn journal_here(&mut self) -> Option<Journal>;
    /// Another live release run of this clone.
    fn holder(&mut self) -> Option<String>;
    /// `PUMP_PUBLISH_DEFAULT="1"` in main's `publish/config.sh`.
    fn publishes(&mut self) -> bool;
    /// [`EVERY_KEY`] in main's `publish/config.sh`.
    fn every(&mut self) -> Every;
    /// The pump's own process and its start ([`Doing`]).
    fn me(&mut self) -> (u32, String);
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
    /// What the fleet line's holder's Mac says of the release pumps it runs, one row per clone's
    /// pump ([`holder_pump`]): `Ok(None)` where its answer cannot say whether a pump runs there
    /// (it carries no pump row), `Ok(Some(rows))` where the rows are all there are (this Mac's own
    /// records, this pump's own left out).
    /// # Errors
    /// Why its answer could not be read.
    fn holder_pump(&mut self) -> Result<Option<Vec<PumpRow>>, String>;
    /// Whether `green`, the newest judged pass that descends from `head`, can take the line of
    /// `version` at `head` in its one re-base ([`Seen::onto`]): it declares `version`, holds
    /// `fix` where one is named, and carries a commit past `head` in the app's cone that is not
    /// `fix` and that no hop of the line already carries.
    /// # Errors
    /// Why it cannot.
    fn onto(
        &mut self,
        version: &str,
        head: &str,
        green: &str,
        fix: Option<&str>,
    ) -> Result<(), String>;
    /// Run one verb to its end, attended ([`attend`]).
    fn run(&mut self, verb: &Verb) -> Ran;
    /// Publish what this cycle read and decided for a fleet survey ([`Fact`]).
    fn publish(&mut self, fact: &Fact);
    /// Renew the lease of `version`'s fleet claim, which `holder`, a release child this pump ran,
    /// holds while the pump holds the line between its runs ([`renewal`]): the claim as it stands,
    /// renewed now by a compare-and-swap, and nothing where the claim names another holder. A
    /// renewal that fails is said, once until one succeeds, and tried again at the next look.
    fn renew(&mut self, version: &str, holder: &Process);
    /// The time this clone's pump last published a look at the world, a run's beat or its end
    /// ([`Fact::at`]), as its survey record holds it, from this process or one before it: a fact
    /// that has not read the world keeps that time ([`Fact::unread`]). `None` where there is none.
    fn looked(&mut self) -> Option<u64>;
    /// Wait `secs`; false once the pump is asked to stop.
    fn wait(&mut self, secs: u64) -> bool;
    fn say(&mut self, line: &str);
    /// Keep `memory` for the next start.
    fn remember(&mut self, memory: &Memory);
}

/// One look ([`Seen`]): main and the journal first, then the repair worker of what they make
/// the target, and for a fix it names, whether the unfinished line admits it, or with no line
/// here, whether main's tip holds it. Whether the line has had its re-base is main's own read
/// ([`Main::superseded`]). Where the unfinished line must re-qualify (a worker is on its red, or
/// `memory` holds its red) the newest judged pass past its head is read, and for a line that has
/// not re-based, whether that pass can take it. Where the record keeps no contender off the fleet
/// line and either a standby would adopt it (with `--adopt`, no journal here) or this clone's
/// unfinished line is claimed by a holder that is no child of this pump ([`taken`]), the holder's
/// Mac is asked after the pumps it runs.
///
/// A look decides on the claim main's read found, so it is discarded where it ended more than
/// [`POLL_SECS`] after that claim was read ([`Main::read_at`]: the Mac slept, or a read hung, in
/// the rest of main's read or after it): a standby may have taken the line meanwhile, and only a
/// fresh read says so.
/// # Errors
/// What could not be read, or a read of main too old to decide on.
pub fn observe(world: &mut dyn World, memory: &Memory) -> Result<Seen, String> {
    let main = world.main()?;
    let read = main.read_at;
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
    let red = |j: &Journal| memory.red.as_ref().is_some_and(|red| red.holds(j));
    let any_unfinished = unfinished.is_some();
    let foreign = main.elsewhere
        || main
            .holder_id
            .as_ref()
            .is_some_and(|holder| !memory.ran.contains(holder));
    let claimed_elsewhere = foreign
        && unfinished.is_some_and(|j| owns(Some(j)) && !j.published && j.version == main.version);
    let requalifies =
        unfinished.is_some_and(|j| owns(Some(j)) && !j.published && (repair.is_some() || red(j)));
    let journal = journal.map(|j| Journal {
        rebased: main.superseded && j.version == main.version,
        ..j
    });
    let requalifying = journal.as_ref().filter(|_| requalifies);
    let green = requalifying.and_then(|j| world.green(&j.head));
    let onto = match (requalifying, &green) {
        (Some(j), Some(green)) if !rebased(memory, j) => {
            let fix = repair.as_ref().and_then(|repair| match &repair.report {
                Report::Fix(fix) => Some(fix.as_str()),
                _ => None,
            });
            Some(world.onto(&j.version, &j.head, green, fix))
        }
        _ => None,
    };
    let base_green = match (&main.opened, any_unfinished) {
        (Some(opened), false)
            if !main.published && main.line.is_none() && main.behind > STALE_BASE_COMMITS =>
        {
            world.green(opened)
        }
        _ => None,
    };
    let adopt = world.adopts();
    let holder_pump = match &main.line {
        Some(head)
            if (adopt && !any_unfinished || claimed_elsewhere)
                && !main.published
                && main.held.is_none() =>
        {
            Some(holder_pump(world.holder_pump(), &main.version, head))
        }
        _ => None,
    };
    let now = world.now();
    if now.saturating_sub(read) > POLL_SECS {
        return Err(format!(
            "its read of the line's claim is {} s old (the Mac slept, or a read hung), and a look \
             decides on the claim that read found",
            now - read
        ));
    }
    Ok(Seen {
        now,
        foreign,
        holder: world.holder(),
        publish: world.publishes(),
        every: world.every(),
        adopt,
        main,
        journal,
        repair,
        opening,
        green,
        onto,
        base_green,
        holder_pump,
    })
}

/// One cycle: look, decide, and either run the verb and remember how it ended, or say why
/// not. Returns the seconds to wait before the next cycle (0 after a run).
pub fn cycle(world: &mut dyn World, memory: &mut Memory, said: &mut String) -> u64 {
    let seen = match observe(world, memory) {
        Ok(seen) => seen,
        Err(why) => {
            let line = format!("pump: cannot read the world ({why}); looking again");
            let looked = world.looked().unwrap_or_else(|| world.now());
            // Never through origin, which has just not answered: the journal's own files say
            // whether the line is this clone's.
            let journal = world.journal_here();
            world.publish(&Fact::unread(journal.as_ref(), looked, &line));
            // A holder's pump is unreadable, and an open or a start due, only at looks in a row
            // that read the world.
            let unread = memory.pump_unread.take().is_some();
            if memory.due.take().is_some() || unread {
                world.remember(memory);
            }
            say_once(world, memory, said, &line, "");
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
    // How long the holder's pump has been unreadable is the pump's own memory of its looks too:
    // of looks in a row at its pace, so a sleep between two unreadable looks counts nothing.
    let pump_unread = match &seen.holder_pump {
        Some(HolderPump::Unknown(_)) => {
            let line = pump_line(&seen.main);
            Some(PumpUnread {
                since: memory
                    .pump_unread
                    .as_ref()
                    .filter(|unread| {
                        unread.line == line
                            && seen.now.saturating_sub(unread.last) <= UNREAD_GAP_SECS
                    })
                    .map_or(seen.now, |unread| unread.since),
                line,
                last: seen.now,
            })
        }
        _ => None,
    };
    if memory.pump_unread != pump_unread {
        memory.pump_unread = pump_unread;
        world.remember(memory);
    }
    // What a standby wants to open or start, and since which look in a row ([`deferred`]).
    let due = match wanted(&seen, memory) {
        Wanted::Run { verb, key, .. } if seen.adopt && opens(&verb) => Some(Due {
            since: memory
                .due
                .as_ref()
                .filter(|due| {
                    due.key == key && seen.now.saturating_sub(due.last) <= UNREAD_GAP_SECS
                })
                .map_or(seen.now, |due| due.since),
            key,
            last: seen.now,
        }),
        _ => None,
    };
    if memory.due != due {
        memory.due = due;
        world.remember(memory);
    }
    match decide(&seen, memory) {
        Decision::Wait { why, note, secs } => {
            // The line this pump holds between its runs stays held while it lives.
            if let Some(holder) = renewal(&seen, memory) {
                world.renew(&seen.main.version, holder);
            }
            world.publish(&Fact::of(&seen, "wait", &why));
            say_once(
                world,
                memory,
                said,
                &format!("pump: waiting — {why}"),
                &note,
            );
            secs
        }
        Decision::Run { verb, key, why } => {
            said.clear();
            let mut line = format!("pump: {why}; running");
            for word in verb.args(&[]) {
                let _ = write!(line, " {word}");
            }
            let fact = Fact::of_run(&seen, &verb, &why);
            world.publish(&fact);
            world.say(&line);
            now_doing(world, memory, &line);
            let ran = world.run(&verb);
            let ended = format!("pump: exit {} — {}", ran.code, ran.last);
            // Before the slow reads of origin: the record says what the run left here from the
            // moment its child no longer holds the claim.
            let left = world.journal_here();
            let now = world.now();
            world.publish(&Fact::left(&fact, left.as_ref(), now, &ended));
            world.say(&ended);
            if let Some(child) = ran.child.clone() {
                memory.ran.retain(|kept| *kept != child);
                memory.ran.push(child);
                let over = memory.ran.len().saturating_sub(RAN_KEPT);
                memory.ran.drain(..over);
            }
            let repeats = memory
                .last
                .as_ref()
                .filter(|last| {
                    last.code == ran.code && last.tried(&verb, &key, seen.journal.as_ref())
                })
                .map_or(0, |last| last.repeats + 1);
            // The run's own view of main and the journal is what a later change is measured
            // against; a read that fails keeps what was seen before it ran.
            let main = world.main().unwrap_or_else(|_| seen.main.clone());
            let journal = world.journal().unwrap_or_else(|_| seen.journal.clone());
            let at = world.now();
            // A red the run ended at is of the line it left: this clone's unfinished journal,
            // else main's version at the commit a start named. A red of the opening judged a tip
            // of main, which moves, and is no line's: it backs off as a refusal.
            if let Some(hash) = ran.red.as_ref().filter(|_| ran.code == 1)
                && verb != Verb::OpenNext
            {
                let (version, head, journaled) = match journal.as_ref().filter(|j| !j.finished) {
                    Some(j) => (j.version.clone(), Some(j.head.clone()), true),
                    None => (
                        main.version.clone(),
                        match &verb {
                            Verb::Start(base) => Some(base.clone()),
                            _ => None,
                        },
                        false,
                    ),
                };
                memory.red = Some(Red {
                    version,
                    head,
                    journaled,
                    hash: hash.clone(),
                    at,
                    last: ran.last.clone(),
                });
            }
            // A re-base that decided anything spends the line's one re-base in this clone, whether
            // it recorded its supersession or was refused before it did: its next red goes back to
            // hops. One the pump never started (no release process ran), one that could not run
            // (exit 3), was interrupted or ended beside a holder decided nothing, and runs again
            // by the rules of its exit; so did one that ended with the fleet line at neither the
            // head it would retire (the journal's head, or the head an undecided supersession it
            // re-ran retires) nor the pass it re-bases onto: another holder moved the line
            // meanwhile (a session's hop), which the release refuses before or at the claim, so
            // that run was never this line's re-base to spend.
            if let Verb::Rebase { onto, requalify } = &verb
                && ran.started
                && !matches!(ran.code, 3 | 4 | 130)
                && let Some(j) = seen.journal.as_ref()
                && (main.superseded
                    || main
                        .line
                        .as_ref()
                        .is_none_or(|line| line == j.retires() || line == onto))
            {
                memory.rebased = Some(Rebased {
                    version: j.version.clone(),
                    from: j.retires().to_string(),
                    onto: onto.clone(),
                    requalify: requalify.clone(),
                    code: ran.code,
                    at,
                });
            }
            memory.last = Some(Attempt {
                verb,
                key,
                code: ran.code,
                last: ran.last,
                at,
                tip: main.tip,
                journal,
                repeats,
            });
            world.remember(memory);
            0
        }
    }
}

/// Keep what the pump does now, `line` as it said it, for a reader that is not the pump.
fn now_doing(world: &mut dyn World, memory: &mut Memory, line: &str) {
    let (pid, started) = world.me();
    memory.doing = Some(Doing {
        pid,
        started,
        reason: line.strip_prefix("pump: ").unwrap_or(line).to_string(),
        since: world.now(),
    });
    world.remember(memory);
}

/// A wait is said when its reason changes, not once a minute, and kept then
/// ([`Memory::doing`]); `note` is said with it and is no reason to say it again.
fn say_once(world: &mut dyn World, memory: &mut Memory, said: &mut String, line: &str, note: &str) {
    if said != line {
        if note.is_empty() {
            world.say(line);
        } else {
            world.say(&format!("{line} ({note})"));
        }
        line.clone_into(said);
        now_doing(world, memory, line);
    }
}

/// The loop: cycles until the world's wait says to stop, or after `limit` cycles. It first
/// publishes what this clone's journal says of its line ([`Fact::unread`]), as old as the last
/// look this clone's pump published. Its end is kept as what it does: a reader sees a pump that
/// stopped, not its last wait.
pub fn pump(world: &mut dyn World, mut memory: Memory, limit: Option<usize>) {
    // Before the first look, which reads origin for seconds: a pump that restarts (launchd's
    // KeepAlive, a re-install) says from its first moment whether it holds a line, and the
    // record no longer names its dead predecessor. It says so with its predecessor's last look,
    // so a pump that dies before every first look ages out; a clone's first pump has none.
    let journal = world.journal_here();
    let looked = world.looked().unwrap_or_else(|| world.now());
    // An unreadable holder's pump, and a standby's open or start, count only the looks of this
    // process.
    memory.pump_unread = None;
    memory.due = None;
    world.publish(&Fact::unread(
        journal.as_ref(),
        looked,
        "pump: started; its first look reads the world",
    ));
    let mut said = String::new();
    let mut cycles = 0usize;
    loop {
        let secs = cycle(world, &mut memory, &mut said);
        cycles += 1;
        if limit.is_some_and(|limit| cycles >= limit) || !world.wait(secs) {
            break;
        }
    }
    now_doing(world, &mut memory, "pump: stopped");
}

/// The newest completed row of a live release run, as the holder of its fleet line records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Epoch seconds the holder last renewed its claim's lease: by a completed row, a step
    /// boundary, or a heartbeat of its working tree with no row at all, so it measures the run's
    /// progress only while that tree is idle; the claim's ceiling is counted from the run's own
    /// progress ([`attend`]).
    pub at: u64,
    /// How many rows the holder has completed (0 where its record predates the count).
    pub rows: u64,
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
    /// The run shows life: refresh the pump's fact with this look's time ([`Fact`]), so a peer
    /// reads a pump that attends a long run as live.
    fn beat(&mut self);
    /// Wait a moment before the next look.
    fn pause(&mut self);
    fn say(&mut self, line: &str);
}

/// Attend a verb's run to its end: its exit code. A stop asked of the pump is passed on once.
///
/// A run is stalled on two readings together, neither alone: its newest completed row is older
/// than [`stall_bound`] (or it records none), and its process tree has made no progress for that
/// long. A row can legitimately take longer than the bound (a slow Mac's test stage completed
/// none for 108 minutes of 0.199.0 while its compilers and tests ran), and an idle moment is
/// nothing; a run that completes no row while its processes do nothing for the whole bound is
/// the wait that never ends (the same line's twelve hours waiting for memory it could not be
/// admitted), and so is one that records no row at all (a child hung before it takes the claim)
/// while its processes do nothing for the floor bound.
///
/// A stall is said once per row. With `interrupts` the run is then interrupted, once per row,
/// so that it ends INTERRUPTED and the pump resumes its journal with backoff: what the steps
/// before it and its compilers produced is kept, so a resume is never further back. Without it
/// (the publish, whose waits on a person or on a notary are long by nature) it is only said.
/// A run whose tree is not read is only waited on, up to the ceiling below.
///
/// A run that completes no row and begins no step for [`RUN_ALONE_SECS`] is stalled whatever its
/// processes do: past that the fleet claim's holder renews nothing, so a tree that spins without
/// finishing anything (a livelocked step, a solver loop, a child busy before it takes the claim)
/// has lost the line's claim, and the pump's fact follows it. That is counted, as the holder's own
/// heartbeat counts it, from the look that first saw the run's newest progress (its step, row and
/// count of rows), or from its start where it records none, never from the record's renewal time,
/// which that heartbeat advances while the tree works with no row.
///
/// Each look refreshes the pump's fact ([`Running::beat`]) while the run shows life; a stalled run
/// that may be interrupted shows none, so a run that ignores its interrupt (hung in its own code)
/// lets the fact age and a standby takes the line from it. A publish, never interrupted, keeps
/// its fact fresh until that ceiling: a standby does not take a line that is being published.
pub fn attend(run: &mut dyn Running, interrupts: bool) -> i32 {
    let mut passed_on = false;
    // The run's newest progress (its step and row, and how many rows), and the look that first
    // saw it: the run's start before it records any.
    let mut mark: (Option<(String, u64)>, u64) = (None, run.now());
    // The progress a stall was judged at (`Some(None)`: a run that records none), so each is
    // judged once.
    let mut judged: Option<Option<(String, u64)>> = None;
    loop {
        if let Some(code) = run.ended() {
            return code;
        }
        if !passed_on && run.stop_asked() {
            run.interrupt();
            passed_on = true;
        }
        let progress = run.progress();
        let bound = stall_bound(progress.as_ref().map_or(0, |progress| progress.p95_s));
        let now = run.now();
        let quiet = progress
            .as_ref()
            .map(|progress| now.saturating_sub(progress.at));
        let idle = run.idle();
        let newest = progress
            .as_ref()
            .map(|progress| (progress.boundary.clone(), progress.rows));
        if newest.is_some() && newest != mark.0 {
            mark = (newest, now);
        }
        // Past the claim's own ceiling the run is stalled whatever its tree does.
        let alone = now.saturating_sub(mark.1) > RUN_ALONE_SECS;
        let stalled = alone
            || (idle.is_some_and(|idle| idle >= bound) && quiet.is_none_or(|quiet| quiet > bound));
        if !((interrupts || alone) && stalled) {
            run.beat();
        }
        if !passed_on && judged.as_ref() != Some(&mark.0) && stalled {
            judged = Some(mark.0.clone());
            let last = progress.as_ref().map_or_else(
                || "the run records no row".to_string(),
                |progress| {
                    format!(
                        "the run has completed no row for {} min (last: {})",
                        if alone {
                            now.saturating_sub(mark.1)
                        } else {
                            quiet.unwrap_or(0)
                        } / 60,
                        progress.boundary
                    )
                },
            );
            let why = if alone {
                format!(
                    "the fleet claim renews nothing for a holder with no row for {} min, whatever \
                     its processes do",
                    RUN_ALONE_SECS / 60
                )
            } else {
                format!(
                    "its bound is {} min and its processes have made no progress for {} min",
                    bound / 60,
                    idle.unwrap_or(0) / 60
                )
            };
            run.say(&format!(
                "pump: STALLED — {last}; {why}; {}",
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
        run.pause();
    }
}

#[cfg(test)]
#[path = "pump_tests.rs"]
mod tests;
