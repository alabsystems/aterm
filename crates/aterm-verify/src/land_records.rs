// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land`'s records: its verdict records (`docs/DESIGN-landing-economy.md` slice 4,
//! [`crate::land_record`]) and its run records
//! (`docs/DESIGN-judged-main-release-pump-2026-11-11.md` §3.1, JM-01, [`crate::land_run`]), how a
//! landing writes them, and `verify land records`, which lists a tree's verdicts for a person.
//! Both kinds go through one spool, the clone's ([`crate::land_spool`]), to one store, the
//! journal's `land/` ([`crate::judged`]).
//!
//! - Once a judgment or a round ended by itself with the checkout still at the commit it judged,
//!   clean, its rows are recorded ([`record`]): every FAIL, and the passes, skips and NOT RUNs of a
//!   run whose record is whole and whose plan keyed them (an unkeyed one names no inputs, so a
//!   verifier older than `run-planned` records its FAILs alone). A run a poll or a signal
//!   interrupted, and one whose checkout moved, records no verdict.
//! - Each judgment, round, compose and abort writes one run record ([`Records::run_record`]), and
//!   each run one end record whatever its exit ([`finish`]): an interrupted run's before it dies
//!   of the signal, a panicked one's before the panic goes on, and that of a run whose handoff
//!   the receiving image refused by that image. A run killed outright, or one a step that is not
//!   cut short holds past the interrupt's report grace, has none.
//! - Each record goes to the spool as it is made, and is held in this run as well. The run
//!   publishes after its last use of main: one compare-and-swap append, at its end, carries this
//!   run's records with every record the spool still holds that its kind's reader takes (one it
//!   refuses is said and stays), so a kill, a failed append or a spool that could not be written
//!   loses none the spool keeps, and none another landing of the clone appended is appended
//!   twice. A run a signal
//!   interrupted does not append: its next land does. A failed append never changes the exit, and
//!   is not a push race: it counts against neither [`COMPOSES`] nor [`RETRIES`].
//! - Nothing reads the records to decide anything: a landing's verdicts, its exit and what it
//!   pushes are the same with them as without them.

use super::*;
use crate::land_record::{self, Entry, Judging as At, Origin, Record, Run, Signed, Signer};
use crate::land_run::{self, Cause, Kind, Landing};
use crate::land_spool::Spool;

/// [`Records::publish`]'s append.
pub type AppendSeam<'a> = Box<dyn FnMut(&Path, &[Signed], u64) -> Result<(), String> + 'a>;

/// The records' seams for writing: the journal's append, the clone's spool, this Mac's signer
/// and host, the clock, this run, and the interrupt it took. Boxed, so a fixture builds them
/// whole.
pub struct Records<'a> {
    /// Append signed records at a Unix time ([`crate::judged::append_land`]), retrying a
    /// compare-and-swap race on a fresh head under one deadline.
    pub append: AppendSeam<'a>,
    /// This clone's spool ([`crate::land_spool::Dir::of_clone`]).
    pub spool: Box<dyn Spool + 'a>,
    /// This Mac's fleet node identity, read once per landing in the controller; `None`: records
    /// are written unsigned.
    pub signer: Option<Box<dyn Signer + 'a>>,
    /// This Mac's host name, as records name it.
    pub host: String,
    /// The Unix time now: records' times.
    pub now: Box<dyn Fn() -> u64 + 'a>,
    /// This run: its id and start (an image the landing hands itself to keeps both,
    /// [`handoff::Handoff::run`]), this Mac and the session.
    pub run: land_run::Run,
    /// The interrupt the landing took, where one did: the signal its handler caught
    /// (`exec::group::kill_on_interrupt`), which it dies of once it has written its end record.
    pub interrupt: Box<dyn Fn() -> Option<i32> + 'a>,
    /// What this run has recorded so far.
    pub tally: Tally,
}

/// What a run has recorded so far, and what its next records need.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    /// The next run record's ordinal.
    pub seq: u32,
    /// Why the next judgment or round runs; `None` before the run's first judgment.
    pub next: Option<Cause>,
    /// The main commit the last judgment or round ran over.
    pub base: String,
    /// The landing, once main holds it.
    pub landing: Option<Landing>,
    /// Whether the last round that passed judged shared Markdown alone.
    pub markdown: bool,
    /// Whether a judgment that passed is kept for a later run as the run ends.
    pub kept: bool,
    /// Whether the landing judged nothing since it resumed the judgment an earlier run kept
    /// ([`super::KEPT`]).
    pub resumed: bool,
    /// Whether the landing said the interrupt it took ([`super::stopped`]).
    pub stopped: bool,
    /// Each record this run made that no append has held yet, and whether the spool took it,
    /// so a spool that did not keep one loses none before this run's own append.
    made: Vec<Made>,
}

/// A spool that cannot be used: every operation fails with why.
pub struct Unavailable(pub String);

impl Spool for Unavailable {
    fn put(&self, _: &Signed) -> Result<(), String> {
        Err(self.0.clone())
    }
    fn pending(&self) -> Result<crate::land_spool::Pending, String> {
        Err(self.0.clone())
    }
    fn done(&self, _: &Signed) -> Result<(), String> {
        Err(self.0.clone())
    }
}

/// A record this run made that no append has held yet ([`Tally::made`]).
#[derive(Clone, Debug)]
struct Made {
    signed: Signed,
    /// Whether the spool took it: one it took and no longer lists, another landing's append of
    /// this clone held (or the prune dropped), so this run appends it no more.
    spooled: bool,
}

/// Why `signed`, a record the spool holds, may not be appended: it fails its kind's reader
/// ([`land_run::Entry::read`], [`Entry::read`]), so the journal would file a record no reader
/// takes. `None` where it reads.
fn unreadable(signed: &Signed) -> Option<String> {
    let body = if signed.path.starts_with(land_run::DIR) {
        land_run::Entry::read(&signed.path, &signed.bytes, Origin::Spool)
            .body
            .map(drop)
    } else {
        Entry::read(&signed.path, &signed.bytes, Origin::Spool)
            .body
            .map(drop)
    };
    body.err()
}

/// Why [`Records::publish`] left records out of the journal, and whether this clone's spool
/// keeps every one of them for its next land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Unwritten {
    /// Why no append held them.
    pub why: String,
    /// `None`: the spool keeps every one of them. Else why it does not, and the records it does
    /// not keep are recorded nowhere.
    pub unkept: Option<String>,
}

impl Records<'_> {
    /// Go on as the run `carried` an image's handoff carried: its id, start, next ordinal and
    /// the cause of the step it handed over.
    pub(super) fn adopt(&mut self, carried: &super::handoff::Carried) {
        self.run.id.clone_from(&carried.id);
        self.run.started = carried.started;
        self.tally.seq = carried.seq;
        self.tally.next.clone_from(&carried.cause);
    }

    /// The cause of the judgment or round that just ran: the one the step before it left
    /// ([`Tally::next`]), else `word` (`first` for a judgment; `handed` for a round, whose
    /// cause an earlier build's handoff did not carry).
    pub(super) fn cause(&mut self, word: &str) -> Cause {
        self.tally.resumed = false;
        self.tally
            .next
            .take()
            .unwrap_or_else(|| Cause::new(word, "", ""))
    }

    /// Keep `signed` in the spool and in this run: a verdict FAIL already held at its path stays
    /// first, as the journal keeps it; any other record replaces the one at its path. Why the
    /// spool did not keep one, where it did not.
    fn keep(&mut self, signed: Vec<Signed>) -> Option<String> {
        let mut unkept = None;
        for record in signed {
            let put = self.spool.put(&record);
            let spooled = put.is_ok();
            if let Err(why) = put {
                unkept = Some(why);
            }
            let fail = crate::land_spool::kept_as(&record.path, 0).is_ok_and(|(_, _, fail)| fail);
            let made = Made {
                signed: record,
                spooled,
            };
            match self
                .tally
                .made
                .iter_mut()
                .find(|held| held.signed.path == made.signed.path)
            {
                Some(_) if fail => {}
                Some(held) => *held = made,
                None => self.tally.made.push(made),
            }
        }
        unkept
    }

    /// Write `record`, a run record of `kind` made now, as this run's next: its schema, ordinal,
    /// moment and run set here, signed, kept ([`Self::keep`]). Said where it could not be signed
    /// or kept; the landing goes on either way.
    pub(super) fn run_record(
        &mut self,
        kind: Kind,
        mut record: land_run::Record,
        say: &mut dyn FnMut(&str),
    ) {
        record.schema = land_run::SCHEMA;
        record.kind = kind.word().to_owned();
        record.seq = self.tally.seq;
        record.at = (self.now)();
        record.run = self.run.clone();
        self.tally.seq += 1;
        match record.sign(self.signer.as_deref()) {
            Ok(signed) => {
                if let Some(why) = self.keep(vec![signed]) {
                    say(&format!(
                        "verify land: this run's {} record was not kept in this clone ({why}); \
                         it is appended at the run's end, or recorded nowhere",
                        kind.word()
                    ));
                }
            }
            Err(why) => say(&format!(
                "verify land: this run's {} record was not written: {why}",
                kind.word()
            )),
        }
    }

    /// Append, in one append, every record the spool holds that reads as its kind's reader reads
    /// it ([`unreadable`]; a FAIL the spool kept first stays first, as the journal keeps it), and
    /// every record this run made that the spool did not take; forget from the spool (by its
    /// bytes, [`Spool::done`]) and the run each record the append held. A spool file that does
    /// not read, and a spooled record its reader refuses, is said and left where it is. A record
    /// the spool took that it no longer lists is not appended again: another landing of this
    /// clone appended it.
    ///
    /// # Errors
    /// The append failed: [`Unwritten`] says whether the spool keeps every record of this run
    /// for this clone's next land.
    pub(super) fn publish(
        &mut self,
        root: &Path,
        say: &mut dyn FnMut(&str),
    ) -> Result<(), Unwritten> {
        // The spool's records that read, the names of its files that do not, and why the spool
        // was not listed, where it was not.
        let (mut appended, unread, unkept) = match self.spool.pending() {
            Ok(pending) => {
                let mut said: Vec<String> = pending
                    .unreadable
                    .iter()
                    .map(|(file, why)| format!("{} ({why})", file.display()))
                    .collect();
                let mut files: Vec<String> = pending
                    .unreadable
                    .iter()
                    .filter_map(|(file, _)| file.file_name()?.to_str().map(str::to_owned))
                    .collect();
                let mut readable = Vec::new();
                for signed in pending.records {
                    match unreadable(&signed) {
                        Some(why) => {
                            said.push(format!("{} ({why})", signed.path));
                            files.extend(crate::land_spool::file_of(&signed.path));
                        }
                        None => readable.push(signed),
                    }
                }
                if !said.is_empty() {
                    say(&format!(
                        "verify land: {} spooled records do not read, so they are not appended: \
                         {}",
                        said.len(),
                        said.join(", ")
                    ));
                }
                (readable, Some(files), None)
            }
            Err(why) => {
                say(&format!(
                    "verify land: this clone's spooled records were not read ({why})"
                ));
                (Vec::new(), None, Some(why))
            }
        };
        let mut unkept = unkept;
        for made in &self.tally.made {
            if appended.iter().any(|held| held.path == made.signed.path) {
                continue;
            }
            // The spool took it, was listed, and no file of its path failed to read: it no
            // longer holds it because another landing's append did.
            let gone = made.spooled
                && unread.as_ref().is_some_and(|files| {
                    crate::land_spool::file_of(&made.signed.path)
                        .is_ok_and(|file| !files.contains(&file))
                });
            if gone {
                continue;
            }
            if !made.spooled {
                unkept.get_or_insert_with(|| "the spool did not keep it".to_owned());
            }
            appended.push(made.signed.clone());
        }
        if appended.is_empty() {
            return Ok(());
        }
        match (self.append)(root, &appended, (self.now)()) {
            Ok(()) => {
                for record in &appended {
                    let _ = self.spool.done(record);
                }
                self.tally.made.clear();
                Ok(())
            }
            Err(why) => Err(Unwritten { why, unkept }),
        }
    }
}

/// The verdict records of `judged`'s rows, from the run that started at `started` judging `at`
/// (holding `tree`) over `base`, which ended by itself with the checkout still at `at`: every
/// FAIL, and, where its record is whole, every other row its plan keyed (an unkeyed pass, skip
/// or NOT RUN names no inputs, so a verifier older than `run-planned` writes FAILs alone).
pub(super) fn end(
    records: &Records<'_>,
    (at, tree, base): (&str, &str, &str),
    started: u64,
    judged: &Judged,
) -> Vec<Record> {
    let whole = judged.whole();
    let run = Run {
        id: judged.invocation.clone().unwrap_or_default(),
        started,
        ended: (records.now)(),
        secs: Some(judged.secs),
        whole: Some(whole),
        sealed_through: Some(judged.sealed_through()),
    };
    let judging = At {
        commit: at,
        tree,
        base,
        host: &records.host,
        planned: judged.planned.as_deref(),
    };
    let fail = |word: &str| word == land_record::Word::Fail.spelled();
    judged
        .checks
        .iter()
        .filter(|row| whole || fail(&row.word))
        .filter_map(|row| Record::of_row(row, &judging, run.clone()))
        .filter(|record| record.keyed() || fail(&record.word))
        .collect()
}

/// Record the rows of the judgment or round of `at` (holding `tree`) over `base` that started at
/// `started` and ended as `judged`, by itself, with the checkout still at `at` ([`end`]): signed
/// and kept for the run's append ([`finish`]); said where one could not be signed. Nothing the
/// landing decides reads what it records.
pub(super) fn record(
    seams: &mut Seams<'_>,
    (at, tree, base): (&str, &str, &str),
    started: u64,
    judged: &Judged,
) {
    let Seams { records, say, .. } = seams;
    let rows = end(records, (at, tree, base), started, judged);
    let signed = rows
        .iter()
        .map(|record| record.sign(records.signer.as_deref()))
        .collect::<Result<Vec<_>, _>>();
    match signed {
        Ok(signed) => {
            if let Some(why) = records.keep(signed) {
                say(&format!(
                    "verify land: the verdict records of {tree} were not kept in this clone \
                     ({why}); they are appended at the run's end, or recorded nowhere"
                ));
            }
        }
        Err(why) => say(&format!(
            "verify land: the verdict records of {tree} were not written: a record was not \
             signed: {why}"
        )),
    }
}

/// `judged`'s red items, each a key ([`land_record::Item::of_finding`]), and the checks that did
/// not run with `not_run`'s reasons: what a judgment or a
/// round record keeps of its run.
pub(super) fn outcome(
    judged: &Judged,
    decision: &Decision,
) -> (Vec<land_record::Item>, usize, Vec<String>) {
    let items: Vec<land_record::Item> = judged
        .checks
        .iter()
        .filter(|row| row.word == land_record::Word::Fail.spelled())
        .flat_map(|row| {
            let ids: Vec<&String> = if row.findings.is_empty() {
                vec![&row.check]
            } else {
                row.findings.iter().collect()
            };
            ids.into_iter()
                .map(|id| land_record::Item::of_finding(&row.check, id))
                .collect::<Vec<_>>()
        })
        .collect();
    let (listed, total) = land_run::listed(&items);
    let not_run = decision
        .not_run
        .iter()
        .take(land_run::LISTED)
        .map(|why| land_run::clip(why))
        .collect();
    (listed, total, not_run)
}

/// The run record of a judgment or a round of `at` (holding `tree`) over `base` that started at
/// `started`, ran as `judged`, came to `decision` (`Err`: none, for this reason: the checkout
/// moved meanwhile) and ran for `cause`: its result, red items, the checks that did not run and
/// the crates its crate-scoped rows judged.
pub(super) fn step(
    (at, tree, base): (&str, &str, &str),
    started: u64,
    judged: &Judged,
    decision: Result<&Decision, &str>,
    cause: Cause,
) -> land_run::Record {
    let none = Decision::default();
    let (items, items_total, mut not_run) = outcome(judged, decision.unwrap_or(&none));
    let result = match decision {
        Ok(decision) => result_of(decision),
        Err(why) => {
            not_run = vec![land_run::clip(why)];
            "not-run"
        }
    };
    let mut crates = std::collections::BTreeSet::new();
    let mut widened = false;
    for (scoped, wide) in judged.checks.iter().filter_map(|row| row.scope.as_ref()) {
        crates.extend(scoped.iter().cloned());
        widened |= *wide;
    }
    let crates: Vec<String> = crates.into_iter().collect();
    let (crates, crates_total) = land_run::listed(&crates);
    land_run::Record {
        started,
        secs: judged.secs,
        head: at.to_owned(),
        base: base.to_owned(),
        tree: tree.to_owned(),
        result: result.to_owned(),
        items,
        items_total,
        not_run,
        crates,
        crates_total,
        widened,
        cause: Some(cause),
        ..land_run::Record::default()
    }
}

/// The cause `word` (`cut`, `doomed`) of main tip `tip`, which dooms a judgment for `doom`: its
/// reason, the paths it shares with the branch and, where they decided it, what main's change
/// and the branch's reach.
pub(super) fn doomed(word: &str, tip: &str, doom: &super::Doom) -> Cause {
    let mut cause = Cause::new(word, tip, &doom.why).sharing(&doom.shared);
    if let Some((main, branch)) = &doom.reach {
        cause.reach = Some(land_run::Reached::of(main));
        cause.branch = Some(land_run::Reached::of(branch));
    }
    cause
}

/// What a decision comes to as a record's result: `fail`, `not-run` or `pass`.
pub(super) fn result_of(decision: &Decision) -> &'static str {
    if !decision.counted.is_empty() {
        "fail"
    } else if !decision.not_run.is_empty() {
        "not-run"
    } else {
        "pass"
    }
}

/// The run's end, whatever its exit `code`: its end record (the exit, the interrupt it took, the
/// checkout as it stands, and its landing where main holds one), kept with the rest of the run's
/// records; then, unless an interrupt ends the run, the one append that carries them all, after
/// the run's last use of main ([`Records::publish`]). An append that failed is said, and changes
/// nothing: the exit is `code` either way.
pub(super) fn finish(
    git: &Git<'_>,
    records: &mut Records<'_>,
    say: &mut dyn FnMut(&str),
    code: i32,
) {
    let signal = (records.interrupt)();
    let now = (records.now)();
    let head = git.head().unwrap_or_default();
    let tree = if head.is_empty() {
        String::new()
    } else {
        git.tree(&head).unwrap_or_default()
    };
    let landing = records.tally.landing.clone();
    let result = match (&landing, code) {
        (Some(_), _) => "landed",
        (None, exit::PASS) => "nothing",
        (None, _) => "not-landed",
    };
    let end = land_run::Record {
        started: records.run.started,
        secs: now.saturating_sub(records.run.started),
        head,
        base: records.tally.base.clone(),
        tree,
        result: result.to_owned(),
        exit: Some(code),
        signal,
        kept: records.tally.kept,
        landing,
        ..land_run::Record::default()
    };
    records.run_record(Kind::End, end, &mut *say);
    if let Some(signal) = signal {
        say(&format!(
            "verify land: interrupted by signal {signal}: this run's records are kept in this \
             clone, and its next land appends them"
        ));
        return;
    }
    if let Err(Unwritten { why, unkept }) = records.publish(git.root, &mut *say) {
        say(&match unkept {
            None => format!(
                "verify land: this run's records were not appended to the journal ({why}); they \
                 are kept in this clone, and its next land appends them"
            ),
            Some(spool) => format!(
                "verify land: this run's records were not appended to the journal ({why}), nor \
                 all kept in this clone ({spool}): those it does not keep are recorded nowhere"
            ),
        });
    }
}

/// `host` as one token: each byte outside letters, digits, `.`, `_` and `-` as `-`
/// ([`land_record::tokened`]), and `unknown` for none.
fn token(host: &str) -> String {
    let token = land_record::tokened(host);
    if token.is_empty() {
        "unknown".to_owned()
    } else {
        token
    }
}

/// The live records: the journal on [`REMOTE`] under [`crate::judged`]'s deadline, the clone's
/// spool, this Mac's fleet node identity (`fleet_member::read_node_identity` in aterm's state
/// directory, which never mints one) and its kernel host name, and a new run: a fresh id, started
/// now, this Mac's memory and cores and the aterm session it runs in, whose interrupt is the one
/// `exec::group::kill_on_interrupt`'s handler took.
pub(super) fn live(root: &Path) -> Records<'static> {
    let spool: Box<dyn Spool> = match crate::land_spool::Dir::of_clone(root) {
        Ok(dir) => Box::new(dir),
        Err(why) => Box::new(Unavailable(format!(
            "this clone's spool was not found: {why}"
        ))),
    };
    let identity = aterm_types::dirs::state_dir().and_then(|dir| {
        aterm_net::fleet_member::read_node_identity(&dir)
            .ok()
            .flatten()
    });
    let node = identity.as_ref().map(|me| me.node().to_owned());
    let signer = identity.map(|identity| Box::new(identity) as Box<dyn Signer>);
    let host = token(&aterm_uds::process::kernel_host_name().unwrap_or_default());
    let session = std::env::var("ATERM_SESSION_ID")
        .ok()
        .map(|id| land_record::tokened(&id))
        .filter(|id| !id.is_empty());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let run = land_run::Run {
        id: run_id(
            aterm_uds::rand::hex_token::<16>(),
            std::process::id(),
            nanos,
            &host,
        ),
        started: unix_now(),
        host: host.clone(),
        node,
        session,
        memory_mib: aterm_sysprobe::physical_memory_mib(),
        cores: u32::try_from(aterm_sysprobe::cpu_layout().logical_cores())
            .ok()
            .filter(|cores| *cores > 0),
    };
    Records {
        append: Box::new(|root, records, now| {
            crate::judged::append_land(root, REMOTE, records, now, crate::judged::deadline())
                .map(drop)
        }),
        spool,
        signer,
        host,
        now: Box::new(unix_now),
        run,
        interrupt: Box::new(crate::exec::group::taken),
        tally: Tally::default(),
    }
}

/// A run's id, 32 lowercase hex digits: the system's random ones `drawn`, else, where none were
/// drawn, the first 32 of a SHA-256 of process `pid` on `host` at `nanos` since the epoch. A run
/// always has one, so every record files and a handoff always carries the run.
pub(super) fn run_id(drawn: std::io::Result<String>, pid: u32, nanos: u128, host: &str) -> String {
    drawn.unwrap_or_else(|_| {
        let mut id = crate::land_seal::hash([
            "aterm-verify/land-run-id",
            &pid.to_string(),
            &nanos.to_string(),
            host,
        ]);
        id.truncate(32);
        id
    })
}

/// The Unix time now, in seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `verify land records [<tree>|<commit>] [--show-seal]`'s usage.
pub const RECORDS_USAGE: &str = "usage: targo --unverified verify land records [<tree>|<commit>] \
                                 [--show-seal]";

/// `verify land records [<tree>|<commit>] [--show-seal]`: every verdict record of a tree (a
/// commit's tree; HEAD's where none is named), from the journal on [`REMOTE`] and this clone's
/// spool: each one's check, word, host, age, base, run, whether that run was whole and sealed
/// through, whose key signed it, and where it is kept; each one that does not read, and why.
/// `--show-seal` adds each record's seal and local field digests (digests only; no value, no
/// path). It lists; it decides nothing.
pub fn records_cli(args: &[String], cwd: &Path) -> i32 {
    let mut named = None;
    let mut show_seal = false;
    for arg in args {
        match arg.as_str() {
            "--show-seal" if !show_seal => show_seal = true,
            "--help" | "-h" => {
                println!("{RECORDS_USAGE}");
                return exit::PASS;
            }
            word if named.is_none() && !word.starts_with('-') => named = Some(word.to_owned()),
            _ => {
                eprintln!("{RECORDS_USAGE}");
                return exit::USAGE;
            }
        }
    }
    let Some(root) = crate::checkout_root(cwd) else {
        eprintln!(
            "verify land records: COULD NOT RUN — {} is not inside a git checkout",
            cwd.display()
        );
        return exit::COULD_NOT_RUN;
    };
    let named = named.unwrap_or_else(|| "HEAD".to_owned());
    let tree = match crate::judged::git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{named}^{{tree}}"),
        ],
    ) {
        Ok(tree) => tree,
        Err(why) => {
            eprintln!("verify land records: COULD NOT RUN — {named} names no tree: {why}");
            return exit::COULD_NOT_RUN;
        }
    };
    let until = crate::judged::deadline();
    let journal = crate::judged::head(&root, REMOTE, until).and_then(|head| {
        crate::judged::land_records(&root, head.as_deref(), &[tree.as_str()], until)
    });
    let mut entries = match &journal {
        Ok(entries) => entries.clone(),
        Err(_) => Vec::new(),
    };
    let records = live(&root);
    match spooled(&records, &tree) {
        Ok(spooled) => entries.extend(spooled),
        Err(why) => eprintln!("verify land records: this clone's spool was not read: {why}"),
    }
    let own = records.signer.as_ref().map(|signer| signer.key());
    for line in listing(&tree, &entries, own, unix_now(), show_seal) {
        println!("{line}");
    }
    match journal {
        Ok(_) => exit::PASS,
        Err(why) => {
            eprintln!(
                "verify land records: COULD NOT RUN — the journal was not read ({why}); only \
                 this clone's spool is listed"
            );
            exit::COULD_NOT_RUN
        }
    }
}

/// The records `records`' spool holds under `tree`, each held to [`Entry::read`]'s checks, and
/// each spool file named as a record of it that could not be read at all ([`Entry::unread`]).
///
/// # Errors
/// The spool could not be listed.
fn spooled(records: &Records<'_>, tree: &str) -> Result<Vec<Entry>, String> {
    let pending = records.spool.pending()?;
    let under = |path: &str| land_record::Filed::parse(path).is_ok_and(|filed| filed.tree == tree);
    let mut entries: Vec<Entry> = pending
        .records
        .iter()
        .filter(|signed| under(&signed.path))
        .map(|signed| Entry::read(&signed.path, &signed.bytes, Origin::Spool))
        .collect();
    for (file, why) in &pending.unreadable {
        if let Some(path) = file
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crate::land_spool::path_of)
            .filter(|path| under(path))
        {
            entries.push(Entry::unread(&path, Origin::Spool, why.clone()));
        }
    }
    Ok(entries)
}

/// The lines [`records_cli`] prints for `entries` under `tree`, at `now`, this Mac's key `own`.
pub(super) fn listing(
    tree: &str,
    entries: &[Entry],
    own: Option<aterm_net::fleet::NodeKey>,
    now: u64,
    show_seal: bool,
) -> Vec<String> {
    let under: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.tree() == Some(tree))
        .collect();
    let mut lines = vec![format!("{tree}: {} records", under.len())];
    for entry in under {
        let origin = match entry.origin {
            Origin::Journal => "journal",
            Origin::Spool => "spooled",
        };
        let read = match &entry.body {
            Ok(read) => read,
            Err(why) => {
                lines.push(format!("  {} ({origin}): unreadable: {why}", entry.path));
                continue;
            }
        };
        let record = &read.record;
        let flag = |value: Option<bool>| match value {
            Some(true) => "yes",
            Some(false) => "no",
            None => "-",
        };
        let signed = match (&read.signature, own) {
            (None, _) => "unsigned".to_owned(),
            (Some(sig), Some(own)) if sig.key == own => "signed by this Mac".to_owned(),
            (Some(sig), _) => format!("signed by key {}", &sig.key.hex()[..16]),
        };
        lines.push(format!(
            "  {} {} base={} host={} {} ago run={} whole={} sealed={} {signed} ({origin})",
            record.check,
            record.word,
            record.base,
            record.local.host,
            land_record::ago(now.saturating_sub(record.run.ended)),
            if record.run.id.is_empty() {
                "-"
            } else {
                &record.run.id
            },
            flag(record.run.whole),
            flag(record.run.sealed_through),
        ));
        if show_seal {
            for (name, digest) in &record.seal_fields {
                lines.push(format!("    seal {name} {digest}"));
            }
            lines.push(format!("    local {}", record.local.digest));
            for (name, digest) in &record.local.fields {
                lines.push(format!("    local {name} {digest}"));
            }
        }
    }
    lines
}
