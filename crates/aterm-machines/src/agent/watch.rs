// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Detached `screen-watch` lock watcher ([`arm`], [`run`], [`watch`], [`disarm`], [`Armed`]).
//! Spawns via [`System::spawn_detached`] and locks after [`QUIET`] idle or [`LIFE`] elapsed.

use super::{Layout, state};
use crate::model::User;
use crate::port::{Cmd, Ran, System};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How long no signed request may reach the host before the watcher locks.
pub const QUIET: Duration = Duration::from_secs(60);

/// The longest a watcher watches.
pub const LIFE: Duration = Duration::from_secs(4 * 3600);

/// The pause between two readings of the heartbeat.
pub const POLL: Duration = Duration::from_secs(2);

/// How long `arm` waits for the watcher it started to write `<id>.watching`: a hang detector,
/// not a startup latency requirement on a loaded host.
pub const START_HANG: Duration = Duration::from_secs(60);

/// The pause between two looks for `<id>.watching`.
const START_POLL: Duration = Duration::from_millis(100);

/// The inner readback's whole hang detector, plus a minute for the command to start and exit.
const LOCK_BUDGET: Duration = Duration::from_secs(super::lock::LOCK_HANG.as_secs() + 60);

/// Time as `arm` and the watcher see it, so a test watches for hours without sleeping.
pub trait Clock {
    /// Time since some fixed start; it never goes back.
    fn elapsed(&self) -> Duration;
    /// Wait `d`.
    fn pause(&self, d: Duration);
}

/// The monotonic clock.
#[derive(Debug)]
pub struct WallClock {
    start: Instant,
}

impl WallClock {
    /// A clock starting now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for WallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for WallClock {
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn pause(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// How long the watcher waits: [`QUIET`], [`LIFE`] and [`POLL`] outside tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// No signed request reached the host for this long: lock.
    pub quiet: Duration,
    /// Watched this long: lock.
    pub life: Duration,
    /// The pause between two readings.
    pub poll: Duration,
}

impl Bounds {
    /// The bounds a host's watcher runs with.
    pub const LIVE: Self = Self {
        quiet: QUIET,
        life: LIFE,
        poll: POLL,
    };
}

/// `<id>.armed`: whose session the watcher locks, and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Armed {
    /// The account's uid (`gui/<uid>`, `launchctl asuser <uid>`).
    pub uid: u32,
    /// The account's name.
    pub user: String,
    /// When it was armed, in Unix seconds.
    pub armed: u64,
}

/// `aterm machines agent screen-watch <args>`.
pub fn main(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    args: &[&str],
    out: &mut dyn Write,
) -> ExitCode {
    match args {
        ["arm", "--uid", uid, "--user", user] => arm(
            sys,
            layout,
            exe,
            (*uid, *user),
            std::process::id(),
            &WallClock::new(),
            out,
        ),
        ["run", id] => run(
            sys,
            layout,
            id,
            std::process::id(),
            &WallClock::new(),
            Bounds::LIVE,
            out,
        ),
        ["disarm", id] => disarm(sys, layout, id, out),
        _ => super::refuse(
            out,
            &format!(
                "`screen-watch {}` is not a screen-watch verb (arm --uid <uid> --user <name>, \
                 run <id>, disarm <id>)",
                args.join(" ")
            ),
        ),
    }
}

/// `state/screen-watch/<id>.<ext>`.
fn file(layout: &Layout, id: &str, ext: &str) -> PathBuf {
    layout.screen_watch().join(format!("{id}.{ext}"))
}

/// Print `line` and answer `code`.
fn say(out: &mut dyn Write, line: &str, code: ExitCode) -> ExitCode {
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    code
}

/// `not armed: <why>`, status 1.
fn not_armed(out: &mut dyn Write, why: &str) -> ExitCode {
    say(out, &format!("not armed: {why}"), ExitCode::FAILURE)
}

/// Whether `path` is root's alone.
fn root_only(sys: &dyn System, path: &Path, dir: bool) -> Result<(), String> {
    let st = sys
        .stat(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if st.symlink || st.dir != dir || st.uid != 0 || st.mode & 0o077 != 0 {
        return Err(format!(
            "{} is not root's alone (uid {}, mode {:o})",
            path.display(),
            st.uid,
            st.mode
        ));
    }
    Ok(())
}

/// Read `<id>.armed`.
fn read_armed(sys: &dyn System, layout: &Layout, id: &str) -> Result<Armed, String> {
    let path = file(layout, id, "armed");
    let bytes = sys
        .read_file(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    aterm_toml::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Remove every file of arm `id`, its `.armed` first.
///
/// # Errors
/// The first removal that failed; the others are still tried.
fn forget(sys: &dyn System, layout: &Layout, id: &str) -> std::io::Result<()> {
    let mut first = Ok(());
    for ext in ["armed", "watching", "ended", "log"] {
        if let Err(e) = sys.remove(&file(layout, id, ext))
            && first.is_ok()
        {
            first = Err(e);
        }
    }
    first
}

/// Retire other same-account arms after the new watcher acknowledges startup.
fn prune(sys: &dyn System, layout: &Layout, uid: u32, new: &str) -> Vec<String> {
    let Ok(names) = sys.list_dir(&layout.screen_watch()) else {
        return Vec::new();
    };
    let mut ids: Vec<&str> = names
        .iter()
        .filter_map(|name| name.split_once('.').map(|(id, _)| id))
        .filter(|id| super::is_run_id(id) && *id != new)
        .collect();
    ids.dedup();
    let mut replaced = Vec::new();
    for id in ids {
        // Unknown records and other accounts stay, regardless of a PID reading.
        if let Ok(armed) = read_armed(sys, layout, id)
            && armed.uid == uid
        {
            let _ = forget(sys, layout, id);
            replaced.push(id.to_owned());
        }
    }
    replaced
}

/// `screen-watch arm`: spawn the watcher ([`System::spawn_detached`]) and wait [`START_HANG`].
pub fn arm(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    (uid, name): (&str, &str),
    pid: u32,
    clock: &dyn Clock,
    out: &mut dyn Write,
) -> ExitCode {
    if sys.euid() != 0 {
        return super::refuse(
            out,
            "screen-watch arm runs as root, as a signed exec (aterm machines screen sends it)",
        );
    }
    let Some(uid) = crate::parse_uint::<u32>(uid) else {
        return super::refuse(out, &format!("{uid:?} is not a uid"));
    };
    let user = match crate::facts::user(sys, name) {
        Ok(user) => user,
        Err(why) => return not_armed(out, &why),
    };
    if user.uid != uid {
        return not_armed(
            out,
            &format!("account {} is uid {}, not {uid}", user.name, user.uid),
        );
    }
    let dir = layout.screen_watch();
    let made = sys
        .create_dir_all(&dir, 0o700)
        .and_then(|()| sys.chown(&dir, 0, 0))
        .map_err(|e| format!("{}: {e}", dir.display()))
        .and_then(|()| root_only(sys, &dir, true));
    if let Err(why) = made {
        return not_armed(out, &why);
    }
    let id = format!("{}-{pid}", sys.now());
    let armed = Armed {
        uid,
        user: user.name,
        armed: sys.now(),
    };
    let written = aterm_toml::to_string(&armed)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            sys.write_file(&file(layout, &id, "armed"), text.as_bytes(), 0o600)
                .map_err(|e| format!("{}: {e}", file(layout, &id, "armed").display()))
        });
    if let Err(why) = written {
        let _ = forget(sys, layout, &id);
        return not_armed(out, &why);
    }
    let watcher = Cmd::new(exe).args(["machines", "agent", "screen-watch", "run", &id]);
    if let Err(e) = sys.spawn_detached(&watcher, &file(layout, &id, "log")) {
        let _ = forget(sys, layout, &id);
        return not_armed(out, &format!("the watcher did not start: {e}"));
    }
    let start = clock.elapsed();
    while sys.read_file(&file(layout, &id, "watching")).is_err() {
        if clock.elapsed().saturating_sub(start) >= START_HANG {
            let log = sys
                .read_file(&file(layout, &id, "log"))
                .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
                .unwrap_or_default();
            let last = log.lines().last().unwrap_or("its log is empty").to_owned();
            let _ = forget(sys, layout, &id);
            return not_armed(
                out,
                &format!(
                    "the watcher did not start watching within {}s ({last})",
                    START_HANG.as_secs()
                ),
            );
        }
        clock.pause(START_POLL);
    }
    let replaced = prune(sys, layout, uid, &id);
    let line = if replaced.is_empty() {
        format!("armed {id}")
    } else {
        format!("armed {id} (replaced {})", replaced.join(" "))
    };
    say(out, &line, ExitCode::SUCCESS)
}

/// What a watch ended with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The arm went (`disarm`, or a later `arm` for the same account): nothing was locked.
    Disarmed,
    /// It ended of its own accord: the line for `<id>.ended`.
    Done(String),
}

/// The heartbeat: the run the run lock names (one in progress).
fn heartbeat(sys: &dyn System, layout: &Layout) -> [Option<String>; 3] {
    [
        state::holder(sys, layout).map(|h| h.run),
        state::last_run(sys, layout),
        state::last_busy(sys, layout),
    ]
}

/// Whether arm `id` is gone.
fn gone(sys: &dyn System, layout: &Layout, id: &str) -> bool {
    matches!(
        sys.stat(&file(layout, id, "armed")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// Probe `user`'s login session with `launchctl print gui/<uid>`.
#[must_use]
pub fn session_cmd(user: &User) -> Cmd {
    Cmd::new(crate::items::person::LAUNCHCTL).args(["print", &format!("gui/{}", user.uid)])
}

/// Build the `lock-screen` command for `user`'s login session.
#[must_use]
pub fn lock_cmd(layout: &Layout, user: &User) -> Cmd {
    Cmd::new(layout.exe_link())
        .args(["machines", "agent", "lock-screen"])
        .as_user(user)
        .in_session()
        .budget(LOCK_BUDGET)
}

/// What a `lock-screen` that did not exit 0 said: its own `not locked: <why>`, else how it ended.
fn refusal(ran: &Ran) -> String {
    let out = ran.out();
    match out.lines().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => line
            .strip_prefix("not locked: ")
            .map_or_else(|| format!("{}: {line}", ran.describe()), str::to_owned),
        None => ran.describe(),
    }
}

/// Watch `id` until [`Ended::Disarmed`], [`heartbeat`] quiet, or `bounds.life`, then [`lock_cmd`].
pub fn watch(
    sys: &dyn System,
    layout: &Layout,
    id: &str,
    user: &User,
    clock: &dyn Clock,
    bounds: Bounds,
) -> Ended {
    let start = clock.elapsed();
    let mut last = heartbeat(sys, layout);
    let mut beat = start;
    let why = loop {
        if gone(sys, layout, id) {
            return Ended::Disarmed;
        }
        let now = heartbeat(sys, layout);
        if now != last {
            last = now;
            beat = clock.elapsed();
        }
        let at = clock.elapsed();
        let quiet = at.saturating_sub(beat);
        if quiet >= bounds.quiet {
            break format!(
                "no signed request reached the host for {}s",
                quiet.as_secs()
            );
        }
        let watched = at.saturating_sub(start);
        if watched >= bounds.life {
            break format!(
                "it had watched for {}s, the longest a watch lasts",
                watched.as_secs()
            );
        }
        clock.pause(bounds.poll);
    };
    if gone(sys, layout, id) {
        return Ended::Disarmed;
    }
    let unread = match sys.run(&session_cmd(user)) {
        Ok(ran) if ran.success() => String::new(),
        Ok(ran) => format!(
            " (whether it has a login session could not be read: {})",
            ran.describe()
        ),
        Err(e) => format!(" (whether it has a login session could not be read: {e})"),
    };
    Ended::Done(match sys.run(&lock_cmd(layout, user)) {
        Ok(ran) if ran.success() => format!("locked {}'s session: {why}{unread}", user.name),
        Ok(ran) => format!("not locked: {why}{unread}; lock-screen: {}", refusal(&ran)),
        Err(e) => format!("not locked: {why}{unread}; lock-screen did not run: {e}"),
    })
}

/// The checks before the watcher trusts arm `id`.
fn accept(sys: &dyn System, layout: &Layout, id: &str) -> Result<Armed, String> {
    root_only(sys, &layout.screen_watch(), true)?;
    root_only(sys, &file(layout, id, "armed"), false)?;
    read_armed(sys, layout, id)
}

/// `screen-watch run <id>`: the watcher `arm` started ([`System::ignore_hangups`], [`watch`]).
pub fn run(
    sys: &dyn System,
    layout: &Layout,
    id: &str,
    pid: u32,
    clock: &dyn Clock,
    bounds: Bounds,
    out: &mut dyn Write,
) -> ExitCode {
    if sys.euid() != 0 {
        return super::refuse(
            out,
            "screen-watch run runs as root, started by screen-watch arm",
        );
    }
    if !super::is_run_id(id) {
        return super::refuse(out, &format!("{id:?} is not a screen-watch id"));
    }
    let armed = match accept(sys, layout, id) {
        Ok(armed) => armed,
        Err(why) => return super::refuse(out, &why),
    };
    let user = match crate::facts::user(sys, &armed.user) {
        Ok(user) if user.uid == armed.uid => user,
        Ok(user) => {
            return super::refuse(
                out,
                &format!(
                    "account {} is uid {} now, not {}",
                    user.name, user.uid, armed.uid
                ),
            );
        }
        Err(why) => return super::refuse(out, &why),
    };
    sys.ignore_hangups();
    if let Err(e) = sys.write_file(
        &file(layout, id, "watching"),
        format!("{pid}\n").as_bytes(),
        0o600,
    ) {
        return super::refuse(out, &format!("{id}.watching: {e}"));
    }
    let _ = writeln!(out, "watching {id} for {} (uid {})", user.name, user.uid);
    let line = match watch(sys, layout, id, &user, clock, bounds) {
        Ended::Disarmed => return say(out, &format!("disarmed {id}"), ExitCode::SUCCESS),
        Ended::Done(line) => line,
    };
    let _ = sys.write_file(
        &file(layout, id, "ended"),
        format!("{line}\n").as_bytes(),
        0o600,
    );
    let _ = sys.remove(&file(layout, id, "armed"));
    let _ = sys.remove(&file(layout, id, "watching"));
    say(out, &line, ExitCode::SUCCESS)
}

/// `screen-watch disarm <id>`: remove arm `id` and everything its watcher left ([`forget`]).
pub fn disarm(sys: &dyn System, layout: &Layout, id: &str, out: &mut dyn Write) -> ExitCode {
    if sys.euid() != 0 {
        return super::refuse(
            out,
            "screen-watch disarm runs as root, as a signed exec (aterm machines screen sends it)",
        );
    }
    if !super::is_run_id(id) {
        return super::refuse(out, &format!("{id:?} is not a screen-watch id"));
    }
    let ended = sys
        .read_file(&file(layout, id, "ended"))
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
        .ok()
        .filter(|l| !l.is_empty());
    let armed = sys.stat(&file(layout, id, "armed")).is_ok();
    if let Err(e) = forget(sys, layout, id) {
        return say(out, &format!("not disarmed: {e}"), ExitCode::FAILURE);
    }
    let line = match (ended, armed) {
        (Some(ended), _) => format!("disarmed {id} (it had ended: {ended})"),
        (None, true) => format!("disarmed {id}"),
        (None, false) => format!("disarmed {id} (nothing was armed)"),
    };
    say(out, &line, ExitCode::SUCCESS)
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
