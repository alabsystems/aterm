// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAB'S HARNESS TAPES.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::supervise::tape::codec::Item;
use aterm_agent::supervise::tape::corpus::{self, Census, Scrub, token};
use aterm_agent::supervise::tape::{replay, report, show, store, text};

use crate::door::Door;
use crate::paths::{paths, physical, profile_safe};
use crate::pyjson::J;
use crate::stop::{self, Stopped};
use crate::{Fail, Res};

/// Development environment variable `(name, value)` that enables hosted-loop tape recording.
#[must_use]
pub fn seam() -> (String, String) {
    (store::HOSTED_SEAM.to_string(), "1".to_string())
}

fn tapes_under(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let kind = std::fs::symlink_metadata(&d)?.file_type();
        if !kind.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a tape directory",
                d.display()
            )));
        }
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other(format!(
                    "{} is a link in the tape inventory",
                    entry.path().display()
                )));
            }
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file() && entry.path().extension().is_some_and(|x| x == "atape") {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

fn ended(path: &Path) -> bool {
    crate::util::read_regular_file(path)
        .ok()
        .and_then(|raw| show::open(&raw, false).ok())
        .is_some_and(|tape| tape.items.iter().any(|i| matches!(i, Item::End(_))))
}

fn recording(dir: &Path) -> io::Result<Vec<String>> {
    let mut sids = Vec::new();
    for path in tapes_under(dir)? {
        if ended(&path) {
            continue;
        }
        let sid = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("an open tape has no UTF-8 session directory"))?;
        if sid != store::KEPT {
            sids.push(sid.to_string());
        }
    }
    sids.sort();
    sids.dedup();
    Ok(sids)
}

/// End every loop still recording under `dir` by setting its session's Autopilot to `off`.
/// # Errors
/// Interrupted.
pub fn end_loops(door: &Door<'_>, dir: &Path) -> Result<bool, Stopped> {
    let open = match recording(dir) {
        Ok(open) => open,
        Err(_) => return Ok(false),
    };
    if open.is_empty() {
        return Ok(true);
    }
    for sid in &open {
        let _ = door.ctl(&[&format!("@{sid}"), "meta", "set", "autopilot", "off"]);
    }
    let stop_by = Instant::now() + Duration::from_secs(60);
    loop {
        match recording(dir) {
            Ok(open) if open.is_empty() => return Ok(true),
            Err(_) => return Ok(false),
            Ok(_) => {}
        }
        if Instant::now() >= stop_by {
            return Ok(false);
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

/// What the tapes under a directory cost and hold ([`summary`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cost {
    /// The tapes.
    pub tapes: u64,
    /// Tapes with no end marker (cut off mid-run).
    pub open: u64,
    /// Their bytes.
    pub bytes: u64,
    /// The door crossings on them: round trips, outputs and flags.
    pub doors: u64,
    /// Their decisions.
    pub decisions: u64,
}

/// What the tapes under `dir` cost and hold, for `summary.json`.
pub fn summary(dir: &Path) -> Result<Cost, String> {
    let mut cost = Cost::default();
    for path in tapes_under(dir).map_err(|e| format!("incomplete tape inventory: {e}"))? {
        let raw = crate::util::read_regular_file(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        cost.tapes += 1;
        cost.bytes += raw.len() as u64;
        match show::open(&raw, true) {
            Ok(tape) => {
                if !tape.items.iter().any(|i| matches!(i, Item::End(_))) {
                    cost.open += 1;
                }
                let s = show::stats(&tape);
                cost.doors += s.doors.values().map(|d| d.records as u64).sum::<u64>();
                cost.decisions += s.decisions.values().map(|n| *n as u64).sum::<u64>();
            }
            Err(_) => cost.open += 1,
        }
    }
    Ok(cost)
}

/// Replay each tape under `dir` strictly and return `(path, verdict_or_error)` pairs.
#[must_use]
pub fn replay_all(dir: &Path) -> Vec<(PathBuf, Result<report::Verdict, String>)> {
    let tapes = match tapes_under(dir) {
        Ok(tapes) => tapes,
        Err(e) => {
            return vec![(
                dir.to_path_buf(),
                Err(format!("incomplete tape inventory: {e}")),
            )];
        }
    };
    tapes
        .into_iter()
        .map(|path| {
            let verdict = crate::util::read_regular_file(&path)
                .map_err(|e| format!("cannot read it: {e}"))
                .and_then(|raw| replay::replay(&raw, &replay::Options::default()))
                .map(|r| r.verdict);
            (path, verdict)
        })
        .collect()
}

const USAGE: &str = "tape promote <results-run|dir|tape> [--as NAME] [--fewer] | tape lint \
                     [FILE|DIR...] | tape replay-jailed [FILE|DIR...] | tape rerecord NAME \
                     [--fewer]";

/// `tape promote|lint|replay-jailed|rerecord`.
/// # Errors
/// A usage error, or the verb's own refusal.
pub fn cmd_tape(args: &[String]) -> Res {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("promote") => cmd_promote(rest),
        Some("lint") => cmd_lint(rest),
        Some("replay-jailed") => cmd_replay_jailed(rest),
        Some("rerecord") => cmd_rerecord(rest),
        _ => Err(Fail::usage(format!("usage: {USAGE}"))),
    }
}

/// Where the corpus lives in the checkout this lab was built from.
#[must_use]
pub fn corpus_dir() -> PathBuf {
    paths().repo.join("crates/aterm-agent/tests/testdata/tapes")
}

fn hostnames() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for args in [&[][..], &["-s"][..]] {
        let name = aterm_uds::spawnfd::command("/bin/hostname")
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Build the recording machine's scrub table and primary home path.
/// # Errors
/// No home directory is set, or a name is refused by the scrub rules.
pub fn machine_scrub() -> Result<(Scrub, PathBuf), String> {
    machine_scrub_of(paths(), &hostnames())
}

/// [`machine_scrub`] of the paths `p` and the machine's host names `hosts`, which a test names
/// rather than this machine's.
fn machine_scrub_of(p: &crate::paths::Paths, hosts: &[String]) -> Result<(Scrub, PathBuf), String> {
    let home = PathBuf::from(p.need_home()?);
    let mut scrub = Scrub::new();
    scrub.path(&p.state, token::STATE)?;
    for repo in [p.repo.clone(), physical(&p.repo)] {
        scrub.path(&repo, token::CHECKOUT)?;
    }
    for owner in p.guarded_homes() {
        scrub.path(Path::new(&owner), token::HOME)?;
    }
    if let Some(tmp) = crate::sys::user_temp_dir() {
        scrub.path(Path::new(&tmp), token::TMP)?;
    }
    scrub.word(&p.user, token::USER)?;
    for host in hosts {
        scrub.host(host, &p.user)?;
    }
    Ok((scrub, home))
}

fn source_path(from: &str) -> PathBuf {
    let run = crate::scn::results_dir().join(from);
    if !from.contains('/') && run.is_dir() {
        run
    } else {
        PathBuf::from(from)
    }
}

/// The scenario a results run is a run of: its directory's name without
/// the run's `-NN`.
pub(crate) fn scenario_of(run: &Path) -> Option<String> {
    let name = run.file_name()?.to_str()?;
    let (scenario, k) = name.rsplit_once('-')?;
    (k.len() == 2 && k.bytes().all(|b| b.is_ascii_digit()) && !scenario.is_empty())
        .then(|| scenario.to_string())
}

/// Whether `name` may name a corpus tape: lower-case letters, digits, `-`
/// and `.`, a letter or digit first, and a file name's length.
pub(crate) fn corpus_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
}

/// Find the tape to promote: `source` itself, or the single ended tape under it.
/// # Errors
/// None ended there, or more than one (each named: give its path).
fn the_tape(source: &Path) -> Result<PathBuf, String> {
    if source.is_file() {
        return Ok(source.to_path_buf());
    }
    let all = corpus::tapes_in(&[source.to_path_buf()])?;
    let whole: Vec<&PathBuf> = all.iter().filter(|p| ended(p)).collect();
    match whole.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!(
            "no tape under {} carries its end ({} tape(s) there): a corpus keeps whole runs",
            source.display(),
            all.len()
        )),
        many => Err(format!(
            "{} tapes under {} carry their ends; promote one by its path: {}",
            many.len(),
            source.display(),
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Why a scenario run's summary says the run did not go as its scenario
/// meant — the `error` it noted (no card, no idle row, a decision never
/// journaled) or the `exception` it ended on — or `None`.
pub(crate) fn run_failed(summary: &J) -> Option<String> {
    ["error", "exception"].into_iter().find_map(|key| {
        let said = summary.get(key).filter(|v| v.truthy())?;
        let said = said.as_str().map_or_else(|| said.py(), str::to_string);
        Some(format!(
            "{key}: {}",
            said.chars().take(300).collect::<String>()
        ))
    })
}

pub(crate) fn summary_refusal(source: &Path) -> Result<(), String> {
    let path = source.join("summary.json");
    if !source.is_dir() || !path.exists() {
        return Ok(());
    }
    let raw = crate::util::read_regular_file(&path)
        .map_err(|e| format!("{} does not read: {e}", path.display()))?;
    let summary =
        J::parse_bytes(&raw).map_err(|e| format!("{} does not read: {e}", path.display()))?;
    if summary
        .get(crate::occupancy_standin::SUMMARY_KEY)
        .is_some_and(J::truthy)
    {
        return Err(format!(
            "{} ran the lab's stand-in, no real agent: its tape is not promoted",
            source.display()
        ));
    }
    match run_failed(&summary) {
        Some(why) => Err(format!(
            "{} did not go as its scenario meant ({why}): its tape is not promoted; run it again",
            source.display()
        )),
        None => Ok(()),
    }
}

fn lost(old: &Census, new: &Census) -> Vec<&'static str> {
    let mut lost: Vec<&'static str> = [
        ("approval(s)", old.approvals, new.approvals),
        ("question(s)", old.questions, new.questions),
        ("turn end(s)", old.turn_ends, new.turn_ends),
        ("wall(s)", old.walls, new.walls),
    ]
    .into_iter()
    .filter(|(_, old, new)| *old > 0 && *new == 0)
    .map(|(word, ..)| word)
    .collect();
    if new.acts < old.acts {
        lost.push("act(s)");
    }
    lost
}

fn held(path: &Path) -> Option<Census> {
    let raw = crate::util::read_regular_file(path).ok()?;
    Some(
        show::open(&raw, false)
            .map(|tape| corpus::census(&tape))
            .unwrap_or_default(),
    )
}

/// Promote `source`'s tape into the corpus as `name`.
/// # Errors
/// Failed run, missing tape, scrub/promote refusal, disallowed loss, or write error.
fn promote_into(
    source: &Path,
    name: &str,
    fewer: bool,
) -> Result<(PathBuf, corpus::Promoted, Option<Census>), String> {
    summary_refusal(source)?;
    let tape = the_tape(source)?;
    let bytes = crate::util::read_regular_file(&tape)
        .map_err(|e| format!("cannot read {}: {e}", tape.display()))?;
    let (scrub, home) = machine_scrub()?;
    let kept = corpus::promote(&bytes, &scrub, Some(&home))
        .map_err(|e| format!("{} is not promoted: {e}", tape.display()))?;
    let dir = corpus_dir();
    let out = dir.join(format!("{name}{}", corpus::TEXT_SUFFIX));
    let old = held(&out);
    if let Some(old) = &old {
        let lost = lost(old, &kept.census);
        if !lost.is_empty() && !fewer {
            return Err(format!(
                "{} is not promoted over {}: it loses {} ({}) of the tape there ({old}); a run \
                 that did not reach what the scenario is kept for is run again, and a \
                 deliberate change of what the loop does is promoted with --fewer",
                tape.display(),
                out.display(),
                lost.join(", "),
                kept.census
            ));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    crate::util::atomic_write(&out, kept.text.as_bytes())
        .map_err(|e| format!("{}: {e}", out.display()))?;
    Ok((out, kept, old))
}

fn said_promoted(verb: &str, out: &Path, kept: &corpus::Promoted, old: Option<&Census>) {
    println!(
        "{verb} {}: {}, {} effects, {} bytes of text",
        out.display(),
        kept.census,
        kept.effects,
        kept.text.len()
    );
    if let Some(old) = old {
        println!("the tape it replaced held {old}");
    }
}

/// `tape promote <results-run|dir|tape> [--as NAME] [--fewer]`.
/// # Errors
/// A usage error or a promote refusal.
pub fn cmd_promote(args: &[String]) -> Res {
    let (mut from, mut name, mut fewer) = (None, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--as" => {
                name = Some(
                    it.next()
                        .ok_or_else(|| Fail::usage("tape promote: --as needs a NAME"))?
                        .clone(),
                );
            }
            "--fewer" => fewer = true,
            s if s.starts_with("--") => {
                return Err(Fail::usage(format!("tape promote: unknown option {s}")));
            }
            s if from.is_none() => from = Some(s.to_string()),
            s => {
                return Err(Fail::usage(format!(
                    "tape promote: one source, not also {s}"
                )));
            }
        }
    }
    let from = from.ok_or_else(|| Fail::usage(format!("usage: {USAGE}")))?;
    let source = source_path(&from);
    let name = name.or_else(|| scenario_of(&source)).ok_or_else(|| {
        Fail::usage(format!(
            "tape promote: {from} is no results run (<scenario>-NN): name the tape with --as NAME"
        ))
    })?;
    if !corpus_name(&name) {
        return Err(Fail::usage(format!(
            "tape promote: {name:?} is no corpus name (a-z, 0-9, `-`, `.`)"
        )));
    }
    let (out, kept, old) = promote_into(&source, &name, fewer).map_err(Fail::die)?;
    said_promoted("promoted", &out, &kept, old.as_ref());
    println!(
        "read it before it is committed: every screen is a row to a line, and a scrub maps names, \
         it does not remove what a screen shows"
    );
    Ok(0)
}

fn named_or_corpus(args: &[String]) -> Result<Vec<PathBuf>, Fail> {
    let named: Vec<PathBuf> = if args.is_empty() {
        vec![corpus_dir()]
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    let tapes = corpus::tapes_in(&named).map_err(Fail::die)?;
    if tapes.is_empty() {
        return Err(Fail::die(format!(
            "no tape (*.atape, *{}) under {}",
            corpus::TEXT_SUFFIX,
            named
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(tapes)
}

/// `tape lint [FILE|DIR...]`: lint each tape's text form against `corpus::lint`.
/// # Errors
/// No tape, or the machine's names do not resolve.
pub fn cmd_lint(args: &[String]) -> Res {
    let tapes = named_or_corpus(args)?;
    let (scrub, home) = machine_scrub().map_err(Fail::die)?;
    let mut flagged = 0;
    for path in &tapes {
        let problems = crate::util::read_regular_file(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| show::open(&bytes, false))
            .map(|tape| {
                let text = text::render(tape.header.as_deref(), &tape.items);
                corpus::lint(&text, &scrub, Some(&home))
            })
            .unwrap_or_else(|e| vec![format!("does not read: {e}")]);
        if problems.is_empty() {
            println!("clean {}", path.display());
        } else {
            flagged += 1;
            println!("LINT {}", path.display());
            for problem in problems {
                println!("    {problem}");
            }
        }
    }
    println!("{} tape(s): {flagged} flagged", tapes.len());
    Ok(i32::from(flagged > 0))
}

/// Render the replay sandbox profile for `bin` and `tape`.
/// # Errors
/// A path a sandbox profile cannot quote.
fn jail_profile(bin: &Path, tape: &Path) -> Result<String, String> {
    let quoted = |p: &Path| {
        p.to_str()
            .filter(|s| profile_safe(s))
            .map(str::to_string)
            .ok_or_else(|| format!("{}: a path the jail's profile cannot quote", p.display()))
    };
    Ok(include_str!("replay_jail.sb")
        .replace("@BIN@", &quoted(bin)?)
        .replace("@TAPE@", &quoted(tape)?))
}

struct Jailed {
    code: Option<i32>,
    out: String,
    err: String,
}

/// Run `aterm drive replay --check <asked>` under `sandbox-exec` allowing `allowed`.
/// # Errors
/// The profile, the spawn, or an interrupt.
fn jailed(bin: &Path, allowed: &Path, asked: &Path) -> Result<Jailed, Fail> {
    let profile = jail_profile(bin, allowed).map_err(Fail::die)?;
    let mut child = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(&profile)
        .arg(bin)
        .args(["drive", "replay", "--check"])
        .arg(asked)
        .env_clear()
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .map_err(|e| Fail::die(format!("sandbox-exec: {e}")))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let hung_at = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| Fail::die(format!("sandbox-exec: {e}")))?
        {
            break Some(status);
        }
        if Instant::now() >= hung_at {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        stop::sleep(Duration::from_millis(50))?;
    };
    let (out, mut err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    if status.is_none() {
        err.push_str("the jailed replay had not ended a minute after it started: killed\n");
    }
    Ok(Jailed {
        code: status.and_then(|s| s.code()),
        out,
        err,
    })
}

fn drain<P: io::Read + Send + 'static>(pipe: Option<P>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut text = String::new();
        if let Some(pipe) = pipe {
            let _ = pipe.take(16 * 1024 * 1024).read_to_string(&mut text);
        }
        text
    })
}

/// `tape replay-jailed [FILE|DIR...]`: verify the sandbox control and replay each tape jailed.
/// # Errors
/// No tape, no aterm built beside the lab, or the control's copy.
pub fn cmd_replay_jailed(args: &[String]) -> Res {
    let tapes = named_or_corpus(args)?;
    let bin = std::fs::canonicalize(paths().need_aterm().map_err(Fail::die)?)
        .map_err(|e| Fail::die(format!("the aterm to replay with: {e}")))?;
    stop::install();
    let physical_tape =
        |p: &Path| std::fs::canonicalize(p).map_err(|e| Fail::die(format!("{}: {e}", p.display())));
    let first = physical_tape(&tapes[0])?;
    let scratch = paths().state.join("replay-jail");
    std::fs::create_dir_all(&scratch)
        .map_err(|e| Fail::die(format!("{}: {e}", scratch.display())))?;
    let copy = physical_tape(&scratch)?.join(format!("control-{}.atape.txt", std::process::id()));
    crate::util::copy_regular_file(&first, &copy)
        .map_err(|e| Fail::die(format!("{}: {e}", copy.display())))?;
    let control = jailed(&bin, &first, &copy);
    let _ = std::fs::remove_file(&copy);
    let control = control?;
    let said = format!("{}{}", control.out, control.err);
    let refused = said
        .lines()
        .find(|l| l.contains("not a readable regular file"));
    if control.code != Some(2) || refused.is_none() {
        println!(
            "THE JAIL DID NOT HOLD: a copy of {} the profile does not name was not refused (exit \
             {:?})\n{}{}",
            first.display(),
            control.code,
            control.out,
            control.err
        );
        return Ok(1);
    }
    println!(
        "the jail holds: a copy of the tape it does not name was refused ({})",
        refused.unwrap_or_default()
    );
    let mut failed = 0;
    for tape in &tapes {
        let tape = physical_tape(tape)?;
        let run = jailed(&bin, &tape, &tape)?;
        let line = run.out.lines().next().unwrap_or_default();
        if run.code == Some(0) {
            println!("{line}");
        } else {
            failed += 1;
            println!(
                "FAILED in the jail (exit {:?}): {}",
                run.code,
                tape.display()
            );
            for l in run.out.lines().chain(run.err.lines()) {
                println!("    {l}");
            }
        }
    }
    println!(
        "{} tape(s) replayed in the jail: {} took every act, {failed} did not",
        tapes.len(),
        tapes.len() - failed
    );
    Ok(i32::from(failed > 0))
}

/// `tape rerecord NAME [--fewer]`: run scenario `NAME` and promote its tape over the corpus.
/// # Errors
/// Unknown scenario name, an interrupt, a failed run, or a promote refusal.
pub fn cmd_rerecord(args: &[String]) -> Res {
    let (name, fewer) = match args {
        [name] => (name, false),
        [name, flag] | [flag, name] if flag == "--fewer" => (name, true),
        _ => return Err(Fail::usage("usage: tape rerecord NAME [--fewer]")),
    };
    let scenario = crate::scn::scenarios()
        .into_iter()
        .find(|s| s.name == *name)
        .ok_or_else(|| {
            Fail::die(format!(
                "no scenario named {name}: a corpus tape is rerecorded by the scenario it is named \
                 after (`scn list`); one promoted --as another name is rerecorded by hand"
            ))
        })?;
    let tag = std::env::var("LAB_TAG")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "survey".into());
    stop::install();
    let ctx = crate::scn::run_one(&scenario, 1, &tag)?;
    let dir = ctx
        .dir
        .clone()
        .ok_or_else(|| Fail::die("the scenario run kept no records"))?;
    if let Some(why) = run_failed(&ctx.sumd) {
        return Err(Fail::die(format!(
            "{name} did not go as its scenario meant ({why}; its records: {}): the corpus's \
             {name} is kept",
            dir.display()
        )));
    }
    let (out, kept, old) = promote_into(&dir, name, fewer).map_err(Fail::die)?;
    said_promoted("rerecorded", &out, &kept, old.as_ref());
    Ok(0)
}

#[cfg(test)]
#[path = "tape_corpus_tests.rs"]
mod corpus_tests;

#[cfg(test)]
#[path = "tape_scrub_tests.rs"]
mod scrub_tests;

#[cfg(test)]
#[path = "tape_inventory_tests.rs"]
mod inventory_tests;

#[cfg(test)]
#[path = "tape_completeness_postfix_tests.rs"]
mod completeness_postfix_tests;
