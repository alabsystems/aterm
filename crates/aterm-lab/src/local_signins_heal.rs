// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `local-signins` world where the accounts upkeep's heal of the default Claude login is due
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 6: local state answers whether an
//! account is signed in here, and a process probe is no local answer).
//!
//! [`lay`] puts in a lane's world everything `default_login_heal_due` reads: a default row in
//! Claude Code's roster (one with no login directory, its address on record), the default home's
//! profile signed in as that address, a sign-out on record for the row, and a ledger `displaced`
//! event after it that moved the default login onto the row. The default home keeps its login as
//! [`Held`] says: in its stored login file, or (as Claude Code keeps it on macOS) in a keychain
//! this world has no entry in, so no file is laid. [`judge`] reads, after an upkeep pass, whether
//! the lane's stand-in `claude` ran (`local_signins::standin_main` notes each run in the world's
//! `standin-ran.log`), how many probe records the journal spool holds, and what the store says
//! of the row: a stored login heals it from the file alone where Claude Code keeps the default
//! login in that file, and a keychain-held one is no local answer, so its sign-out stands. On
//! macOS, whose default login is the keychain's, a stored login file is not that login either,
//! so a [`Held::File`] world's sign-out stands there too
//! (`aterm_agent::ai::connector::claude_default_login_in_file`). Either way nothing runs and
//! nothing is probed.

use crate::instance::World;
use aterm_agent::ai::connector::Cli;
use aterm_agent::harness::accounts::{self, Auth, Event, Kind, Store};
use std::path::{Path, PathBuf};

/// The file the lane's stand-ins note their runs in, under the world's directory
/// (`local_signins`'s `RAN`).
const RAN: &str = "standin-ran.log";

/// How long before the lay's `now` the row's sign-out on record is written, in seconds.
const SIGNED_OUT_AGO: i64 = 120;

/// How long before the lay's `now` the default login moved onto the row, in seconds: after its
/// sign-out, so the sign-out is an earlier login's and the heal is due.
const MOVED_AGO: i64 = 60;

/// Where the default home keeps its login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    /// In its stored login file (`~/.claude/.credentials.json`), holding a refresh token.
    File,
    /// In a keychain: the default home keeps no stored login file.
    Keychain,
}

/// Whether the heal heals a default login held as `held`: only from its stored login file, and
/// only where Claude Code keeps the default login in that file (every OS but macOS).
#[must_use]
pub(crate) fn heals(held: Held) -> bool {
    held == Held::File && aterm_agent::ai::connector::claude_default_login_in_file()
}

/// The default row's label on side `side` (`ac-def`).
#[must_use]
pub(crate) fn default_label(side: char) -> String {
    format!("{side}c-def")
}

/// The row the default login held before it moved onto the default row: on record only as the
/// `displaced` event's account, never a roster row, so the move takes no other row's sessions.
fn was_label(side: char) -> String {
    format!("{side}c-was")
}

/// The default row's address.
fn address(label: &str) -> String {
    format!("{label}@example.test")
}

/// The roster row [`lay`] appends: a default row (no `dir`) whose address is on record.
#[must_use]
pub(crate) fn default_row(label: &str) -> String {
    format!(
        "[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\nemail = \"{}\"\n\n",
        address(label)
    )
}

/// Claude Code's profile `profile` (`~/.claude.json`, or `{}` where none is laid yet) signed in
/// as `email`, as `/login` leaves it; every other field kept.
///
/// # Errors
/// `profile` is not a JSON object.
pub(crate) fn signed_in_profile(profile: &str, email: &str) -> Result<String, String> {
    let mut value: aterm_json::Value =
        aterm_json::from_str(profile).map_err(|e| format!("profile: {e}"))?;
    let object = value.as_object_mut().ok_or("profile: not a JSON object")?;
    let account = aterm_json::from_str::<aterm_json::Value>(&format!(
        "{{\"accountUuid\":\"uuid-heal\",\"emailAddress\":\"{email}\",\
         \"organizationType\":\"claude_max\"}}"
    ))
    .map_err(|e| format!("profile: {e}"))?;
    object.insert("oauthAccount".into(), account);
    aterm_json::to_string(&value).map_err(|e| format!("profile: {e}"))
}

/// The stored login file a [`Held::File`] default home keeps: a refresh token and no access
/// token, which Claude Code renews from.
#[must_use]
pub(crate) fn stored_login(label: &str) -> String {
    format!(
        "{{\"claudeAiOauth\":{{\"accessToken\":\"\",\"refreshToken\":\"lab-refresh-{label}\",\
         \"scopes\":[\"user:inference\",\"user:profile\"],\"subscriptionType\":\"max\"}}}}"
    )
}

/// The harness state directory of `world` (`<state>/harness/claude-harness`).
fn harness_state(world: &World) -> PathBuf {
    world
        .state()
        .join("harness")
        .join(aterm_agent::harness::cli::HARNESS)
}

/// Claude Code's roster in `world`.
fn roster_path(world: &World) -> PathBuf {
    world
        .cfg
        .join("aterm")
        .join(Cli::Claude.accounts().roster_file)
}

fn put(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        crate::util::create_private_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Lay side `side`'s heal-due default login in `world` at `now`, its login held as `held`
/// (module doc), after the lane's own rows are laid: the default row joins Claude Code's roster
/// as it stands. Returns the default row's label.
///
/// # Errors
/// A file could not be read or written, or the store refused a record.
pub(crate) fn lay(world: &World, side: char, held: Held, now: i64) -> Result<String, String> {
    let label = default_label(side);
    let roster = roster_path(world);
    let mut rows = match std::fs::read_to_string(&roster) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", roster.display())),
    };
    rows.push_str(&default_row(&label));
    put(&roster, &rows)?;

    let profile_path = world.home.join(".claude.json");
    let profile = match std::fs::read_to_string(&profile_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
        Err(e) => return Err(format!("{}: {e}", profile_path.display())),
    };
    put(
        &profile_path,
        &signed_in_profile(&profile, &address(&label))?,
    )?;
    match held {
        Held::File => put(
            &world.home.join(".claude/.credentials.json"),
            &stored_login(&label),
        )?,
        Held::Keychain => crate::util::create_private_dir_all(&world.home.join(".claude"))
            .map_err(|e| e.to_string())?,
    }

    let store = Store::at(&harness_state(world));
    let mut note = None;
    let signed_out_at = now - SIGNED_OUT_AGO;
    store
        .transact(signed_out_at, &mut note, |state| {
            Ok(accounts::note_auth(
                state,
                &label,
                Auth::SignedOut,
                signed_out_at,
            ))
        })
        .map_err(|e| format!("claude store: {e}"))?;
    let moved_at = now - MOVED_AGO;
    let moved = Event::new(moved_at, Kind::Displaced, &was_label(side))
        .with_to(&label)
        .because(&format!("default login now {label}"));
    store
        .transact(moved_at, &mut note, |_| Ok(vec![moved]))
        .map_err(|e| format!("claude store: {e}"))?;
    Ok(label)
}

/// How many runs of the stand-in `claude` the run log `ran` notes (one line each, its CLI's name
/// first: `local_signins::standin_main`).
#[must_use]
pub(crate) fn claude_runs(ran: &str) -> usize {
    ran.lines()
        .filter(|line| line.split(' ').next() == Some("claude"))
        .count()
}

/// How many probe records (`"k":"probe"`) the journal spool text `spool` holds.
#[must_use]
pub(crate) fn probe_records(spool: &str) -> usize {
    spool
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| v["k"].as_str() == Some("probe"))
        .count()
}

/// What [`judge`] read after an upkeep pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Judged {
    /// Runs of the stand-in `claude` the world's run log notes.
    pub(crate) claude_runs: usize,
    /// Probe records in the world's journal spool.
    pub(crate) probes: usize,
    /// The default row's auth word on record.
    pub(crate) auth: Auth,
    /// Whether the heal is still due.
    pub(crate) due: bool,
}

impl Judged {
    /// Whether the pass did what a default login held as `held` calls for: nothing ran and
    /// nothing was probed; a stored login healed the row (signed in, nothing due) where the
    /// default login is that file, and otherwise (a keychain-held login, or any file on macOS)
    /// left its sign-out standing (still due).
    #[must_use]
    pub(crate) fn pass(&self, held: Held) -> bool {
        let answered = if heals(held) {
            self.auth == Auth::Ok && !self.due
        } else {
            self.auth == Auth::SignedOut && self.due
        };
        self.claude_runs == 0 && self.probes == 0 && answered
    }

    /// The record's detail words.
    #[must_use]
    pub(crate) fn words(&self) -> String {
        format!(
            "stand-in claude runs={}; probe records={}; default row auth={:?}, heal still due={}",
            self.claude_runs, self.probes, self.auth, self.due
        )
    }
}

/// Read `world` after an upkeep pass at `now` (module doc) for the default row `label`, counting
/// only the stand-in runs noted after the first `noted` lines of the run log (the lane's own
/// stand-in controls run each CLI once before any pass).
///
/// # Errors
/// The store cannot be read.
pub(crate) fn judge(world: &World, label: &str, now: i64, noted: usize) -> Result<Judged, String> {
    let ran = crate::util::read_regular_text(&world.dir.join(RAN)).unwrap_or_default();
    let spool_dir = world.state().join("alab");
    let probes = ["spool.jsonl", "spool.taken", "outbox.jsonl"]
        .iter()
        .filter_map(|name| crate::util::read_regular_text(&spool_dir.join(name)).ok())
        .map(|text| probe_records(&text))
        .sum();
    let state_dir = harness_state(world);
    let (state, _) = Store::at(&state_dir)
        .load(now)
        .map_err(|e| format!("claude store: {e}"))?;
    let due = aterm_agent::harness::accounts_sync::default_login_heal_due_for_lab(
        &world.home,
        &state_dir,
        &roster_path(world),
        now,
    )
    .is_some();
    Ok(Judged {
        claude_runs: claude_runs(&ran.lines().skip(noted).collect::<Vec<_>>().join("\n")),
        probes,
        auth: state.account(label).auth,
        due,
    })
}

#[cfg(test)]
#[path = "local_signins_heal_tests.rs"]
mod tests;
