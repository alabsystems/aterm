// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `screen-watch` (design §6): the part of `screen`'s lock that outlives the controller.
//!
//! `screen` locks a session the admin logged in to over its relay once the walk ends or is
//! stopped (`controller::screen`). A controller that is killed (`SIGKILL`), crashes, loses
//! power or stays cut off from the host never sends that lock, and the Mac stays logged in and
//! unlocked at its display. So when `screen` finds nobody logged in, it arms a watcher on the
//! host before it relays anything, and disarms it after its own lock:
//!
//! | verb | what |
//! |---|---|
//! | `screen-watch arm --uid <uid> --user <name>` | records the arm (`<id>.armed`), starts `screen-watch run <id>` detached, and prints `armed <id>` once the watcher watches ([`arm`]) |
//! | `screen-watch run <id>` | the watcher, started by `arm` alone ([`run`], [`watch`]) |
//! | `screen-watch disarm <id>` | removes the arm and what its watcher left; a watcher whose arm is gone ends without locking ([`disarm`]) |
//!
//! All three run as root. `arm` and `disarm` come as signed `exec` envelopes like every other
//! request `screen` makes, so nothing here adds a way to root, and all the watcher can do is
//! lock one account's login session. It holds no run lock, so the walk's own requests are never
//! `busy` because of it. The watcher leads a session of its own ([`System::spawn_detached`]), so
//! it outlives the run that started it, and it ignores hang-ups.
//!
//! The heartbeat is the agent's own record of signed requests, which only root writes: a run's
//! start is `state/run.lock` naming it, its end `state/last_run` naming it, and a request
//! refused because another run holds the host is `state/last_busy` naming it (a walk's
//! requests are all refused for as long as another command's `apply` lasts). A walk makes a
//! signed request every few seconds (each re-probe, each reading of the session), so once none
//! of the three has changed for [`QUIET`], or the watcher has watched for [`LIFE`], the
//! controller is taken to be gone. A run in progress does not hold the lock off: one that hung
//! would hold it off for good. The watcher probes the login session (`launchctl print
//! gui/<uid>`) and attempts the agent's `lock-screen` even if that reading failed or was
//! signaled: no failed reading establishes absence. The existing lock reports an absent
//! session through its own refusal. The watcher writes what happened to `<id>.ended` and ends. A Mac that
//! loses power needs nothing: it comes back with no session, or with auto-login's own.
//!
//! `state/screen-watch/` (root:wheel 0700) holds, for each arm `<id>` (`<unix seconds>-<pid of
//! the arm>`):
//!
//! * `<id>.armed` (0600): the account and when it was armed ([`Armed`]); the watcher watches
//!   while it is there;
//! * `<id>.watching` (0600): the watcher's pid, written once it watches;
//! * `<id>.log` (0600): the watcher's standard output and error;
//! * `<id>.ended` (0600): the one line the watcher ended with, when it ended of its own accord.
//!
//! One watch per account once startup is acknowledged: only then does `arm` remove other
//! arms whose parsed uid matches (their watchers then end without locking). Unknown records,
//! other accounts and leftover diagnostics stay. A failed replacement leaves the prior watch armed.

use super::{Layout, state};
use crate::model::User;
use crate::port::{Cmd, Ran, System};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How long no signed request may reach the host (no run starting or ending, none refused
/// `busy`) before the watcher takes the controller to be gone. A walk's requests come one
/// every few seconds, its pause between re-probes (`controller::screen::POLL`, 2 s) plus a
/// request's own time, so this is a hang detector over the walk's gaps, never a latency
/// budget. The longest come while a login over the relay keeps the host busy, well under this
/// but not by the margin of a step's wait (design §11); a walk that sees a gap of half this
/// arms the watch again (`controller::screen`), in case it locked and ended meanwhile.
pub const QUIET: Duration = Duration::from_secs(60);

/// The longest a watcher watches. Each of a walk's waits ends at its hang detector
/// (`controller::screen::PERSON_HANG`, 15 minutes), so a walk of a dozen steps ends well
/// before this; a watcher that reaches it locks and ends.
pub const LIFE: Duration = Duration::from_secs(4 * 3600);

/// The pause between two readings of the heartbeat.
pub const POLL: Duration = Duration::from_secs(2);

/// How long `arm` waits for the watcher it started to write `<id>.watching`: a hang detector.
pub const START_HANG: Duration = Duration::from_secs(10);

/// The pause between two looks for `<id>.watching`.
const START_POLL: Duration = Duration::from_millis(100);

/// The bound on the watcher's `lock-screen` (which reads the lock back for at most
/// `lock::LOCK_HANG`).
const LOCK_BUDGET: Duration = Duration::from_secs(30);

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

/// Whether `path` is root's alone: owned by root, not a link, of the kind asked for, and
/// neither readable nor writable by anyone else.
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

/// Remove every file of arm `id`, its `.armed` first: the watcher reads that one, and ends
/// without locking once it is gone.
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

/// After the new watcher acknowledges startup: remove only other arms whose parsed uid
/// matches `uid`. Unknown records, other accounts and leftover diagnostics stay: a failed
/// read or a PID probe does not establish that another watch may be retired. Returns the
/// same-account arms it replaced.
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
        // Only positive same-account ownership authorizes retiring another arm.
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

/// `screen-watch arm --uid <uid> --user <name>`: check that `user` is an account with that
/// uid, record the arm, start the watcher detached ([`System::spawn_detached`], its output to
/// `<id>.log`), and wait until it writes `<id>.watching` ([`START_HANG`]). Prints `armed <id>`
/// (with the arms it replaced) and exits 0, or `not armed: <why>` and exits 1, with no arm
/// left behind. `pid` is this process's own (the id names it).
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
    let Ok(uid) = uid.parse::<u32>() else {
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
    // Retire the prior watch only after its replacement has acknowledged startup.
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

/// The heartbeat: the run the run lock names (one in progress), the newest finished run, and
/// the newest signed request refused `busy`. Any one changing is a signed request reaching the
/// host: a run starting or ending, or one refused while another command's run holds the host.
/// A run still in progress is not one: a run that hung would hold the lock off for good.
fn heartbeat(sys: &dyn System, layout: &Layout) -> [Option<String>; 3] {
    [
        state::holder(sys, layout).map(|h| h.run),
        state::last_run(sys, layout),
        state::last_busy(sys, layout),
    ]
}

/// Whether arm `id` is gone. Only its absence counts: a reading that fails otherwise keeps
/// the watch going.
fn gone(sys: &dyn System, layout: &Layout, id: &str) -> bool {
    matches!(
        sys.stat(&file(layout, id, "armed")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// Probe `user`'s login session with `launchctl print gui/<uid>`. Success reads it present;
/// non-success alone cannot establish absence, so the watcher still attempts the lock.
#[must_use]
pub fn session_cmd(user: &User) -> Cmd {
    Cmd::new(crate::items::person::LAUNCHCTL).args(["print", &format!("gui/{}", user.uid)])
}

/// The lock: the agent's own `lock-screen` (through the stable `bin/aterm`, which an upgrade
/// while the watcher runs leaves in place) as `user` in `user`'s login session, the way
/// `controller::screen::lock_cmd` runs it (`launchctl asuser`, `sudo -n -u`, `env -i`).
#[must_use]
pub fn lock_cmd(layout: &Layout, user: &User) -> Cmd {
    Cmd::new(layout.exe_link())
        .args(["machines", "agent", "lock-screen"])
        .as_user(user)
        .in_session()
        .budget(LOCK_BUDGET)
}

/// What a `lock-screen` that did not exit 0 said: its own `not locked: <why>`, else how it
/// ended.
fn refusal(ran: &Ran) -> String {
    let out = ran.out();
    match out.lines().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => line
            .strip_prefix("not locked: ")
            .map_or_else(|| format!("{}: {line}", ran.describe()), str::to_owned),
        None => ran.describe(),
    }
}

/// Watch arm `id` for `user` until it is gone ([`Ended::Disarmed`]), or until no signed
/// request has reached the host ([`heartbeat`]) for `bounds.quiet`, or it has watched for
/// `bounds.life`. Then read
/// the login session and attempt its lock ([`lock_cmd`]); a failed or signaled probe is
/// unknown, never positive absence. Its diagnostic is retained beside the lock's outcome.
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
    // A disarm that came with the last reading still wins.
    if gone(sys, layout, id) {
        return Ended::Disarmed;
    }
    let unread = match sys.run(&session_cmd(user)) {
        Ok(ran) if ran.success() => String::new(),
        // A failed or signaled query cannot establish that the session is absent.
        Ok(ran) => format!(
            " (whether it has a login session could not be read: {})",
            ran.describe()
        ),
        // Not known to be absent: the safe side is to try the lock.
        Err(e) => format!(" (whether it has a login session could not be read: {e})"),
    };
    Ended::Done(match sys.run(&lock_cmd(layout, user)) {
        Ok(ran) if ran.success() => format!("locked {}'s session: {why}{unread}", user.name),
        Ok(ran) => format!("not locked: {why}{unread}; lock-screen: {}", refusal(&ran)),
        Err(e) => format!("not locked: {why}{unread}; lock-screen did not run: {e}"),
    })
}

/// The checks before the watcher trusts arm `id`: `state/screen-watch/` and `<id>.armed` are
/// root's alone, and the arm parses.
fn accept(sys: &dyn System, layout: &Layout, id: &str) -> Result<Armed, String> {
    root_only(sys, &layout.screen_watch(), true)?;
    root_only(sys, &file(layout, id, "armed"), false)?;
    read_armed(sys, layout, id)
}

/// `screen-watch run <id>`: the watcher `arm` started. It refuses an arm that is not root's
/// own or names an account that no longer has that uid; otherwise it ignores hang-ups
/// ([`System::ignore_hangups`]), writes `<id>.watching` (`pid`, this process's own), and
/// [`watch`]es. A watch that ended of its own accord leaves its line in `<id>.ended` and
/// removes the arm. Everything it prints goes to `<id>.log`.
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
/// Prints `disarmed <id>`, with the watcher's last line when it had ended of its own accord,
/// or `nothing was armed` when no arm was there (a later `arm` replaced it); `not disarmed:
/// <why>` and status 1 when a file stayed.
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
mod tests {
    use super::*;
    use crate::port::{Fake, Stat};
    use std::sync::Mutex;

    const ID: &str = "1800000000-4242";
    const DIR: &str = "/A/state/screen-watch";

    fn layout() -> Layout {
        Layout { root: "/A".into() }
    }

    fn admin() -> User {
        User {
            name: "admin".into(),
            uid: 501,
            gid: 20,
            home: "/Users//admin".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        }
    }

    /// A root fake that knows the admin (uid 501) as `dscl` printed it in the VM, with
    /// `state/screen-watch/` root's alone.
    fn host() -> Fake {
        let fake = Fake::root();
        fake.on(
            "/usr/bin/dscl . -read /Groups/admin GroupMembership",
            Ran::ok("GroupMembership: root _mbsetupuser admin\n"),
        );
        fake.on(
            "/usr/bin/dscl . -read /Users//admin UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
            Ran::ok(
                "NFSHomeDirectory: /Users//admin\nPrimaryGroupID: 20\nUniqueID: 501\nUserShell: /bin/zsh\n",
            ),
        );
        fake.stat_of(
            DIR,
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        fake
    }

    fn path(id: &str, ext: &str) -> String {
        format!("{DIR}/{id}.{ext}")
    }

    /// Seed `path` 0600, as the agent writes every file of a watch.
    fn put(fake: &Fake, path: &str, bytes: &[u8]) {
        fake.write_file(Path::new(path), bytes, 0o600).unwrap();
    }

    /// A clock that moves only when paused, and runs `each` with the new time after every
    /// pause: what the host does meanwhile (a signed run, a disarm, the watcher starting).
    struct Script<'a> {
        at: Mutex<Duration>,
        each: Box<dyn Fn(Duration) + 'a>,
    }

    impl<'a> Script<'a> {
        fn new(each: impl Fn(Duration) + 'a) -> Self {
            Self {
                at: Mutex::new(Duration::ZERO),
                each: Box::new(each),
            }
        }
    }

    impl Clock for Script<'_> {
        fn elapsed(&self) -> Duration {
            *self.at.lock().unwrap()
        }

        fn pause(&self, d: Duration) {
            let now = {
                let mut at = self.at.lock().unwrap();
                *at += d;
                *at
            };
            // Past the longest watch with room to spare: a loop that never ends fails here.
            assert!(now <= LIFE * 2, "still waiting after {}s", now.as_secs());
            (self.each)(now);
        }
    }

    fn arm_as(fake: &Fake, uid: &str, user: &str, clock: &dyn Clock) -> (ExitCode, String) {
        let mut out = Vec::new();
        let code = arm(
            fake,
            &layout(),
            Path::new("/x"),
            (uid, user),
            4242,
            clock,
            &mut out,
        );
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    /// The watcher writing `<id>.watching` at its first pause, as a live one does once its
    /// checks pass.
    fn starts(fake: &Fake) -> Script<'_> {
        Script::new(move |_| {
            fake.file(&path(ID, "watching"), b"777\n");
        })
    }

    #[test]
    fn arm_checks_the_account_records_the_arm_and_starts_the_watcher_detached() {
        let fake = host();
        let (code, out) = arm_as(&fake, "501", "admin", &starts(&fake));
        assert_eq!(out, format!("armed {ID}\n"));
        assert_eq!(code, ExitCode::SUCCESS);
        let calls: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|c| !c.contains("dscl"))
            .collect();
        assert_eq!(
            calls,
            [
                format!("mkdir {DIR} 700"),
                format!("chown 0:0 {DIR}"),
                format!("write {} 600", path(ID, "armed")),
                format!(
                    "detach /x machines agent screen-watch run {ID} >> {}",
                    path(ID, "log")
                ),
            ]
        );
        let armed: Armed = aterm_toml::from_str(&String::from_utf8_lossy(
            &fake.written(&path(ID, "armed")).unwrap(),
        ))
        .unwrap();
        assert_eq!(
            armed,
            Armed {
                uid: 501,
                user: "admin".into(),
                armed: 1_800_000_000,
            }
        );
        assert_eq!(fake.mode_of(&path(ID, "armed")), Some(0o600));
    }

    #[test]
    fn arm_refuses_an_account_whose_uid_differs_or_that_does_not_exist_and_arms_nothing() {
        let fake = host();
        let (code, out) = arm_as(&fake, "502", "admin", &starts(&fake));
        assert_eq!(out, "not armed: account admin is uid 501, not 502\n");
        assert_eq!(code, ExitCode::FAILURE);
        let fake = host();
        fake.on(
            "/usr/bin/dscl . -read /Users//bob UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
            Ran::exit(56, "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)\n"),
        );
        let (code, out) = arm_as(&fake, "502", "bob", &starts(&fake));
        assert_eq!(out, "not armed: no account bob\n");
        assert_eq!(code, ExitCode::FAILURE);
        // Not a number, or not root: refused before anything is read.
        let fake = host();
        let (code, out) = arm_as(&fake, "admin", "admin", &starts(&fake));
        assert!(out.starts_with("refused: "), "{out}");
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        let mut fake = host();
        fake.euid = 501;
        let (code, _) = arm_as(&fake, "501", "admin", &starts(&fake));
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        for fake in [host(), fake] {
            assert!(
                !fake
                    .calls()
                    .iter()
                    .any(|c| c.starts_with("write") || c.starts_with("detach")),
                "{:?}",
                fake.calls()
            );
        }
    }

    #[test]
    fn a_watcher_that_never_starts_watching_leaves_no_arm_behind() {
        let fake = host();
        fake.file(&path(ID, "log"), b"refused: the account changed\n");
        let clock = Script::new(|_| {});
        let (code, out) = arm_as(&fake, "501", "admin", &clock);
        assert_eq!(
            out,
            "not armed: the watcher did not start watching within 10s (refused: the account \
             changed)\n"
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(clock.elapsed() >= START_HANG);
        assert!(fake.written(&path(ID, "armed")).is_none());
        assert!(
            fake.calls()
                .contains(&format!("remove {}", path(ID, "armed")))
        );
    }

    #[test]
    fn a_second_arm_for_the_same_account_replaces_the_first_and_keeps_another_accounts() {
        let fake = host();
        let old = "1799999000-7";
        let other = "1799999000-8";
        let ended = "1799990000-9";
        let unknown = "1799990000-10";
        let missing = "1799990000-11";
        fake.file(
            &path(old, "armed"),
            b"uid = 501\nuser = \"admin\"\narmed = 1799999000\n",
        )
        .file(&path(old, "watching"), b"7\n")
        .file(
            &path(other, "armed"),
            b"uid = 502\nuser = \"student\"\narmed = 1799999000\n",
        )
        .file(&path(other, "watching"), b"8\n")
        .file(&path(ended, "ended"), b"locked admin's session: ...\n")
        .file(&path(unknown, "armed"), b"unreadable ownership = [")
        .file(&path(unknown, "watching"), b"10\n")
        .file(&path(missing, "watching"), b"11\n")
        .alive(7)
        .alive(10)
        .alive(11);
        let (code, out) = arm_as(&fake, "501", "admin", &starts(&fake));
        assert_eq!(out, format!("armed {ID} (replaced {old})\n"));
        assert_eq!(code, ExitCode::SUCCESS);
        // Positive same-account replacement still works; every unknown or different
        // owner stays even if no live PID was observed for it.
        assert!(fake.written(&path(old, "armed")).is_none());
        assert!(fake.written(&path(other, "armed")).is_some());
        let removes: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("remove"))
            .collect();
        assert_eq!(removes[0], format!("remove {}", path(old, "armed")));
        assert!(removes.contains(&format!("remove {}", path(old, "armed"))));
        assert!(!removes.iter().any(|c| c.contains(other)), "{removes:?}");
        assert!(fake.written(&path(ended, "ended")).is_some());
        assert!(fake.written(&path(unknown, "armed")).is_some());
        assert!(fake.written(&path(missing, "watching")).is_some());
        for retained in [other, ended, unknown, missing] {
            assert!(
                !removes.iter().any(|c| c.contains(retained)),
                "arming one account must preserve unknown and different owners: {retained}: {removes:?}"
            );
        }
    }

    /// A replacement that never starts must leave the existing watch in charge.
    /// Successful replacement and cleanup are ordinary controls; the final outcome
    /// comes from the actual surviving old watch, not a duplicate admission predicate.
    #[test]
    fn an_arm_that_cannot_start_keeps_the_watch_it_would_replace() {
        let old = "1799999000-7";
        let seed_old = |fake: &Fake| {
            fake.file(
                &path(old, "armed"),
                b"uid = 501\nuser = \"admin\"\narmed = 1799999000\n",
            )
            .file(&path(old, "watching"), b"7\n")
            .alive(7);
        };
        let good = host();
        seed_old(&good);
        let (code, _) = arm_as(&good, "501", "admin", &starts(&good));
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(good.written(&path(ID, "armed")).is_some());
        assert!(good.written(&path(old, "armed")).is_none());

        let failed = host();
        seed_old(&failed);
        failed.on(&session_cmd(&admin()).display(), Ran::ok(""));
        failed.on(
            &lock_cmd(&layout(), &admin()).display(),
            Ran::ok("locked\n"),
        );
        let clock = Script::new(|_| {});
        let (code, out) = arm_as(&failed, "501", "admin", &clock);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(out.contains("did not start watching within 10s"), "{out}");
        assert!(failed.written(&path(ID, "armed")).is_none());
        let ended = watch(&failed, &layout(), old, &admin(), &clock, TEST);
        assert!(
            matches!(ended, Ended::Done(ref line) if line.starts_with("locked admin's session")),
            "a failed replacement must leave the old watcher able to lock: {ended:?}"
        );
        assert_eq!(locks(&failed), 1);
    }

    /// The heartbeat files as the agent writes them: a run in progress, or the newest
    /// finished run.
    fn runs(fake: &Fake, lock: Option<&str>, last: Option<&str>) {
        match lock {
            Some(run) => {
                fake.file(
                    "/A/state/run.lock",
                    format!("pid=10 run={run}\n").as_bytes(),
                );
            }
            None => {
                let _ = fake.remove(Path::new("/A/state/run.lock"));
            }
        }
        if let Some(run) = last {
            fake.file("/A/state/last_run", format!("{run}\n").as_bytes());
        }
    }

    /// A fake with arm [`ID`] for the admin in place, and the lock and session readings
    /// scripted as given.
    fn armed_host(session: Option<Ran>, lock: Option<Ran>) -> Fake {
        let fake = host();
        put(
            &fake,
            &path(ID, "armed"),
            b"uid = 501\nuser = \"admin\"\narmed = 1800000000\n",
        );
        if let Some(ran) = session {
            fake.on(&session_cmd(&admin()).display(), ran);
        }
        if let Some(ran) = lock {
            fake.on(&lock_cmd(&layout(), &admin()).display(), ran);
        }
        fake
    }

    const TEST: Bounds = Bounds {
        quiet: QUIET,
        life: LIFE,
        poll: POLL,
    };

    fn locks(fake: &Fake) -> usize {
        let lock = lock_cmd(&layout(), &admin()).display();
        fake.calls().iter().filter(|c| **c == lock).count()
    }

    #[test]
    fn the_watcher_waits_while_signed_runs_come_and_locks_once_none_has_for_the_quiet_time() {
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        // A walk: one signed run every 4 s, starting and ending, for 100 s; then nothing.
        let clock = Script::new(|t| {
            let s = t.as_secs();
            if s <= 100 && s % 4 == 0 {
                let run = format!("{}-{s}", 1_800_000_000 + s);
                if s % 8 == 0 {
                    runs(&fake, Some(&run), None);
                } else {
                    runs(&fake, None, Some(&run));
                }
            }
        });
        let ended = watch(&fake, &layout(), ID, &admin(), &clock, TEST);
        assert_eq!(
            ended,
            Ended::Done(
                "locked admin's session: no signed request reached the host for 60s".into()
            )
        );
        assert_eq!(locks(&fake), 1);
        // The last run ended at 100 s: the lock came one quiet time after it, not before.
        assert_eq!(clock.elapsed(), Duration::from_secs(160));
        // The lock is the admin's own `lock-screen`, in the admin's session.
        assert_eq!(
            lock_cmd(&layout(), &admin()).display(),
            "admin@gui$ /A/bin/aterm machines agent lock-screen"
        );
        assert!(lock_cmd(&layout(), &admin()).session);
    }

    #[test]
    fn a_run_still_in_progress_is_one_that_started_and_a_long_one_does_not_hold_off_the_lock() {
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        // A run starts at 10 s and never ends (the controller died with it in flight).
        let clock = Script::new(|t| {
            if t == Duration::from_secs(10) {
                runs(&fake, Some("1800000010-3"), None);
            }
        });
        let ended = watch(&fake, &layout(), ID, &admin(), &clock, TEST);
        assert!(matches!(ended, Ended::Done(l) if l.starts_with("locked")));
        assert_eq!(clock.elapsed(), Duration::from_secs(70));
    }

    #[test]
    fn requests_refused_busy_while_another_run_holds_the_host_keep_the_watch_waiting() {
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        // Another command's run holds the host from 10 s on and is still going at the end; the
        // walk's requests, one every 4 s, are all refused `busy` until 148 s; then nothing.
        let clock = Script::new(|t| {
            let s = t.as_secs();
            if s == 10 {
                runs(&fake, Some("1800000010-3"), None);
            }
            if (12..=148).contains(&s) && s % 4 == 0 {
                fake.file(
                    "/A/state/last_busy",
                    format!("{}-{s}\n", 1_800_000_000 + s).as_bytes(),
                );
            }
        });
        let ended = watch(&fake, &layout(), ID, &admin(), &clock, TEST);
        assert_eq!(
            ended,
            Ended::Done(
                "locked admin's session: no signed request reached the host for 60s".into()
            )
        );
        assert_eq!(locks(&fake), 1);
        // One quiet time after the last refusal, the other run still in progress.
        assert_eq!(clock.elapsed(), Duration::from_secs(208));
    }

    #[test]
    fn an_unreadable_session_reading_still_attempts_the_lock_and_keeps_why() {
        let present = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        let ended = watch(
            &present,
            &layout(),
            ID,
            &admin(),
            &Script::new(|_| {}),
            TEST,
        );
        assert!(matches!(ended, Ended::Done(ref line) if line.starts_with("locked")));
        assert_eq!(locks(&present), 1);
        let missing = armed_host(None, Some(Ran::ok("locked\n")));
        let ended = watch(
            &missing,
            &layout(),
            ID,
            &admin(),
            &Script::new(|_| {}),
            TEST,
        );
        assert!(matches!(ended, Ended::Done(ref line) if line.contains("could not be read")));
        assert_eq!(locks(&missing), 1);
        for reading in [
            Ran::exit(125, "Domain does not support specified action"),
            Ran::exit(13, "Permission denied"),
            Ran {
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        ] {
            let diagnostic = reading.describe();
            let fake = armed_host(Some(reading), Some(Ran::ok("locked\n")));
            let ended = watch(&fake, &layout(), ID, &admin(), &Script::new(|_| {}), TEST);
            assert_eq!(
                locks(&fake),
                1,
                "an unreadable session is not positive absence: {ended:?}"
            );
            assert!(
                matches!(ended, Ended::Done(ref line) if line.starts_with("locked") && line.contains("could not be read") && line.contains(&diagnostic)),
                "the lock outcome must retain the failed reading: {ended:?}"
            );
        }
        let reading = Ran::exit(125, "Domain does not support specified action");
        let diagnostic = reading.describe();
        let reason = "no login session";
        let absent = armed_host(
            Some(reading),
            Some(Ran {
                status: Some(1),
                stdout: format!("not locked: {reason}\n").into_bytes(),
                stderr: Vec::new(),
            }),
        );
        let ended = watch(&absent, &layout(), ID, &admin(), &Script::new(|_| {}), TEST);
        assert_eq!(locks(&absent), 1);
        assert!(
            matches!(ended, Ended::Done(ref line) if line.contains(reason) && line.contains(&diagnostic)),
            "{ended:?}"
        );
    }

    #[test]
    fn a_lock_that_fails_says_why() {
        let fake = armed_host(
            Some(Ran::ok("")),
            Some(Ran {
                status: Some(1),
                stdout: b"not locked: SACLockScreenImmediate answered 22\n".to_vec(),
                stderr: Vec::new(),
            }),
        );
        let ended = watch(&fake, &layout(), ID, &admin(), &Script::new(|_| {}), TEST);
        assert_eq!(
            ended,
            Ended::Done(
                "not locked: no signed request reached the host for 60s; lock-screen: \
                 SACLockScreenImmediate answered 22"
                    .into()
            )
        );
    }

    #[test]
    fn a_watch_disarmed_or_replaced_ends_without_reading_or_locking() {
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        let clock = Script::new(|t| {
            if t == Duration::from_secs(30) {
                let _ = forget(&fake, &layout(), ID);
            }
        });
        assert_eq!(
            watch(&fake, &layout(), ID, &admin(), &clock, TEST),
            Ended::Disarmed
        );
        assert_eq!(clock.elapsed(), Duration::from_secs(30));
        assert_eq!(locks(&fake), 0);
        assert!(
            !fake.calls().contains(&session_cmd(&admin()).display()),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn a_watch_that_reaches_its_longest_life_locks_even_while_runs_still_come() {
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        let clock = Script::new(|t| {
            let s = t.as_secs();
            if s % 10 == 0 {
                runs(&fake, None, Some(&format!("{}-{s}", 1_800_000_000 + s)));
            }
        });
        let short = Bounds {
            life: Duration::from_secs(300),
            ..TEST
        };
        let ended = watch(&fake, &layout(), ID, &admin(), &clock, short);
        assert_eq!(
            ended,
            Ended::Done(
                "locked admin's session: it had watched for 300s, the longest a watch lasts".into()
            )
        );
        assert_eq!(locks(&fake), 1);
    }

    fn run_watcher(fake: &Fake, clock: &dyn Clock) -> (ExitCode, String) {
        let mut out = Vec::new();
        let code = run(fake, &layout(), ID, 777, clock, TEST, &mut out);
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn the_watcher_records_what_it_did_and_leaves_no_arm() {
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        let (code, out) = run_watcher(&fake, &Script::new(|_| {}));
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            out,
            "watching 1800000000-4242 for admin (uid 501)\nlocked admin's session: no signed \
             request reached the host for 60s\n"
        );
        assert_eq!(
            fake.written(&path(ID, "ended")).as_deref(),
            Some(&b"locked admin's session: no signed request reached the host for 60s\n"[..])
        );
        assert_eq!(fake.mode_of(&path(ID, "ended")), Some(0o600));
        assert!(fake.written(&path(ID, "armed")).is_none());
        assert!(fake.written(&path(ID, "watching")).is_none());
        let calls = fake.calls();
        let at = |c: &str| calls.iter().position(|x| x == c).unwrap();
        // Hang-ups are ignored before it watches; `.watching` says it does.
        assert!(at("ignore hangups") < at(&format!("write {} 600", path(ID, "watching"))));
        // A disarmed watch writes nothing.
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        let clock = Script::new(|_| {
            let _ = forget(&fake, &layout(), ID);
        });
        let (code, out) = run_watcher(&fake, &clock);
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(out.ends_with(&format!("disarmed {ID}\n")), "{out}");
        assert!(fake.written(&path(ID, "ended")).is_none());
        assert_eq!(locks(&fake), 0);
    }

    #[test]
    fn the_watcher_trusts_only_an_arm_that_is_roots_alone_for_an_account_with_that_uid() {
        // Another uid now.
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        put(
            &fake,
            &path(ID, "armed"),
            b"uid = 502\nuser = \"admin\"\narmed = 1800000000\n",
        );
        let (code, out) = run_watcher(&fake, &Script::new(|_| {}));
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(
            out.contains("account admin is uid 501 now, not 502"),
            "{out}"
        );
        // A directory others may read.
        let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        fake.stat_of(
            DIR,
            Stat {
                uid: 0,
                gid: 0,
                mode: 0o755,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        let (code, out) = run_watcher(&fake, &Script::new(|_| {}));
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(out.contains("is not root's alone"), "{out}");
        // Not root.
        let mut fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
        fake.euid = 501;
        let (code, _) = run_watcher(&fake, &Script::new(|_| {}));
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert_eq!(locks(&fake), 0);
        assert!(!fake.calls().contains(&"ignore hangups".to_owned()));
    }

    fn disarm_it(fake: &Fake, id: &str) -> (ExitCode, String) {
        let mut out = Vec::new();
        let code = disarm(fake, &layout(), id, &mut out);
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn disarm_removes_the_arm_first_and_says_what_an_ended_watcher_did() {
        let fake = armed_host(None, None);
        fake.file(&path(ID, "watching"), b"777\n");
        let (code, out) = disarm_it(&fake, ID);
        assert_eq!(
            (code, out.as_str()),
            (ExitCode::SUCCESS, "disarmed 1800000000-4242\n")
        );
        let removes: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("remove"))
            .collect();
        assert_eq!(
            removes,
            ["armed", "watching", "ended", "log"].map(|ext| format!("remove {}", path(ID, ext)))
        );
        assert!(fake.written(&path(ID, "armed")).is_none());
        // The watcher had locked and ended.
        let fake = host();
        fake.file(
            &path(ID, "ended"),
            b"locked admin's session: no signed request reached the host for 61s\n",
        );
        let (_, out) = disarm_it(&fake, ID);
        assert_eq!(
            out,
            "disarmed 1800000000-4242 (it had ended: locked admin's session: no signed \
             request reached the host for 61s)\n"
        );
        // Nothing there: a later arm replaced it.
        let (code, out) = disarm_it(&host(), ID);
        assert_eq!(
            (code, out.as_str()),
            (
                ExitCode::SUCCESS,
                "disarmed 1800000000-4242 (nothing was armed)\n"
            )
        );
        // An id that is not one, or not root: refused, nothing removed.
        let fake = host();
        let (code, _) = disarm_it(&fake, "../../etc/passwd");
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        let mut fake = host();
        fake.euid = 501;
        let (code, _) = disarm_it(&fake, ID);
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(!fake.calls().iter().any(|c| c.starts_with("remove")));
    }

    #[test]
    fn screen_watch_is_an_agent_verb_and_its_unknown_forms_are_refused() {
        let fake = host();
        for args in [
            &["screen-watch"][..],
            &["screen-watch", "arm", "--user", "admin", "--uid", "501"],
            &["screen-watch", "lock"],
        ] {
            let mut out = Vec::new();
            let code = super::super::run(
                &fake,
                &layout(),
                Path::new("/x"),
                args,
                &mut std::io::empty(),
                &mut out,
            );
            assert_eq!(code, ExitCode::from(super::super::REFUSED), "{args:?}");
            assert!(String::from_utf8_lossy(&out).starts_with("refused: "));
        }
        let mut out = Vec::new();
        let code = super::super::run(
            &fake,
            &layout(),
            Path::new("/x"),
            &["screen-watch", "disarm", ID],
            &mut std::io::empty(),
            &mut out,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(fake.calls().len(), 4, "the disarm's removals only");
    }
}
