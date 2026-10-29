// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-lab-guest`: the guest side of `lab mac nux`, copied into a macOS guest and run there by
//! the lane (dev-only: never packed, shipped or run on the Mac that builds it).
//!
//! - `census OUT`: every 250 ms, the on-screen windows (`CGWindowListCopyWindowInfo`: a window's
//!   number, owner name, owner pid, layer and bounds need no Screen Recording grant; its title,
//!   which does, is never read): the first sweep's windows as `base` lines, then each window that
//!   appears (`new`) and goes (`gone`).
//! - `person OUT [--password PW] [--admin NAME:PW] [--sheet reopen|later] [--app EXE]`: the
//!   person at the guest's console. A one-point pointer move every [`NUDGE`] (a posted HID event, which resets
//!   `HIDIdleTime`: each input's line carries the idle clock read just before it and the read
//!   after it that showed the reset, with the window from the post to that read), and in a window
//!   of an owner it acts on ([`ACTS_ON`]): a password field filled with posted keys and Return,
//!   System Settings' Quit & Reopen sheet answered ([`reopen_sheet`]: Quit & Reopen, or Later
//!   with `--sheet later`), a System Settings switch whose row names aterm turned on, or an
//!   allowing button ([`ALLOW`]) pressed, each with a posted click at its centre; and, within
//!   [`QUIT_ASKED_WITHIN`] of its own Quit & Reopen press, the app's own question before it quits
//!   answered with Quit, in a window of a process running EXE (`--app`; [`quit_question`],
//!   [`answers_quit`]); and on System Settings' Developer Tools page ([`devtools_pane`]) aterm
//!   added to its list ([`Devtools::step`]: the helper panel's tile of aterm dragged onto the
//!   list, else + and `/Applications/aterm.app` chosen in the open panel; never while a sheet or
//!   panel stands, and an open panel a try left up cancelled) and aterm's switch there turned on. The password is
//!   the account's own (`--password`), or, with `--admin`, an administrator's name and password for a dialog that asks for one ([`answer`]),
//!   as a person at a standard account types them with an administrator at hand; an
//!   administrator's dialog still there after its Return is answered again, at most [`TRIES`]
//!   times, the account's own password typed once per dialog. No password goes in while the focus
//!   reads a text field that is not secure ([`may_type`]), and none is written to the record
//!   ([`Credentials::holds_password`]). It needs Accessibility (to read the dialogs) and the
//!   event-post grant, which the lane's person base writes for this file's path.
//! - `watch FILE OUT [--kill PID --when-running MASK]`: FILE's text, on one line, each time it
//!   changes (looked at every 50 ms); with `--kill`, PID is sent SIGKILL the first time the
//!   record's `running` stage is one of MASK's bits.
//! - `procs EXE OUT`: every [`WATCH_EVERY`], the processes running EXE with no argument (the app,
//!   as LaunchServices opens it; never a command run from it): `up` with the kernel's start time
//!   for each that appears, `down` for each that goes ([`procs`]).
//! - `member HOME LISTEN OUT [--allow NAME]`: the Allowing member of the `vault-phrase` row, F10's
//!   members desk on the vault custody under HOME behind an `ask`-only listener on loopback,
//!   which presses Allow for the first Mac that asks and writes the phrase to OUT ([`member`]).
//! - `browse SECS DIR SERVICE...`, `dns-sd SECS OUT ARG...`, `arrivals SEED JOINER FILE...`,
//!   `enrol HOME AUTHORITY`: the guest steps that are more than one command ([`steps`]); each
//!   ends by itself.
//!
//! The first five write a record: every line starts with its kind and the guest's clock in epoch
//! ms, and is flushed as written; the lane reads them (`mac_nux::read_census`, `read_person`,
//! `read_watch`, `read_procs`, `mac_nux::phrase::read_desk`). Each of the five runs until it is
//! killed.

#![deny(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod member;
mod steps;

/// How often `census` and `person` sweep the windows.
const SWEEP: Duration = Duration::from_millis(250);
/// How often `person` moves the pointer: presence is HID input within the last minute.
const NUDGE: Duration = Duration::from_secs(5);
/// How often `watch` reads its file.
const WATCH_EVERY: Duration = Duration::from_millis(50);
/// The owners `person` acts in: the authorization dialog, the consent and Gatekeeper alerts, the
/// Accessibility prompt, the Local Network alert, and System Settings (its switches and sheets).
const ACTS_ON: [&str; 6] = [
    "SecurityAgent",
    "UserNotificationCenter",
    "CoreServicesUIAgent",
    "universalAccessAuthWarn",
    "nehelper",
    "System Settings",
];
/// The buttons `person` presses in a dialog (never in System Settings itself): what a person
/// letting aterm go on presses.
const ALLOW: [&str; 7] = [
    "Allow",
    "OK",
    "Open",
    "Continue",
    "Modify Settings",
    "Unlock",
    "Install Helper",
];
/// How long the same control is left alone after `person` pressed it.
const AGAIN: Duration = Duration::from_secs(3);
/// How many times `person` answers one administrator's dialog that stays after its Return: in a
/// loaded Golden Gate guest (2026-10-28) the System Settings sheet went inactive (occluded) under a
/// CoreServicesUIAgent window between the person's keys, so its typed password and Return were
/// lost and nothing was authorized; a person sees the dialog still there and types again. A try
/// that typed nothing ([`may_type`]) counts too. A prompt for the account's own password is typed
/// into once, its Return not watched.
const TRIES: u32 = 3;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("aterm-lab-guest: {why}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: aterm-lab-guest census OUT | person OUT [--password PW] [--admin NAME:PW] [--sheet reopen|later] [--app EXE] | watch FILE OUT [--kill PID --when-running MASK] | procs EXE OUT | member HOME LISTEN OUT [--allow NAME] | browse SECS DIR SERVICE... | dns-sd SECS OUT ARG... | arrivals SEED JOINER FILE... | enrol HOME AUTHORITY";
    match args.first().map(String::as_str) {
        Some("census") => census(&Out::open(args.get(1).ok_or(usage)?)?),
        Some("person") => {
            let out = Out::open(args.get(1).ok_or(usage)?)?;
            let admin = match flag(args, "--admin") {
                Some(a) => Some(
                    a.split_once(':')
                        .map(|(name, pw)| (name.to_owned(), pw.to_owned()))
                        .ok_or("--admin takes NAME:PASSWORD")?,
                ),
                None => None,
            };
            let who = Credentials {
                password: flag(args, "--password").unwrap_or_default(),
                admin,
                account: account_names(),
            };
            let sheet = match flag(args, "--sheet").as_deref() {
                None | Some("reopen") => Sheet::Reopen,
                Some("later") => Sheet::Later,
                Some(other) => return Err(format!("--sheet {other}: reopen or later")),
            };
            person(&out, &who, sheet, flag(args, "--app").as_deref())
        }
        Some("watch") => {
            let file = args.get(1).ok_or(usage)?;
            let out = Out::open(args.get(2).ok_or(usage)?)?;
            let kill = match (flag(args, "--kill"), flag(args, "--when-running")) {
                (Some(pid), Some(bit)) => Some((
                    pid.parse::<i32>()
                        .map_err(|_| format!("--kill {pid}: no pid"))?,
                    bit.parse::<u64>()
                        .map_err(|_| format!("--when-running {bit}: no stage mask"))?,
                )),
                (None, None) => None,
                _ => return Err("--kill and --when-running go together".into()),
            };
            watch(file, &out, kill)
        }
        Some("procs") => {
            let exe = args.get(1).ok_or(usage)?;
            procs(exe, &Out::open(args.get(2).ok_or(usage)?)?)
        }
        Some("member") => member::member(&member::Ask::parse(&args[1..])?),
        Some("browse") => steps::browse(&args[1..]),
        Some("dns-sd") => steps::dns_sd(&args[1..]),
        Some("arrivals") => steps::arrivals(&args[1..], &mut std::io::stdout().lock()),
        Some("enrol") => steps::enrol(&args[1..], &mut std::io::stdout().lock()),
        _ => Err(usage.into()),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).cloned()
}

/// What the person answers a password dialog with.
struct Credentials {
    /// The account's own password (`--password`).
    password: String,
    /// An administrator's name and password (`--admin`), for a dialog that asks for one.
    admin: Option<(String, String)>,
    /// The account's short and full names, as a dialog's name field shows the account.
    account: Vec<String>,
}

impl Credentials {
    /// Whether a field's `value` holds a password the person types (keys that went to a field not
    /// secure), so the record shows `(a password)` in its place: one of the names the person reads
    /// or types there (the account's, the administrator's) is shown, though a password may equal
    /// it (the image's administrator is `admin`, password `admin`). Pure.
    fn holds_password(&self, value: &str) -> bool {
        let name = self
            .account
            .iter()
            .chain(self.admin.as_ref().map(|(name, _)| name))
            .any(|n| n.trim() == value.trim());
        !name
            && std::iter::once(&self.password)
                .chain(self.admin.as_ref().map(|(_, password)| password))
                .any(|p| !p.is_empty() && value.contains(p.as_str()))
    }
}

/// This account's short and full names (`getpwuid_r`; on macOS the full name is `pw_gecos`).
fn account_names() -> Vec<String> {
    // SAFETY: getuid reads this process's real uid and cannot fail; `pw` and `buf` are stack
    // buffers handed to the reentrant getpwuid_r, which writes the entry's strings into `buf` and
    // points `pw`'s fields at them; the fields are read only when the call succeeded and found an
    // entry, each non-null one as a NUL-terminated string, while `buf` is still alive.
    #[expect(unsafe_code, reason = "getpwuid_r(3): this account's names")]
    let names = unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0u8; 4096];
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut found,
        );
        if rc != 0 || found.is_null() {
            Vec::new()
        } else {
            [pw.pw_name, pw.pw_gecos]
                .into_iter()
                .filter(|s| !s.is_null())
                .map(|s| std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        }
    };
    names.into_iter().filter(|n| !n.trim().is_empty()).collect()
}

/// The guest's clock, in epoch ms.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A record file, each line flushed as written.
struct Out(std::sync::Mutex<std::fs::File>);

impl Out {
    fn open(path: &str) -> Result<Out, String> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(|f| Out(std::sync::Mutex::new(f)))
            .map_err(|e| format!("{path}: {e}"))
    }

    fn line(&self, kind: &str, rest: &str) {
        let mut f = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(f, "{kind} {} {rest}", now_ms());
        let _ = f.flush();
    }
}

/// One on-screen window, as the window list names it.
#[derive(Clone, Debug)]
struct Window {
    id: u64,
    pid: i32,
    layer: i64,
    owner: String,
    frame: (f64, f64, f64, f64),
}

impl Window {
    fn describe(&self) -> String {
        format!(
            "{} {} {} {:.0}x{:.0} {}",
            self.id, self.pid, self.layer, self.frame.2, self.frame.3, self.owner
        )
    }
}

fn census(out: &Out) -> Result<(), String> {
    let Some(first) = cg::on_screen() else {
        out.line(
            "census-error",
            "the window list is unreadable: no window server session",
        );
        return Err("no window list".into());
    };
    out.line(
        "census",
        &format!("every={} windows={}", SWEEP.as_millis(), first.len()),
    );
    let mut known: BTreeSet<u64> = BTreeSet::new();
    for w in &first {
        out.line("base", &w.describe());
        known.insert(w.id);
    }
    loop {
        std::thread::sleep(SWEEP);
        let Some(now) = cg::on_screen() else {
            continue;
        };
        let ids: BTreeSet<u64> = now.iter().map(|w| w.id).collect();
        for w in now.iter().filter(|w| !known.contains(&w.id)) {
            out.line("new", &w.describe());
        }
        for gone in known.difference(&ids) {
            out.line("gone", &gone.to_string());
        }
        known = ids;
    }
}

/// How long [`proven`] reads the idle clock for an input to show: a hang detector.
const SHOWN_WITHIN: Duration = Duration::from_secs(60);

/// How long the idle clock has run since its last reset before [`proven`] posts: macOS leaves
/// `HIDIdleTime` running through input that comes soon after its last reset. In a Golden Gate
/// guest (`lab mac nux --person`, 2026-10-28) a click, a typed password, and two pointer moves
/// posted 0.35 to 2.1 s after a reset left the clock running, though the password was taken and
/// Full Disk Access came on with it; the ten inputs posted 5.18 s or more after one each reset
/// it. So the person waits this long between two inputs it proves, as a person between two
/// actions would.
const QUIET_BEFORE: Duration = Duration::from_millis(5500);

/// How much longer the person waits before an input so the idle clock, read `idle_ns`, has run
/// [`QUIET_BEFORE`]: `None` once it has, or when the clock is unreadable. Pure.
fn quiet_left(idle_ns: Option<u64>) -> Option<Duration> {
    QUIET_BEFORE
        .checked_sub(Duration::from_nanos(idle_ns?))
        .filter(|left| !left.is_zero())
}

/// Wait until the idle clock has run [`QUIET_BEFORE`] (no longer than [`SHOWN_WITHIN`]), post
/// `input`, then read the idle clock (in process: a registry read) until it shows a reset since the
/// post: an `input` line. Each read takes `window`, the time since just before the post, before it
/// reads, so `idle_after_ns <= window_ns` holds only for a reset after the post, whatever stalls
/// the helper after the read; and reading until the clock shows it, rather than once after a pause,
/// keeps an event WindowServer takes late from reading as lost. An input the clock never shows
/// within [`SHOWN_WITHIN`] keeps its last read, past its window. What this proves is that the
/// helper's posted events reach the HID system at all (the event-post grant, the console session),
/// which fails for every input together: a lone dropped input among delivered ones can pass on an
/// earlier input's late event. It hands back the instant just before the post, which what the
/// input did is timed from ([`went`]): the read that showed the reset can come up to
/// [`SHOWN_WITHIN`] after it.
fn proven(out: &Out, what: &str, input: impl FnOnce()) -> Instant {
    let waits = Instant::now();
    let before = loop {
        let before = cg::hid_idle_ns();
        match quiet_left(before) {
            Some(left) if waits.elapsed() < SHOWN_WITHIN => std::thread::sleep(left),
            _ => break before,
        }
    };
    let posted = Instant::now();
    input();
    let (after, window) = loop {
        let window = posted.elapsed();
        let after = cg::hid_idle_ns();
        let shown = after.is_some_and(|ns| u128::from(ns) <= window.as_nanos());
        if shown || after.is_none() || window >= SHOWN_WITHIN {
            break (after, window);
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let ns = |v: Option<u64>| v.map_or_else(|| "?".to_owned(), |n| n.to_string());
    out.line(
        "input",
        &format!(
            "{what} idle_before_ns={} idle_after_ns={} window_ns={}",
            ns(before),
            ns(after),
            window.as_nanos()
        ),
    );
    posted
}

fn person(out: &Out, who: &Credentials, sheet: Sheet, app: Option<&str>) -> Result<(), String> {
    out.line(
        "person",
        &format!(
            "trusted={} post={} password={}",
            if ax::trusted() { "yes" } else { "no" },
            if cg::may_post() { "yes" } else { "no" },
            if who.password.is_empty() { "no" } else { "yes" }
        ),
    );
    let mut last_nudge: Option<Instant> = None;
    let mut step = 1.0;
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut pressed: BTreeMap<String, Instant> = BTreeMap::new();
    // Each password dialog typed into, by its windows' key: how many tries (one that typed nothing
    // counts), [`TRIES`] once it went or once the account's own password went in.
    let mut typed: BTreeMap<String, u32> = BTreeMap::new();
    // Each process's verdict by its pid and start: whether it runs the app ([`app_pids`]).
    let mut known: BTreeMap<(u32, u64), bool> = BTreeMap::new();
    let mut dev = Devtools::new(app);
    loop {
        if last_nudge.is_none_or(|t| t.elapsed() >= NUDGE) {
            proven(out, "nudge", || cg::nudge(step));
            step = -step;
            last_nudge = Some(Instant::now());
        }
        let windows = cg::on_screen().unwrap_or_default();
        let ids: BTreeSet<u64> = windows.iter().map(|w| w.id).collect();
        seen.retain(|id| {
            let stays = ids.contains(id);
            if !stays {
                out.line("gone", &id.to_string());
            }
            stays
        });
        // Each app acted in, its owner name and its windows' numbers (the key a typed password
        // is kept under, so a new dialog of the same app is typed into afresh).
        let mut apps: BTreeMap<i32, (String, Vec<u64>)> = BTreeMap::new();
        for w in windows
            .iter()
            .filter(|w| ACTS_ON.contains(&w.owner.as_str()))
        {
            if seen.insert(w.id) {
                out.line("seen", &format!("{} {}", w.id, w.owner));
            }
            let app = apps
                .entry(w.pid)
                .or_insert_with(|| (w.owner.clone(), Vec::new()));
            app.1.push(w.id);
        }
        let keys: BTreeSet<String> = apps
            .iter()
            .map(|(pid, (_, ids))| format!("{pid}:{ids:?}"))
            .collect();
        typed.retain(|k, _| keys.contains(k));
        for (pid, (owner, ids)) in &apps {
            act(
                out,
                *pid,
                owner,
                who,
                sheet,
                &mut pressed,
                &mut typed,
                ids,
                &mut dev,
            );
        }
        if let Some(exe) = app
            && answers_quit(sheet, reopen_pressed(&pressed).map(|t| t.elapsed()))
        {
            answer_quit(out, exe, &windows, who, &mut pressed, &mut known);
        }
        std::thread::sleep(SWEEP);
    }
}

/// One pass over `pid`'s dialogs (`ids`, its windows on screen): the first thing a person would do
/// there, done; a Quit & Reopen sheet answered with `sheet`; on System Settings' Developer Tools
/// page, aterm added to its list and switched on (`dev`, [`Devtools::step`]).
#[expect(
    clippy::too_many_arguments,
    reason = "the person's pass: the app, its owner, the answers it gives and what it remembers"
)]
fn act(
    out: &Out,
    pid: i32,
    owner: &str,
    who: &Credentials,
    sheet: Sheet,
    pressed: &mut BTreeMap<String, Instant>,
    typed: &mut BTreeMap<String, u32>,
    ids: &[u64],
    dev: &mut Devtools,
) {
    // The key a typed password is kept under, as `person` keeps it.
    let windows = format!("{pid}:{ids:?}");
    let nodes = ax::nodes(pid);
    let fresh = |key: &str, pressed: &BTreeMap<String, Instant>| {
        pressed.get(key).is_none_or(|t| t.elapsed() >= AGAIN)
    };
    if !who.password.is_empty()
        && typed.get(&windows).is_none_or(|&tries| tries < TRIES)
        && let Some(secure) = nodes.iter().position(|n| n.secure)
    {
        out.line(
            "dialog",
            &format!(
                "{pid} {owner}: {}",
                describe(&nodes, dialog_around(&nodes, secure), who)
            ),
        );
        let admin = who.admin.as_ref().map(|(name, _)| name.as_str());
        let (answer, field) = answer(&nodes, secure, &who.account, admin);
        // The name field as the dialog showed it, then as it read once the person typed there.
        let mut name = field.map_or_else(
            || "-".to_owned(),
            |at| token(nodes[at].value_text.as_deref(), who),
        );
        let (admin, password) = credentials(answer, who);
        if let (Some(admin), Some(at)) = (admin, field) {
            let field = &nodes[at];
            click_into(out, pid, field, "name", who);
            // Text in the field, or text unread, is selected so the name replaces it.
            if field.value_text.as_deref().is_none_or(|t| !t.is_empty()) {
                proven(out, "key", cg::select_all);
            }
            proven(out, "key", || cg::type_text(admin));
            focus(out, pid, "named", who);
            let now = ax::nodes(pid)
                .into_iter()
                .find(|n| n.role == field.role && !n.secure && n.frame == field.frame)
                .and_then(|n| n.value_text);
            name = format!("{name}>{}", token(now.as_deref(), who));
        }
        let field = &nodes[secure];
        let focused = if answer == Answer::Account {
            proven(out, "click", || cg::click(field.centre()));
            focus(out, pid, "password", who)
        } else {
            click_into(out, pid, field, "password", who)
        };
        let tries = typed.entry(windows).or_default();
        if !may_type(focused.as_ref()) {
            out.line(
                "skip",
                &format!(
                    "{pid} {owner}: the {}'s password not typed (the focus read a text field \
                     not secure)",
                    answer.whose()
                ),
            );
            *tries += 1;
            return;
        }
        // The instant just before the Return went out, which the dialog's going is timed from.
        let mut returned = None;
        for key in password_keys(answer, password) {
            let posted = proven(out, "key", || key.post());
            if key == Key::Return {
                returned = Some(posted);
            }
        }
        let went = match returned {
            Some(at) if answer.watched() => Some(went(pid, ids, at)),
            _ => None,
        };
        out.line(
            "typed",
            &format!(
                "{pid} {} {} {name} {} {owner}",
                answer.whose(),
                password.chars().count(),
                match went {
                    None => "unwatched".to_owned(),
                    Some(None) => "stayed".to_owned(),
                    Some(Some(ms)) => format!("went:{ms}"),
                }
            ),
        );
        *tries = if went == Some(None) {
            *tries + 1
        } else {
            TRIES
        };
        pressed.insert(typed_key(pid), Instant::now());
        return;
    }
    if let Some(found) = reopen_sheet(&nodes) {
        let button = &nodes[found.button(sheet)];
        let key = sheet_key(pid, sheet);
        if fresh(&key, pressed) {
            out.line(
                "sheet",
                &format!(
                    "{pid} {owner}: {}",
                    describe(&nodes, found.dialog.clone(), who)
                ),
            );
            proven(out, "click", || cg::click(button.centre()));
            out.line(
                "click",
                &format!("{pid} {owner} {}", one_line(sheet.title())),
            );
            pressed.insert(key, Instant::now());
        }
        return;
    }
    if owner == "System Settings" {
        if let Some(pane) = devtools_pane(&nodes) {
            dev.step(out, pid, owner, &nodes, &pane, pressed, who);
            return;
        }
        for switch in nodes
            .iter()
            .filter(|n| n.role == "AXCheckBox" && n.value == Some(0))
        {
            let names_aterm = switch.text.to_lowercase().contains("aterm")
                || nodes
                    .iter()
                    .any(|n| n.parent == switch.parent && n.text.to_lowercase().contains("aterm"));
            let key = format!("{pid} switch {:.0},{:.0}", switch.frame.0, switch.frame.1);
            if names_aterm && fresh(&key, pressed) {
                proven(out, "click", || cg::click(switch.centre()));
                out.line(
                    "click",
                    &format!("{pid} {owner} switch:{}", one_line(&switch.text)),
                );
                pressed.insert(key, Instant::now());
                return;
            }
        }
        return;
    }
    for name in ALLOW {
        let Some(button) = nodes
            .iter()
            .find(|n| n.role == "AXButton" && n.text == name && n.enabled)
        else {
            continue;
        };
        let key = format!("{pid} {name}");
        if fresh(&key, pressed) {
            proven(out, "click", || cg::click(button.centre()));
            out.line("click", &format!("{pid} {owner} {}", one_line(name)));
            pressed.insert(key, Instant::now());
        }
        return;
    }
}

/// How the person answers System Settings' Quit & Reopen sheet (`--sheet`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sheet {
    /// What a person most often presses: aterm quits and macOS opens it again.
    Reopen,
    /// Leave aterm running; the access takes effect at its next start.
    Later,
}

impl Sheet {
    /// Its button's title, as the sheet shows it.
    const fn title(self) -> &'static str {
        match self {
            Sheet::Reopen => "Quit & Reopen",
            Sheet::Later => "Later",
        }
    }
}

/// The Quit & Reopen sheet [`reopen_sheet`] found: its controls' range in the list, and its two
/// buttons.
#[derive(Debug, PartialEq, Eq)]
struct ReopenSheet {
    dialog: std::ops::Range<usize>,
    reopen: usize,
    later: usize,
}

impl ReopenSheet {
    /// The button that gives `answer`.
    const fn button(&self, answer: Sheet) -> usize {
        match answer {
            Sheet::Reopen => self.reopen,
            Sheet::Later => self.later,
        }
    }
}

/// Whether `n` is an enabled button titled `title`: its text (title, description and value,
/// joined) is the title, or starts with it as whole words, spacing and case aside. Pure.
fn titled(n: &ax::Node, title: &str) -> bool {
    let words: Vec<String> = n.text.split_whitespace().map(str::to_lowercase).collect();
    let want: Vec<String> = title.split_whitespace().map(str::to_lowercase).collect();
    n.role == "AXButton" && n.enabled && !want.is_empty() && words.starts_with(&want)
}

/// The sheet System Settings puts up once an access switch for a running app changed (Full Disk
/// Access): an `AXSheet` holding an enabled `Quit & Reopen` button and an enabled `Later` button
/// ([`titled`]). The same buttons in a window that is no sheet, or a sheet missing either, are not
/// it. Pure.
fn reopen_sheet(nodes: &[ax::Node]) -> Option<ReopenSheet> {
    (0..nodes.len())
        .filter(|&k| titled(&nodes[k], Sheet::Reopen.title()))
        .find_map(|reopen| {
            let dialog = dialog_around(nodes, reopen);
            if nodes[dialog.start].role != "AXSheet" {
                return None;
            }
            let later = dialog
                .clone()
                .find(|&k| titled(&nodes[k], Sheet::Later.title()))?;
            Some(ReopenSheet {
                dialog,
                reopen,
                later,
            })
        })
}

/// The key the person keeps its press on `pid`'s Quit & Reopen sheet under, answered `sheet`.
fn sheet_key(pid: i32, sheet: Sheet) -> String {
    format!("{pid} sheet {}", sheet.title())
}

/// When the person last pressed Quit & Reopen on a sheet ([`sheet_key`]): never for Later, an
/// allowing button or a switch. Pure.
fn reopen_pressed(pressed: &BTreeMap<String, Instant>) -> Option<Instant> {
    let suffix = format!(" sheet {}", Sheet::Reopen.title());
    pressed
        .iter()
        .filter(|(key, _)| {
            key.strip_suffix(suffix.as_str())
                .is_some_and(|pid| pid.parse::<i32>().is_ok())
        })
        .map(|(_, at)| *at)
        .max()
}

/// How long after its own Quit & Reopen press the person answers the app's question before it
/// quits: macOS asks the app to quit as the sheet goes, and the app asks at once.
const QUIT_ASKED_WITHIN: Duration = Duration::from_secs(60);

/// The question the app asks before it quits (`aterm_gui::quit_safety::confirm_prompt`'s title
/// for a quit), as its alert's text reads.
const QUIT_QUESTION: &str = "Are you sure you want to quit aterm?";

/// The word of the person's press on [`QUIT_QUESTION`]'s Quit, as its `click` line carries it
/// (`mac_nux_guest::QUIT_CONFIRM`).
const QUIT_CONFIRM: &str = "quit-confirm";

/// Whether the person answers the app's question before it quits: only after its own Quit &
/// Reopen press, less than [`QUIT_ASKED_WITHIN`] ago (`since`: how long ago; `None`: never
/// pressed), and never when it answers the sheet with Later. Pure.
fn answers_quit(sheet: Sheet, since: Option<Duration>) -> bool {
    sheet == Sheet::Reopen && since.is_some_and(|d| d < QUIT_ASKED_WITHIN)
}

/// The app's question before it quits [`quit_question`] found: its controls' range in the list,
/// and its Quit button.
#[derive(Debug, PartialEq, Eq)]
struct QuitQuestion {
    dialog: std::ops::Range<usize>,
    quit: usize,
}

/// Whether `n` is an enabled button whose text is `title`, spacing and case aside (never one that
/// only starts with it: `Quit & Reopen` is no `Quit`). Pure.
fn named(n: &ax::Node, title: &str) -> bool {
    let words = |t: &str| {
        t.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    n.role == "AXButton" && n.enabled && !title.is_empty() && words(&n.text) == words(title)
}

/// Whether `n` is static text that reads `words` (spacing aside), and nothing else: a text
/// whose title and value both carry them reads them twice. Pure.
fn reads(n: &ax::Node, words: &str) -> bool {
    let text = n.text.split_whitespace().collect::<Vec<_>>().join(" ");
    n.role == "AXStaticText"
        && text.contains(words)
        && text.split(words).all(|rest| rest.trim().is_empty())
}

/// The app's alert before it quits: a window or sheet whose text reads [`QUIT_QUESTION`], with an
/// enabled `Quit` button and an enabled `Cancel` button ([`named`]). An alert with Cancel alone,
/// other words, or the app's own pages (their Continue and Open) are not it. Pure.
fn quit_question(nodes: &[ax::Node]) -> Option<QuitQuestion> {
    (0..nodes.len())
        .filter(|&k| named(&nodes[k], "Quit"))
        .find_map(|quit| {
            let dialog = dialog_around(nodes, quit);
            if !matches!(nodes[dialog.start].role.as_str(), "AXWindow" | "AXSheet") {
                return None;
            }
            dialog.clone().find(|&k| reads(&nodes[k], QUIT_QUESTION))?;
            dialog.clone().find(|&k| named(&nodes[k], "Cancel"))?;
            Some(QuitQuestion { dialog, quit })
        })
}

/// The app's question before it quits, answered as a person who pressed Quit & Reopen answers it:
/// in each on-screen window's process that runs `exe` with no argument (the app, as LaunchServices
/// opens it: [`app_pids`]), an alert [`quit_question`] finds, recorded (`sheet` line) and its Quit
/// pressed with a proven click (`click ... quit-confirm`), at most once every [`AGAIN`]. Never a
/// window of another process, nor another of the app's buttons.
fn answer_quit(
    out: &Out,
    exe: &str,
    windows: &[Window],
    who: &Credentials,
    pressed: &mut BTreeMap<String, Instant>,
    known: &mut BTreeMap<(u32, u64), bool>,
) {
    let Some(apps) = app_pids(exe, known) else {
        return;
    };
    let mut owners: BTreeMap<i32, &str> = BTreeMap::new();
    for w in windows {
        if u32::try_from(w.pid).is_ok_and(|pid| apps.contains_key(&pid)) {
            owners.entry(w.pid).or_insert(w.owner.as_str());
        }
    }
    for (pid, owner) in owners {
        let nodes = ax::nodes(pid);
        let Some(found) = quit_question(&nodes) else {
            continue;
        };
        let key = format!("{pid} {QUIT_CONFIRM}");
        if pressed.get(&key).is_some_and(|t| t.elapsed() < AGAIN) {
            continue;
        }
        out.line(
            "sheet",
            &format!(
                "{pid} {owner}: {}",
                describe(&nodes, found.dialog.clone(), who)
            ),
        );
        proven(out, "click", || cg::click(nodes[found.quit].centre()));
        out.line("click", &format!("{pid} {owner} {QUIT_CONFIRM}"));
        pressed.insert(key, Instant::now());
    }
}

/// The key the person keeps the time it last typed a password into `pid`'s dialog under
/// ([`may_add`] waits on it).
fn typed_key(pid: i32) -> String {
    format!("{pid} typed")
}

/// How long after its last add, and after a password it typed since, the person waits for aterm
/// to show on the Developer Tools list before it adds aterm again.
const ADD_SETTLE: Duration = Duration::from_secs(20);

/// The most times the person adds aterm to the Developer Tools list in a row: one by dragging the
/// helper panel's tile (the first try that finds it, [`add_how`]), the rest with + (a drop
/// something took leaves the tile spent, and the panel then says to press +). A try that did
/// nothing counts too.
const ADDS: usize = 3;

/// How long the person looks for the open panel after its + press, and for what a key in it
/// brings (the Go to field's focus, an enabled Open), before it gives the try up.
const PANEL_WITHIN: Duration = Duration::from_secs(20);

/// The helper panel's heading on its Developer Tools page (`atpkg::devtools::HELPER_TITLE`): the
/// window the person drags aterm's tile from reads it.
const DEVTOOLS_HELPER_TITLE: &str = "Add aterm to Developer Tools";

/// The tile's accessibility label (`aterm_gui::devtools_helper::TILE_CAPTION`, on an `AXImage`).
const TILE_LABEL: &str = "aterm";

/// Words of the Developer Tools page's line above its list ("Allow the applications below to run
/// software locally that does not meet the system's security policy."): no other page says them.
const DEVTOOLS_WORDS: &str = "run software locally";

/// The page's name, as its window's title and heading read it.
const DEVTOOLS_TITLE: &str = "Developer Tools";

/// The roles of a list the person drops aterm on and reads rows from.
const LISTS: [&str; 4] = ["AXScrollArea", "AXTable", "AXOutline", "AXList"];

/// The roles of an open panel's view of files.
const FILE_VIEWS: [&str; 4] = ["AXBrowser", "AXOutline", "AXTable", "AXList"];

/// The start of the window list's owner name for the process that shows a sandboxed app's open
/// panel, when the panel is not the app's own window.
const PANEL_SERVICE: &str = "Open and Save Panel Service";

/// The words of the person's acts on the Developer Tools page, as its `click` lines carry them
/// (`mac_nux_guest::DEVTOOLS_ACTS`).
const DEVTOOLS_DRAG: &str = "devtools-drag";
const DEVTOOLS_PLUS: &str = "devtools-plus";
const DEVTOOLS_GOTO: &str = "devtools-goto";
const DEVTOOLS_OPEN: &str = "devtools-open";
const DEVTOOLS_CANCEL: &str = "devtools-cancel";
const DEVTOOLS_SWITCH: &str = "devtools-switch";

/// `text`'s words, lowercased. Pure.
fn words_of(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether `text` is `want`, spacing and case aside. Pure.
fn same_words(text: &str, want: &str) -> bool {
    words_of(text) == words_of(want)
}

/// Whether `text` names aterm: one of its words, quotes and punctuation aside, is `aterm` or
/// `aterm.app` (case aside). Pure.
fn names_aterm(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || "“”‘’\"'(),:;".contains(c))
        .any(|w| w.eq_ignore_ascii_case("aterm") || w.eq_ignore_ascii_case("aterm.app"))
}

/// `nodes[root]` and its descendants, as the range of the list they take (a node's descendants
/// follow it, depth first). Pure.
fn subtree(nodes: &[ax::Node], root: usize) -> std::ops::Range<usize> {
    let under = |mut k: usize| {
        while let Some(n) = nodes.get(k) {
            if n.parent == root {
                return true;
            }
            k = n.parent;
        }
        false
    };
    let end = (root + 1..nodes.len())
        .find(|&k| !under(k))
        .unwrap_or(nodes.len());
    root..end
}

/// The window `nodes[at]` is in: its topmost ancestor, when that is an `AXWindow`. Pure.
fn window_of(nodes: &[ax::Node], at: usize) -> Option<usize> {
    let mut root = at;
    while nodes.get(nodes[root].parent).is_some() {
        root = nodes[root].parent;
    }
    (nodes[root].role == "AXWindow").then_some(root)
}

/// Whether `nodes[k]` sits in a sheet: it or one of its ancestors is an `AXSheet` (an open
/// panel's file view, a password sheet's controls). Pure.
fn in_sheet(nodes: &[ax::Node], k: usize) -> bool {
    let mut at = k;
    loop {
        if nodes[at].role == "AXSheet" {
            return true;
        }
        if nodes.get(nodes[at].parent).is_none() {
            return false;
        }
        at = nodes[at].parent;
    }
}

/// What the person read of System Settings' Developer Tools page ([`devtools_pane`]).
#[derive(Debug, PartialEq, Eq)]
struct DevtoolsPane {
    /// The line the list is found under: the page's words above it, or its heading.
    anchor: usize,
    /// The list (its scroll area, or the table itself): `None` when none read under the anchor.
    list: Option<usize>,
    /// Whether a row of the list names aterm.
    holds: bool,
    /// The switch on aterm's row.
    switch: Option<usize>,
    /// The + below the list.
    plus: Option<usize>,
}

/// System Settings' Developer Tools page, by its words: a window's static text saying
/// [`DEVTOOLS_WORDS`], or, where none does, a window titled [`DEVTOOLS_TITLE`] with a heading
/// that reads it (Privacy & Security's own page lists Developer Tools as a row, in a window titled
/// otherwise and with none of the words). Its list is the nearest one ([`LISTS`]) whose top is at
/// or below the anchor's and which spans some of its width (the sidebar's does not, nor the
/// scroll area of the whole page, which starts above it); else the window's table, outline or list
/// that holds a switch (the sidebar holds none). A list, switch or button in a sheet (the open
/// panel's file view, which can list aterm.app, and a password sheet's) is never the page's. Pure.
fn devtools_pane(nodes: &[ax::Node]) -> Option<DevtoolsPane> {
    let anchor = (0..nodes.len())
        .find(|&k| {
            nodes[k].role == "AXStaticText"
                && words_of(&nodes[k].text).join(" ").contains(DEVTOOLS_WORDS)
        })
        .or_else(|| {
            let window = (0..nodes.len()).find(|&k| {
                nodes[k].role == "AXWindow" && same_words(&nodes[k].text, DEVTOOLS_TITLE)
            })?;
            subtree(nodes, window).find(|&k| {
                k != window
                    && matches!(nodes[k].role.as_str(), "AXStaticText" | "AXHeading")
                    && same_words(&nodes[k].text, DEVTOOLS_TITLE)
            })
        })?;
    let window = window_of(nodes, anchor)?;
    let (ax, ay, aw, _) = nodes[anchor].frame;
    let shown = |k: usize| nodes[k].frame.2 > 0.0 && nodes[k].frame.3 > 0.0;
    let below = subtree(nodes, window)
        .filter(|&k| {
            let (x, y, w, _) = nodes[k].frame;
            LISTS.contains(&nodes[k].role.as_str())
                && shown(k)
                && !in_sheet(nodes, k)
                && y >= ay
                && x < ax + aw.max(1.0)
                && ax < x + w
        })
        .min_by(|&a, &b| {
            nodes[a]
                .frame
                .1
                .total_cmp(&nodes[b].frame.1)
                .then(a.cmp(&b))
        });
    let list = below.or_else(|| {
        subtree(nodes, window).find(|&k| {
            LISTS[1..].contains(&nodes[k].role.as_str())
                && shown(k)
                && !in_sheet(nodes, k)
                && subtree(nodes, k).any(|j| nodes[j].role == "AXCheckBox")
        })
    });
    let holds =
        list.is_some_and(|l| subtree(nodes, l).any(|k| k != l && names_aterm(&nodes[k].text)));
    let switch = list.and_then(|l| {
        subtree(nodes, l).find(|&k| nodes[k].role == "AXCheckBox" && row_names_aterm(nodes, k, l))
    });
    let plus = list.and_then(|l| plus_below(nodes, window, l));
    Some(DevtoolsPane {
        anchor,
        list,
        holds,
        switch,
        plus,
    })
}

/// Whether the switch `nodes[switch]` in the list `nodes[list]` is aterm's: its own text names
/// aterm, or its row does (its nearest `AXRow` inside the list, else its parent: a switch and its
/// app's name can sit in different cells of one row). Pure.
fn row_names_aterm(nodes: &[ax::Node], switch: usize, list: usize) -> bool {
    if names_aterm(&nodes[switch].text) {
        return true;
    }
    let mut row = nodes[switch].parent;
    let mut k = row;
    while k != list && nodes.get(k).is_some() {
        if nodes[k].role == "AXRow" {
            row = k;
            break;
        }
        k = nodes[k].parent;
    }
    nodes.get(row).is_some()
        && row != list
        && subtree(nodes, row).any(|j| j != switch && names_aterm(&nodes[j].text))
}

/// The + of the list `nodes[list]` in `nodes[window]`: an enabled button titled `+` or `Add…`
/// whose top is between the list's top and just below its bottom, within its width; else the
/// leftmost enabled untitled button just below it (+ is left of −). A titled `Remove` or `−` is
/// never it, nor a button in a sheet. Pure.
fn plus_below(nodes: &[ax::Node], window: usize, list: usize) -> Option<usize> {
    let (x, y, w, h) = nodes[list].frame;
    let bottom = y + h;
    let within = |k: usize, top: f64| {
        let f = nodes[k].frame;
        nodes[k].role == "AXButton"
            && nodes[k].enabled
            && !in_sheet(nodes, k)
            && f.1 >= top
            && f.1 <= bottom + 60.0
            && f.0 >= x - 4.0
            && f.0 < x + w
    };
    let titled_plus = |text: &str| {
        text.trim() == "+" || words_of(text).first().is_some_and(|w| w.starts_with("add"))
    };
    subtree(nodes, window)
        .find(|&k| within(k, y) && titled_plus(&nodes[k].text))
        .or_else(|| {
            subtree(nodes, window)
                .filter(|&k| within(k, bottom - 4.0) && nodes[k].text.trim().is_empty())
                .min_by(|&a, &b| nodes[a].frame.0.total_cmp(&nodes[b].frame.0))
        })
}

/// The page's state as the person's `devtools ... pane` line says it: whether the list read and
/// holds aterm, and aterm's switch. Pure.
fn pane_state(nodes: &[ax::Node], pane: &DevtoolsPane) -> String {
    let list = match (pane.list, pane.holds) {
        (None, _) => "unread",
        (Some(_), true) => "holds",
        (Some(_), false) => "lacks",
    };
    let switch = pane.switch.map_or("none", |k| match nodes[k].value {
        Some(0) => "off",
        Some(_) => "on",
        None => "?",
    });
    format!("list={list} switch={switch}")
}

/// Whether the person adds aterm to the list now, after `adds` tries, the last `last_add` ago, and
/// the last password it typed in System Settings `typed` ago: at most [`ADDS`] tries, each
/// [`ADD_SETTLE`] after the last and after a password typed since it (the add may ask one). Pure.
fn may_add(adds: usize, last_add: Option<Duration>, typed: Option<Duration>) -> bool {
    let settled = |ago: Option<Duration>| ago.is_none_or(|d| d >= ADD_SETTLE);
    adds < ADDS && settled(last_add) && (adds == 0 || settled(typed))
}

/// The tile of aterm on the helper panel's Developer Tools page: an `AXImage` labelled
/// [`TILE_LABEL`] with a frame, in a window that reads [`DEVTOOLS_HELPER_TITLE`] (the Full Disk
/// Access page's tile, the app's alert icon, and an image named aterm elsewhere are not it).
/// Pure.
fn devtools_tile(nodes: &[ax::Node]) -> Option<usize> {
    (0..nodes.len())
        .filter(|&k| {
            let n = &nodes[k];
            n.role == "AXImage"
                && same_words(&n.text, TILE_LABEL)
                && n.frame.2 > 0.0
                && n.frame.3 > 0.0
        })
        .find(|&k| {
            window_of(nodes, k)
                .is_some_and(|w| subtree(nodes, w).any(|j| reads(&nodes[j], DEVTOOLS_HELPER_TITLE)))
        })
}

/// An open panel [`open_panel`] found: its controls' range in the list, its Open and its Cancel.
#[derive(Debug, PartialEq, Eq)]
struct OpenPanel {
    dialog: std::ops::Range<usize>,
    open: usize,
    cancel: usize,
}

/// An open panel: a window or sheet holding a button titled Open (enabled or not: it is off until
/// something is chosen), an enabled Cancel of its own (not one of a sheet inside it: the Go to
/// sheet's) and a view of files ([`FILE_VIEWS`]). An alert with Open and Cancel and no files
/// (Gatekeeper's), and the app's quit question, are not it. Pure.
fn open_panel(nodes: &[ax::Node]) -> Option<OpenPanel> {
    (0..nodes.len())
        .filter(|&k| nodes[k].role == "AXButton" && same_words(&nodes[k].text, "Open"))
        .find_map(|open| {
            let dialog = dialog_around(nodes, open);
            if !matches!(nodes[dialog.start].role.as_str(), "AXWindow" | "AXSheet") {
                return None;
            }
            let cancel = dialog.clone().find(|&k| {
                named(&nodes[k], "Cancel") && dialog_around(nodes, k).start == dialog.start
            })?;
            dialog
                .clone()
                .find(|&k| FILE_VIEWS.contains(&nodes[k].role.as_str()))?;
            Some(OpenPanel {
                dialog,
                open,
                cancel,
            })
        })
}

/// The enabled Cancel of a sheet inside the open panel `panel` (the Go to sheet), which takes the
/// panel's clicks while it stands: `None` when no such sheet is up. Pure.
fn inner_cancel(nodes: &[ax::Node], panel: &OpenPanel) -> Option<usize> {
    panel.dialog.clone().find(|&k| {
        named(&nodes[k], "Cancel") && {
            let around = dialog_around(nodes, k).start;
            around != panel.dialog.start && nodes[around].role == "AXSheet"
        }
    })
}

/// Whether the focus, read as `focus`, is where the person types a path after Go to Folder: a
/// text field that is neither secure nor the panel's search field (where the path would search,
/// not go). An unreadable focus is no sign it is. Pure.
fn goes_to(focus: Option<&ax::Node>) -> bool {
    focus.is_some_and(|f| takes_text(f) && !f.secure && f.subrole != "AXSearchField")
}

/// What one read of the open panel after the path's Return says ([`panel_read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelRead {
    /// The panel's Open is enabled: the person presses it.
    Open,
    /// The panel went.
    Gone,
    /// Nothing to act on yet: Open still off, the read empty, or one miss.
    Unsure,
}

/// One read of the open panel after the path's Return: `open`, whether the panel read and its Open
/// is enabled (`None`: no panel matched); `read`, whether the read gave any controls; `shown`,
/// whether the panel's process has a window on screen; `misses`, the misses in a row before this
/// read, which it updates. The panel went only when its process shows no window, or when two
/// reads in a row that gave controls matched no panel: one read that misses (a panel mid-redraw)
/// is no sign. Pure.
fn panel_read(open: Option<bool>, read: bool, shown: bool, misses: &mut u32) -> PanelRead {
    match open {
        Some(enabled) => {
            *misses = 0;
            if enabled {
                PanelRead::Open
            } else {
                PanelRead::Unsure
            }
        }
        None if !shown => PanelRead::Gone,
        None if read => {
            *misses += 1;
            if *misses >= 2 {
                PanelRead::Gone
            } else {
                PanelRead::Unsure
            }
        }
        None => PanelRead::Unsure,
    }
}

/// The app bundle `exe` runs from (`/Applications/aterm.app` for
/// `/Applications/aterm.app/Contents/MacOS/aterm`): `None` for an executable outside one. Pure.
fn bundle_of(exe: &str) -> Option<&str> {
    let (bundle, name) = exe.rsplit_once("/Contents/MacOS/")?;
    (bundle.ends_with(".app") && !name.is_empty() && !name.contains('/')).then_some(bundle)
}

/// A frame's centre, as a record line says it. Pure.
fn centre_words(frame: (f64, f64, f64, f64)) -> String {
    format!(
        "{:.0},{:.0}",
        frame.0 + frame.2 / 2.0,
        frame.1 + frame.3 / 2.0
    )
}

/// Look every [`SWEEP`] until `look` finds something, for `within` at most (once at least).
fn look_for<T>(within: Duration, mut look: impl FnMut() -> Option<T>) -> Option<T> {
    let from = Instant::now();
    loop {
        if let Some(found) = look() {
            return Some(found);
        }
        if from.elapsed() >= within {
            return None;
        }
        std::thread::sleep(SWEEP);
    }
}

/// How long an open panel stands, with no try of the person's running, before the person presses
/// its Cancel ([`DevAct::CancelPanel`]), and between such presses: a panel a try left up (one that
/// came after [`PANEL_WITHIN`], or one a try took for gone) blocks every later add.
const PANEL_STRAY: Duration = Duration::from_secs(10);

/// How long after it first read the page the person waits for the helper panel's tile before its
/// first add goes to + instead of a drag: System Settings can read the page in the sweep before
/// aterm's helper panel is on screen.
const TILE_WITHIN: Duration = Duration::from_secs(20);

/// What the person does on the Developer Tools page in one sweep ([`next`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DevAct {
    Wait,
    /// Press Cancel in an open panel that has stood [`PANEL_STRAY`] with no try running.
    CancelPanel,
    /// Press aterm's switch on the list.
    Switch,
    /// Add aterm to the list ([`add_how`]).
    Add,
}

/// What [`next`] judges a sweep of the Developer Tools page by.
#[derive(Clone, Copy, Debug, Default)]
struct DevSweep {
    /// The page's list read.
    list: bool,
    /// The list holds aterm.
    holds: bool,
    /// aterm's switch on the list reads off.
    switch_off: bool,
    /// A sheet stands in System Settings' window, or the open panel service shows a window.
    sheet_up: bool,
    /// How long an open panel has stood (the person's tries run inside one sweep, so none runs).
    panel_for: Option<Duration>,
    /// The person's adds so far, the last one how long ago.
    adds: usize,
    last_add: Option<Duration>,
    /// How long ago the person last typed a password into System Settings.
    typed: Option<Duration>,
    /// How long ago the person last pressed aterm's switch.
    switched: Option<Duration>,
    /// How long ago the person last pressed a stray panel's Cancel.
    cancelled: Option<Duration>,
}

/// What the person does this sweep, as a person there would: a stray open panel cancelled first
/// (it blocks the page); nothing else while a sheet or panel stands (the switch under a password
/// sheet is not pressed, and a sheet's list is not the page's); once the list holds aterm, its
/// switch pressed while it reads off, once per [`AGAIN`]; while the list lacks aterm, an add when
/// [`may_add`] says so. Pure.
fn next(s: &DevSweep) -> DevAct {
    let fresh = |ago: Option<Duration>, wait: Duration| ago.is_none_or(|d| d >= wait);
    if let Some(stood) = s.panel_for {
        return if stood >= PANEL_STRAY && fresh(s.cancelled, PANEL_STRAY) {
            DevAct::CancelPanel
        } else {
            DevAct::Wait
        };
    }
    if !s.list || s.sheet_up {
        return DevAct::Wait;
    }
    if s.holds {
        return if s.switch_off && fresh(s.switched, AGAIN) {
            DevAct::Switch
        } else {
            DevAct::Wait
        };
    }
    if may_add(s.adds, s.last_add, s.typed) {
        DevAct::Add
    } else {
        DevAct::Wait
    }
}

/// How the person adds aterm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddBy {
    /// The helper panel's tile dragged onto the list.
    Drag,
    /// + below the list and the open panel.
    Plus,
}

/// How the person adds aterm on a try after `adds` tries: by dragging the helper panel's tile on
/// the first try that finds it shown (`tile`) while no earlier try `dragged` it (a drop something
/// took leaves the tile spent, and the panel then says to press +), else with +. `None` on the
/// first try while no tile is shown and the page was read (`page_for` ago) less than
/// [`TILE_WITHIN`] ago: the person waits for the panel. Pure.
fn add_how(adds: usize, dragged: bool, tile: bool, page_for: Duration) -> Option<AddBy> {
    if !dragged && tile {
        Some(AddBy::Drag)
    } else if adds == 0 && !tile && page_for < TILE_WITHIN {
        None
    } else {
        Some(AddBy::Plus)
    }
}

/// The helper panel's tile of aterm as the person found it: the process showing it, its frame and
/// its centre.
struct Tile {
    app: i32,
    frame: (f64, f64, f64, f64),
    centre: cg::Point,
}

/// What the person remembers of System Settings' Developer Tools page across its sweeps: the app
/// it adds (`--app`), its tries, whether one dragged the tile, when it first read the page, since
/// when an open panel has stood, and the page's state it last recorded.
struct Devtools {
    app: Option<String>,
    known: BTreeMap<(u32, u64), bool>,
    adds: Vec<Instant>,
    dragged: bool,
    page_since: Option<Instant>,
    panel_since: Option<Instant>,
    said: Option<String>,
}

impl Devtools {
    fn new(app: Option<&str>) -> Devtools {
        Devtools {
            app: app.map(str::to_owned),
            known: BTreeMap::new(),
            adds: Vec::new(),
            dragged: false,
            page_since: None,
            panel_since: None,
            said: None,
        }
    }

    /// One sweep on the Developer Tools page (`pane`, in `pid`'s `nodes`), as a person there: the
    /// page's state recorded as it changes (`devtools ... pane`), then what [`next`] says: a stray
    /// open panel's Cancel ([`cancel`], `devtools ... stray`), aterm's switch turned on
    /// ([`DEVTOOLS_SWITCH`]), or aterm added ([`Self::add`]). A password macOS asks for is the
    /// person's password path's, in a later sweep.
    #[expect(
        clippy::too_many_arguments,
        reason = "the person's sweep of one page: where, what it read, and what it remembers"
    )]
    fn step(
        &mut self,
        out: &Out,
        pid: i32,
        owner: &str,
        nodes: &[ax::Node],
        pane: &DevtoolsPane,
        pressed: &mut BTreeMap<String, Instant>,
        who: &Credentials,
    ) {
        let said = pane_state(nodes, pane);
        if self.said.as_deref() != Some(said.as_str()) {
            let mut line = format!("{pid} pane {said}");
            if pane.list.is_none() {
                let around = nodes[pane.anchor].parent;
                let range = if nodes.get(around).is_some() {
                    subtree(nodes, around)
                } else {
                    subtree(nodes, pane.anchor)
                };
                line = format!("{line}: {}", describe(nodes, range, who));
            }
            out.line("devtools", &line);
            self.said = Some(said);
        }
        self.page_since.get_or_insert_with(Instant::now);
        let panel = find_panel(pid);
        if panel.is_some() {
            self.panel_since.get_or_insert_with(Instant::now);
        } else {
            self.panel_since = None;
        }
        let ago = |t: Option<&Instant>| t.map(Instant::elapsed);
        let switch_key = format!("{pid} {DEVTOOLS_SWITCH}");
        let cancel_key = format!("{pid} {DEVTOOLS_CANCEL}");
        let sweep = DevSweep {
            list: pane.list.is_some(),
            holds: pane.holds,
            switch_off: pane.switch.is_some_and(|k| nodes[k].value == Some(0)),
            sheet_up: nodes.iter().any(|n| n.role == "AXSheet")
                || cg::on_screen()
                    .unwrap_or_default()
                    .iter()
                    .any(|w| w.owner.starts_with(PANEL_SERVICE)),
            panel_for: self.panel_since.map(|t| t.elapsed()),
            adds: self.adds.len(),
            last_add: ago(self.adds.last()),
            typed: ago(pressed.get(&typed_key(pid))),
            switched: ago(pressed.get(&switch_key)),
            cancelled: ago(pressed.get(&cancel_key)),
        };
        match next(&sweep) {
            DevAct::Wait => {}
            DevAct::CancelPanel => {
                if let Some((panel_pid, panel_owner, panel_nodes, found)) = panel {
                    out.line(
                        "devtools",
                        &format!(
                            "{pid} stray open-panel {panel_pid} {panel_owner}: {}",
                            describe(&panel_nodes, found.dialog.clone(), who)
                        ),
                    );
                    cancel(out, panel_pid, &panel_owner);
                }
                pressed.insert(cancel_key, Instant::now());
            }
            DevAct::Switch => {
                if let Some(switch) = pane.switch {
                    proven(out, "click", || cg::click(nodes[switch].centre()));
                    out.line("click", &format!("{pid} {owner} {DEVTOOLS_SWITCH}"));
                }
                pressed.insert(switch_key, Instant::now());
            }
            DevAct::Add => self.add(out, pid, owner, nodes, pane, who),
        }
    }

    /// One add: by [`add_how`], dragging the helper panel's tile ([`Self::tile`]) onto the list,
    /// else with + ([`plus`]); nothing while the person waits for the tile. The try is counted
    /// ([`ADDS`]) and timed from when it ends.
    fn add(
        &mut self,
        out: &Out,
        pid: i32,
        owner: &str,
        nodes: &[ax::Node],
        pane: &DevtoolsPane,
        who: &Credentials,
    ) {
        let Some(list) = pane.list else {
            return;
        };
        let Some(exe) = self.app.clone() else {
            self.adds.push(Instant::now());
            out.line(
                "devtools",
                &format!("{pid} skip: no --app, so the person knows no aterm to add"),
            );
            return;
        };
        let (tile, running) = self.tile(&exe);
        let page_for = self.page_since.map_or(Duration::ZERO, |t| t.elapsed());
        let Some(how) = add_how(self.adds.len(), self.dragged, tile.is_some(), page_for) else {
            return;
        };
        self.adds.push(Instant::now());
        match (how, tile) {
            (AddBy::Drag, Some(Tile { app, frame, centre })) => {
                self.dragged = true;
                out.line(
                    "devtools",
                    &format!(
                        "{pid} drag tile={app} from={} to={}",
                        centre_words(frame),
                        centre_words(nodes[list].frame)
                    ),
                );
                let to = nodes[list].centre();
                proven(out, "drag", || cg::drag(centre, to));
                out.line("click", &format!("{pid} {owner} {DEVTOOLS_DRAG}"));
            }
            _ => {
                if !self.dragged {
                    out.line(
                        "devtools",
                        &format!(
                            "{pid} no-tile: no window of the app ({running} running) shows the \
                             helper panel's tile"
                        ),
                    );
                }
                plus(out, pid, owner, nodes, pane, &exe, who);
            }
        }
        if let Some(last) = self.adds.last_mut() {
            *last = Instant::now();
        }
    }

    /// The helper panel's tile of aterm, in a window of a process running `exe` ([`app_pids`],
    /// [`devtools_tile`]), `None` when no window shows it; and how many processes run the app.
    fn tile(&mut self, exe: &str) -> (Option<Tile>, usize) {
        let apps = app_pids(exe, &mut self.known).unwrap_or_default();
        let shown: BTreeSet<i32> = cg::on_screen()
            .unwrap_or_default()
            .iter()
            .map(|w| w.pid)
            .filter(|p| u32::try_from(*p).is_ok_and(|p| apps.contains_key(&p)))
            .collect();
        let tile = shown.into_iter().find_map(|app| {
            let tiles = ax::nodes(app);
            devtools_tile(&tiles).map(|k| Tile {
                app,
                frame: tiles[k].frame,
                centre: tiles[k].centre(),
            })
        });
        (tile, apps.len())
    }
}

/// Add aterm with + below the list, as a person does: + pressed, then in the open panel
/// ([`find_panel`]) Command-Shift-G, the app's bundle (from `exe`: [`bundle_of`]) typed over what
/// the Go to field holds once the focus reads it ([`goes_to`]), Return, and Open pressed once it
/// is enabled ([`panel_read`]); each a proven input, recorded (`devtools ...` lines, `click` lines
/// [`DEVTOOLS_PLUS`], [`DEVTOOLS_GOTO`], [`DEVTOOLS_OPEN`]). A try that cannot go on presses
/// the panel's Cancel ([`cancel`], [`DEVTOOLS_CANCEL`]); a panel it leaves up all the same is
/// cancelled by a later sweep ([`DevAct::CancelPanel`]).
fn plus(
    out: &Out,
    pid: i32,
    owner: &str,
    nodes: &[ax::Node],
    pane: &DevtoolsPane,
    exe: &str,
    who: &Credentials,
) {
    let (Some(plus), Some(bundle)) = (pane.plus, bundle_of(exe)) else {
        let why = if pane.plus.is_none() {
            "no + below the list".to_owned()
        } else {
            format!("--app {exe} is in no app bundle")
        };
        out.line("devtools", &format!("{pid} skip: {why}"));
        return;
    };
    proven(out, "click", || cg::click(nodes[plus].centre()));
    out.line("click", &format!("{pid} {owner} {DEVTOOLS_PLUS}"));
    let Some((panel_pid, panel_owner, panel_nodes, panel)) =
        look_for(PANEL_WITHIN, || find_panel(pid))
    else {
        out.line(
            "devtools",
            &format!(
                "{pid} open-panel not seen within {} s of +",
                PANEL_WITHIN.as_secs()
            ),
        );
        return;
    };
    out.line(
        "devtools",
        &format!(
            "{pid} open-panel {panel_pid} {panel_owner}: {}",
            describe(&panel_nodes, panel.dialog.clone(), who)
        ),
    );
    proven(out, "key", cg::go_to_folder);
    if look_for(PANEL_WITHIN, || {
        ax::focused(panel_pid).filter(|f| goes_to(Some(f)))
    })
    .is_none()
    {
        let read = ax::focused(panel_pid).map_or_else(|| "?".to_owned(), |f| part(&f, who));
        out.line(
            "devtools",
            &format!("{pid} skip: the Go to field never took the focus (it read {read})"),
        );
        cancel(out, panel_pid, &panel_owner);
        return;
    }
    proven(out, "key", cg::select_all);
    proven(out, "key", || cg::type_text(bundle));
    proven(out, "key", cg::press_return);
    out.line(
        "click",
        &format!("{panel_pid} {panel_owner} {DEVTOOLS_GOTO}"),
    );
    let mut misses = 0;
    let open = look_for(PANEL_WITHIN, || {
        let now = ax::nodes(panel_pid);
        let found = open_panel(&now);
        let read = panel_read(
            found.as_ref().map(|p| now[p.open].enabled),
            !now.is_empty(),
            on_screen(panel_pid),
            &mut misses,
        );
        match (read, found) {
            (PanelRead::Open, Some(p)) => Some(Some(now[p.open].centre())),
            (PanelRead::Gone, _) => Some(None),
            _ => None,
        }
    });
    match open {
        Some(Some(at)) => {
            proven(out, "click", || cg::click(at));
            out.line(
                "click",
                &format!("{panel_pid} {panel_owner} {DEVTOOLS_OPEN}"),
            );
        }
        Some(None) => out.line(
            "devtools",
            &format!("{pid} open-panel went after the path's Return"),
        ),
        None => {
            out.line(
                "devtools",
                &format!("{pid} skip: the open panel's Open stayed off after the path's Return"),
            );
            cancel(out, panel_pid, &panel_owner);
        }
    }
}

/// Whether `pid` has a window on screen.
fn on_screen(pid: i32) -> bool {
    cg::on_screen().is_some_and(|ws| ws.iter().any(|w| w.pid == pid))
}

/// The open panel System Settings (`settings`) shows after +, in its own windows or in a window of
/// the panel's service ([`PANEL_SERVICE`]): its process, owner name, controls and
/// [`open_panel`].
fn find_panel(settings: i32) -> Option<(i32, String, Vec<ax::Node>, OpenPanel)> {
    let mut owners = vec![(settings, "System Settings".to_owned())];
    for w in cg::on_screen().unwrap_or_default() {
        if w.owner.starts_with(PANEL_SERVICE) && !owners.iter().any(|(p, _)| *p == w.pid) {
            owners.push((w.pid, w.owner.clone()));
        }
    }
    owners.into_iter().find_map(|(pid, owner)| {
        let nodes = ax::nodes(pid);
        let panel = open_panel(&nodes)?;
        Some((pid, owner, nodes, panel))
    })
}

/// Press Cancel in `pid`'s open panel, when it shows one: a person leaves a try that did not go on.
/// A Go to sheet still up in the panel ([`inner_cancel`]) takes the panel's clicks, so its Cancel
/// goes first, and the panel's own once the sheet went (looked for [`PANEL_WITHIN`] at most).
fn cancel(out: &Out, pid: i32, owner: &str) {
    let nodes = ax::nodes(pid);
    let Some(panel) = open_panel(&nodes) else {
        return;
    };
    let own = if let Some(inner) = inner_cancel(&nodes, &panel) {
        proven(out, "click", || cg::click(nodes[inner].centre()));
        out.line("click", &format!("{pid} {owner} {DEVTOOLS_CANCEL}"));
        look_for(PANEL_WITHIN, || {
            let now = ax::nodes(pid);
            let panel = open_panel(&now)?;
            inner_cancel(&now, &panel)
                .is_none()
                .then(|| now[panel.cancel].centre())
        })
    } else {
        Some(nodes[panel.cancel].centre())
    };
    if let Some(at) = own {
        proven(out, "click", || cg::click(at));
        out.line("click", &format!("{pid} {owner} {DEVTOOLS_CANCEL}"));
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join("_")
}

/// A name field's text as one word of a record line: `(empty)` for none, `(unread)` when its
/// value did not read, `(a password)` when it holds one ([`Credentials::holds_password`]).
fn token(text: Option<&str>, who: &Credentials) -> String {
    match text {
        Some(t) if who.holds_password(t) => "(a password)".to_owned(),
        Some(t) if !t.trim().is_empty() => one_line(t),
        Some(_) => "(empty)".to_owned(),
        None => "(unread)".to_owned(),
    }
}

/// The control `pid` has the keyboard focus in, recorded as a `focus` line after `step`.
fn focus(out: &Out, pid: i32, step: &str, who: &Credentials) -> Option<ax::Node> {
    let f = ax::focused(pid);
    out.line(
        "focus",
        &format!(
            "{pid} {step} {}",
            f.as_ref().map_or_else(|| "?".to_owned(), |f| part(f, who))
        ),
    );
    f
}

/// How many clicks the person gives a field of an administrator's dialog before typing into it
/// anyway: in a Golden Gate guest (`lab mac nux --standard-user --person`, 2026-10-28) a click on
/// the System Settings sheet's password field (LocalAuthentication's remote view) left the focus
/// reading the name field once and unreadable once, and the password typed after each was lost
/// (the sheet stayed); the third try's click left it reading the secure field, and that sheet went
/// 812 ms after the idle clock showed its Return.
const CLICKS: u32 = 3;

/// Click `field` until the focus reads in it ([`is_at`]; at most [`CLICKS`] clicks, each a
/// proven input), each reading recorded after `step`, as a person clicks again into a field the
/// caret did not go to: the last reading, for the caller to judge ([`may_type`]).
fn click_into(
    out: &Out,
    pid: i32,
    field: &ax::Node,
    step: &str,
    who: &Credentials,
) -> Option<ax::Node> {
    let mut last = None;
    for _ in 0..CLICKS {
        proven(out, "click", || cg::click(field.centre()));
        last = focus(out, pid, step, who);
        if last.as_ref().is_some_and(|f| is_at(f, field)) {
            break;
        }
    }
    last
}

/// Whether the focus, read as `f`, is in `field`: as secure as it, its frame holding `field`'s
/// centre (the window's search field, and the dialog's other field, are elsewhere). Pure.
fn is_at(f: &ax::Node, field: &ax::Node) -> bool {
    let (x, y, w, h) = f.frame;
    let centre = (
        field.frame.0 + field.frame.2 / 2.0,
        field.frame.1 + field.frame.3 / 2.0,
    );
    f.secure == field.secure
        && w > 0.0
        && h > 0.0
        && (x..=x + w).contains(&centre.0)
        && (y..=y + h).contains(&centre.1)
}

/// Whether `n` takes typed text: a text field (a secure one too), text area or combo box. Pure.
fn takes_text(n: &ax::Node) -> bool {
    matches!(n.role.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox")
}

/// Whether the person types a password with the focus read as `focus`: never into a text field
/// that is not secure, where it would sit in clear (and in the record's next reading of the
/// dialog); the try types nothing and a later sweep clicks again. An unreadable focus (`None`), or
/// one on a control that takes no text, is no sign the keys go astray. Pure.
fn may_type(focus: Option<&ax::Node>) -> bool {
    focus.is_none_or(|f| f.secure || !takes_text(f))
}

/// A key the person posts into a password dialog, each a proven input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key<'a> {
    /// Command-A: select the focused field's text.
    SelectAll,
    /// The text, as key presses.
    Text(&'a str),
    Return,
}

impl Key<'_> {
    fn post(self) {
        match self {
            Key::SelectAll => cg::select_all(),
            Key::Text(text) => cg::type_text(text),
            Key::Return => cg::press_return(),
        }
    }
}

/// The keys of `answer`'s `password`, after the click into the secure field. An administrator's
/// dialog is answered again while it stays ([`TRIES`]), so its secure field may hold an earlier
/// try's keys: Command-A first, so the password replaces them rather than following them. The
/// account's own password, typed once per dialog: the password and Return. Pure.
fn password_keys(answer: Answer, password: &str) -> Vec<Key<'_>> {
    match answer {
        Answer::Account => vec![Key::Text(password), Key::Return],
        Answer::Administrator { .. } => vec![Key::SelectAll, Key::Text(password), Key::Return],
    }
}

/// How long the person watches a password dialog after its Return for the dialog to go.
const GONE_WITHIN: Duration = Duration::from_secs(5);

/// How long after its Return, posted at `returned` ([`proven`]), `pid`'s password dialog (its
/// windows `dialog`) went ([`gone`]), in ms, by [`gone_after`]: `None` when it stayed.
fn went(pid: i32, dialog: &[u64], returned: Instant) -> Option<u128> {
    gone_after(returned, || {
        let listed = cg::on_screen().map(|now| now.iter().map(|w| w.id).collect::<BTreeSet<_>>());
        gone(dialog, listed.as_ref(), &ax::nodes(pid))
    })
}

/// How long after the Return posted at `returned` a look (`look`, whether the dialog went) first
/// saw the dialog gone, in ms, looking every [`SWEEP`] until [`GONE_WITHIN`] after the Return, and
/// once at least (the Return's proof can take longer than that to show): `None` when it stayed.
fn gone_after(returned: Instant, mut look: impl FnMut() -> bool) -> Option<u128> {
    loop {
        std::thread::sleep(SWEEP);
        if look() {
            return Some(returned.elapsed().as_millis());
        }
        if returned.elapsed() >= GONE_WITHIN {
            return None;
        }
    }
}

/// Whether a password dialog whose windows were `dialog` went, by one look: `listed`, the window
/// list's windows (`None`: it did not read), and `nodes`, its app's controls. It went when none of
/// its windows is listed (an app whose only window is the dialog, SecurityAgent's, shows no window
/// and reads no controls once the dialog closes), or when its controls read and hold no secure
/// field (a sheet inside a window that stays: System Settings' LocalAuthentication sheet). A read
/// that gives nothing (a busy app past the messaging timeout) while its windows are listed is no
/// sign it went, nor is a window list that did not read. Pure.
fn gone(dialog: &[u64], listed: Option<&BTreeSet<u64>>, nodes: &[ax::Node]) -> bool {
    let left = listed.is_some_and(|listed| !dialog.iter().any(|id| listed.contains(id)));
    left || (!nodes.is_empty() && !nodes.iter().any(|n| n.secure))
}

/// Whose password the person types into a password dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// The account's own, in the secure field alone.
    Account,
    /// The administrator's, after the administrator's name goes into the name field when it shows
    /// another (`rename`).
    Administrator { rename: bool },
}

impl Answer {
    fn whose(self) -> &'static str {
        match self {
            Answer::Account => "account",
            Answer::Administrator { .. } => "administrator",
        }
    }

    /// Whether the person watches the dialog go after this answer's Return ([`went`]) and answers
    /// it again while it stays ([`TRIES`]): an administrator's dialog, where a loaded guest lost
    /// the keys. The account's own password is typed once per dialog, nothing watched after it.
    fn watched(self) -> bool {
        matches!(self, Answer::Administrator { .. })
    }
}

/// The dialog holding `nodes[at]`: its nearest window or sheet, as the range of the list its
/// nodes take (a node's descendants follow it in the list, depth first). Pure.
fn dialog_around(nodes: &[ax::Node], at: usize) -> std::ops::Range<usize> {
    let mut root = at;
    while let Some(up) = nodes.get(nodes[root].parent) {
        root = nodes[root].parent;
        if up.role == "AXSheet" || up.role == "AXWindow" {
            break;
        }
    }
    subtree(nodes, root)
}

/// The name field of the dialog whose secure field is `nodes[secure]`: its text field that is
/// neither secure nor a search field, the nearest before the secure field, else the first after
/// it. Pure.
fn name_field(nodes: &[ax::Node], secure: usize) -> Option<usize> {
    let dialog = dialog_around(nodes, secure);
    let is_name = |k: &usize| {
        let n = &nodes[*k];
        n.role == "AXTextField" && !n.secure && n.subrole != "AXSearchField"
    };
    (dialog.start..secure)
        .rev()
        .find(is_name)
        .or_else(|| (secure + 1..dialog.end).find(is_name))
}

/// How the person answers the password dialog whose secure field is `nodes[secure]`, and the
/// dialog's name field ([`name_field`]). The account's own password, unless the person has an
/// administrator (`admin`, its name) and the dialog has a name field and asks for an
/// administrator: in its words, or by a name field that shows the administrator or does not show
/// this account (`account`, its names). A prompt for the account's own password has no name
/// field, or one that shows the account. A name field whose text did not read shows neither, so
/// the words alone decide. Pure.
fn answer(
    nodes: &[ax::Node],
    secure: usize,
    account: &[String],
    admin: Option<&str>,
) -> (Answer, Option<usize>) {
    let field = name_field(nodes, secure);
    let (Some(admin), Some(at)) = (admin, field) else {
        return (Answer::Account, field);
    };
    let words = dialog_around(nodes, secure)
        .filter(|&k| k != at && !nodes[k].secure)
        .any(|k| nodes[k].text.to_lowercase().contains("administrator"));
    let Some(shown) = nodes[at].value_text.as_deref().map(str::trim) else {
        let answer = if words {
            Answer::Administrator { rename: true }
        } else {
            Answer::Account
        };
        return (answer, field);
    };
    let shows = |name: &str| !name.trim().is_empty() && shown.eq_ignore_ascii_case(name.trim());
    let answer = if shows(admin) {
        Answer::Administrator { rename: false }
    } else if words || !account.iter().any(|a| shows(a)) {
        Answer::Administrator { rename: true }
    } else {
        Answer::Account
    };
    (answer, field)
}

/// What the person types for `answer`: the name for the dialog's name field (the administrator's,
/// when the answer puts it there: `rename`), and the password, the administrator's for an
/// administrator's answer with one at hand, else the account's own. Pure.
fn credentials(answer: Answer, who: &Credentials) -> (Option<&str>, &str) {
    match (answer, &who.admin) {
        (Answer::Administrator { rename }, Some((admin, password))) => {
            (rename.then_some(admin.as_str()), password.as_str())
        }
        _ => (None, who.password.as_str()),
    }
}

/// A dialog's controls on one line, for the record: each one's role, subrole and text (a secure
/// field's text never, nor a password in another: [`part`]), at most [`DESCRIBED`] of them.
fn describe(nodes: &[ax::Node], dialog: std::ops::Range<usize>, who: &Credentials) -> String {
    let shown = dialog.len().min(DESCRIBED);
    let mut parts: Vec<String> = nodes[dialog.start..dialog.start + shown]
        .iter()
        .map(|n| part(n, who))
        .collect();
    if dialog.len() > shown {
        parts.push(format!("and {} more", dialog.len() - shown));
    }
    parts.join("; ")
}

/// The most controls of a dialog [`describe`] names.
const DESCRIBED: usize = 40;

/// One control for the record: its role, subrole and text, a secure field's text never, nor the
/// text of a control that takes text (or whose role did not read) when its value holds a password
/// ([`Credentials::holds_password`]).
fn part(n: &ax::Node, who: &Credentials) -> String {
    let mut part = n.role.clone();
    if !n.subrole.is_empty() {
        part = format!("{part}/{}", n.subrole);
    }
    let text = if (takes_text(n) || n.role.is_empty())
        && n.value_text
            .as_deref()
            .is_some_and(|v| who.holds_password(v))
    {
        "(a password)".to_owned()
    } else {
        n.text.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    if n.secure {
        part.push_str(" (secure)");
    } else if !text.is_empty() {
        part = format!("{part} {:?}", text.chars().take(80).collect::<String>());
    }
    part
}

fn watch(file: &str, out: &Out, mut kill: Option<(i32, u64)>) -> Result<(), String> {
    // `None` before the first look, so the first text (or its absence) is written too.
    let mut last: Option<Option<String>> = None;
    loop {
        let text = std::fs::read_to_string(file).ok();
        if last.as_ref() != Some(&text) {
            match &text {
                Some(t) => out.line("at", &t.split_whitespace().collect::<Vec<_>>().join(" ")),
                None => out.line("at", "absent"),
            }
            if let (Some((pid, mask)), Some(t)) = (kill, &text) {
                let running = aterm_json::from_str::<aterm_json::Value>(t)
                    .ok()
                    .and_then(|v| v.get("running").and_then(aterm_json::Value::as_u64));
                if let Some(bit) = running.filter(|r| r & mask != 0) {
                    // SAFETY: kill(2) takes any pid and signal number; a gone pid is ESRCH.
                    #[expect(unsafe_code, reason = "kill(2): the one way to send SIGKILL")]
                    let sent = unsafe { libc::kill(pid, libc::SIGKILL) } == 0;
                    out.line(
                        "killed",
                        &format!(
                            "{pid} running={bit} sent={}",
                            if sent { "yes" } else { "no" }
                        ),
                    );
                    kill = None;
                }
            }
            last = Some(text);
        }
        std::thread::sleep(WATCH_EVERY);
    }
}

/// The pids running `exe` with no argument, by the kernel's process table, each with its start
/// (epoch ms): `None` when the table did not read. `known` keeps each process's verdict by its pid
/// and start, so a process's arguments are read once; a process whose arguments did not read (one
/// mid-exec) is read again next time.
fn app_pids(exe: &str, known: &mut BTreeMap<(u32, u64), bool>) -> Option<BTreeMap<u32, u64>> {
    let name = std::path::Path::new(exe)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let records = aterm_sysprobe::kproc::records()?;
    let mut live = BTreeMap::new();
    let mut listed = BTreeSet::new();
    for r in records {
        // The kernel's short name is the image's basename, cut to 16 bytes.
        if r.zombie || r.comm.is_empty() || !name.starts_with(r.comm.as_str()) {
            continue;
        }
        let start = r.start_s * 1000 + u64::from(r.start_us) / 1000;
        listed.insert((r.pid, start));
        let app = match known.get(&(r.pid, start)) {
            Some(&app) => app,
            None => match aterm_sysprobe::kproc::argv(r.pid) {
                Some(argv) => {
                    let app = argv.len() == 1 && argv[0] == exe.as_bytes();
                    known.insert((r.pid, start), app);
                    app
                }
                None => continue,
            },
        };
        if app {
            live.insert(r.pid, start);
        }
    }
    known.retain(|process, _| listed.contains(process));
    Some(live)
}

/// The `procs` verb: a `procs` line, then each process running `exe` with no argument as it
/// appears (`up <pid> start=<epoch ms>`) and goes (`down <pid>`), looked at every [`WATCH_EVERY`].
/// A process table that did not read is a `procs-error` line (once until it reads again).
fn procs(exe: &str, out: &Out) -> Result<(), String> {
    out.line(
        "procs",
        &format!("every={} exe={exe}", WATCH_EVERY.as_millis()),
    );
    let mut known = BTreeMap::new();
    let mut live: BTreeMap<u32, u64> = BTreeMap::new();
    let mut failed = false;
    loop {
        match app_pids(exe, &mut known) {
            None => {
                if !failed {
                    out.line("procs-error", "the kernel's process table did not read");
                }
                failed = true;
            }
            Some(now) => {
                failed = false;
                for (pid, start) in &now {
                    if live.get(pid) != Some(start) {
                        if live.contains_key(pid) {
                            out.line("down", &pid.to_string());
                        }
                        out.line("up", &format!("{pid} start={start}"));
                    }
                }
                for pid in live.keys().filter(|pid| !now.contains_key(pid)) {
                    out.line("down", &pid.to_string());
                }
                live = now;
            }
        }
        std::thread::sleep(WATCH_EVERY);
    }
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CoreFoundation, CoreGraphics and IOKit calls: the window list, posted HID events and \
              the HID idle clock"
)]
mod cg {
    //! The window list and posted HID events (CoreGraphics), the HID idle clock (IOKit), and the
    //! CoreFoundation reads they take.

    use std::ffi::{c_char, c_void};

    use super::Window;

    pub(crate) type CfRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct Point {
        pub x: f64,
        pub y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct Size {
        pub w: f64,
        pub h: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        origin: Point,
        size: Size,
    }

    const UTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;
    const ON_SCREEN_ONLY: u32 = 1;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    const HID_TAP: u32 = 0;
    const MOUSE_MOVED: u32 = 5;
    const LEFT_DOWN: u32 = 1;
    const LEFT_UP: u32 = 2;
    /// `kCGEventLeftMouseDragged`: a move with the left button held.
    const LEFT_DRAGGED: u32 = 6;
    const LEFT: u32 = 0;
    const RETURN_KEY: u16 = 36;
    /// `kVK_ANSI_A`, `kVK_Command`, and `kCGEventFlagMaskCommand`.
    const A_KEY: u16 = 0;
    const COMMAND_KEY: u16 = 55;
    const COMMAND: u64 = 1 << 20;
    /// `kVK_ANSI_G`, `kVK_Shift`, and `kCGEventFlagMaskShift`.
    const G_KEY: u16 = 5;
    const SHIFT_KEY: u16 = 56;
    const SHIFT: u64 = 1 << 17;
    /// How many moves a drag takes from its start to its end, and how many it holds still over
    /// the end before the button goes up, each [`DRAG_STEP`] apart.
    const DRAG_MOVES: u32 = 30;
    const DRAG_HOLDS: u32 = 10;
    const DRAG_STEP: std::time::Duration = std::time::Duration::from_millis(25);

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: CfRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CfRef, at: isize) -> CfRef;
        fn CFDictionaryGetValue(dict: CfRef, key: CfRef) -> CfRef;
        fn CFStringGetCString(s: CfRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFStringCreateWithCString(alloc: CfRef, s: *const c_char, encoding: u32) -> CfRef;
        fn CFNumberGetValue(n: CfRef, kind: isize, out: *mut c_void) -> u8;
        fn CFGetTypeID(v: CfRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFBooleanGetValue(b: CfRef) -> u8;
        fn CFRelease(v: CfRef);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> CfRef;
        fn IOServiceGetMatchingService(main_port: u32, matching: CfRef) -> u32;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: CfRef,
            alloc: CfRef,
            options: u32,
        ) -> CfRef;
        fn IOObjectRelease(object: u32) -> i32;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        static kCGWindowNumber: CfRef;
        static kCGWindowOwnerName: CfRef;
        static kCGWindowOwnerPID: CfRef;
        static kCGWindowLayer: CfRef;
        static kCGWindowBounds: CfRef;
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CfRef;
        fn CGRectMakeWithDictionaryRepresentation(dict: CfRef, rect: *mut Rect) -> bool;
        fn CGEventCreate(source: CfRef) -> CfRef;
        fn CGEventGetLocation(event: CfRef) -> Point;
        fn CGEventCreateMouseEvent(source: CfRef, kind: u32, at: Point, button: u32) -> CfRef;
        fn CGEventCreateKeyboardEvent(source: CfRef, key: u16, down: bool) -> CfRef;
        fn CGEventKeyboardSetUnicodeString(event: CfRef, len: usize, text: *const u16);
        fn CGEventSetFlags(event: CfRef, flags: u64);
        fn CGEventPost(tap: u32, event: CfRef);
        fn CGPreflightPostEventAccess() -> bool;
    }

    /// A CoreFoundation object this code owns (a Create or Copy result), released once.
    pub(crate) struct Owned(pub CfRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a non-null Create/Copy result, released exactly once here.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    /// A CFString of `text` (owned).
    pub(crate) fn string(text: &str) -> Owned {
        let c = std::ffi::CString::new(text).unwrap_or_default();
        // SAFETY: a NUL-terminated UTF-8 string; the result is owned by the caller.
        Owned(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) })
    }

    /// `v` as text when it is a CFString.
    pub(crate) fn text(v: CfRef) -> Option<String> {
        // SAFETY: CFGetTypeID takes any CF object; a null one is refused first.
        if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
            return None;
        }
        let mut buf = vec![0 as c_char; 1024];
        // SAFETY: a CFString and a buffer of the length passed.
        let ok = unsafe { CFStringGetCString(v, buf.as_mut_ptr(), 1024, UTF8) } != 0;
        ok.then(|| {
            // SAFETY: CFStringGetCString wrote a NUL-terminated string into buf.
            unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }

    /// `v` as a whole number when it is a CFNumber or CFBoolean.
    pub(crate) fn number(v: CfRef) -> Option<i64> {
        if v.is_null() {
            return None;
        }
        // SAFETY: CFGetTypeID takes any CF object.
        let kind = unsafe { CFGetTypeID(v) };
        // SAFETY: type ids are plain reads.
        if kind == unsafe { CFBooleanGetTypeID() } {
            // SAFETY: a CFBoolean.
            return Some(i64::from(unsafe { CFBooleanGetValue(v) }));
        }
        // SAFETY: a type id read.
        if kind != unsafe { CFNumberGetTypeID() } {
            return None;
        }
        let mut n: i64 = 0;
        // SAFETY: a CFNumber read into an i64, the kind named.
        let ok = unsafe { CFNumberGetValue(v, SINT64, (&raw mut n).cast()) } != 0;
        ok.then_some(n)
    }

    /// `v`'s items when it is a CFArray (borrowed from it).
    pub(crate) fn items(v: CfRef) -> Vec<CfRef> {
        // SAFETY: CFGetTypeID takes any CF object; a null one is refused first.
        if v.is_null() || unsafe { CFGetTypeID(v) != CFArrayGetTypeID() } {
            return Vec::new();
        }
        // SAFETY: a CFArray, read within its count.
        let n = unsafe { CFArrayGetCount(v) };
        // SAFETY: as above.
        (0..n)
            .map(|i| unsafe { CFArrayGetValueAtIndex(v, i) })
            .collect()
    }

    /// The on-screen windows, `None` when the window server gives no list (no session).
    pub(crate) fn on_screen() -> Option<Vec<Window>> {
        // SAFETY: a Copy call; its result is owned and released by `Owned`.
        let list =
            Owned(unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) });
        if list.0.is_null() {
            return None;
        }
        Some(
            items(list.0)
                .into_iter()
                .filter_map(|dict| {
                    // SAFETY: the keys are CoreGraphics' own constants; each value is borrowed
                    // from the dictionary, which the list holds.
                    let get = |key: CfRef| unsafe { CFDictionaryGetValue(dict, key) };
                    // SAFETY: reads of CoreGraphics' key constants.
                    let (k_number, k_owner, k_pid, k_layer, k_bounds) = unsafe {
                        (
                            kCGWindowNumber,
                            kCGWindowOwnerName,
                            kCGWindowOwnerPID,
                            kCGWindowLayer,
                            kCGWindowBounds,
                        )
                    };
                    let mut rect = Rect::default();
                    let b = get(k_bounds);
                    // SAFETY: a bounds dictionary (or null, refused) and a Rect to fill.
                    let framed = !b.is_null()
                        && unsafe { CGRectMakeWithDictionaryRepresentation(b, &raw mut rect) };
                    Some(Window {
                        id: u64::try_from(number(get(k_number))?).ok()?,
                        pid: i32::try_from(number(get(k_pid))?).ok()?,
                        layer: number(get(k_layer)).unwrap_or(0),
                        owner: text(get(k_owner)).unwrap_or_else(|| "?".to_owned()),
                        frame: if framed {
                            (rect.origin.x, rect.origin.y, rect.size.w, rect.size.h)
                        } else {
                            (0.0, 0.0, 0.0, 0.0)
                        },
                    })
                })
                .collect(),
        )
    }

    /// `HIDIdleTime` (ns since the last HID input) read from `IOHIDSystem` in this process, `None`
    /// unreadable.
    pub(crate) fn hid_idle_ns() -> Option<u64> {
        // SAFETY: a NUL-terminated class name; the matching dictionary it returns is consumed by
        // `IOServiceGetMatchingService` (main port 0 is the default).
        let service =
            unsafe { IOServiceGetMatchingService(0, IOServiceMatching(c"IOHIDSystem".as_ptr())) };
        if service == 0 {
            return None;
        }
        // SAFETY: a NUL-terminated UTF-8 key, owned and released by `Owned`.
        let key = Owned(unsafe {
            CFStringCreateWithCString(std::ptr::null(), c"HIDIdleTime".as_ptr(), UTF8)
        });
        // SAFETY: a live service object and key; the property is a Create result, owned below.
        let value =
            Owned(unsafe { IORegistryEntryCreateCFProperty(service, key.0, std::ptr::null(), 0) });
        // SAFETY: the service object this call got, released once.
        unsafe { IOObjectRelease(service) };
        let mut ns: i64 = 0;
        // SAFETY: `value` checked to be a CFNumber; `ns` is a 64-bit out slot for SINT64.
        let read = !value.0.is_null()
            && unsafe { CFGetTypeID(value.0) == CFNumberGetTypeID() }
            && unsafe { CFNumberGetValue(value.0, SINT64, (&raw mut ns).cast()) } != 0;
        read.then(|| u64::try_from(ns).ok()).flatten()
    }

    /// Whether this process may post events (the event-post grant).
    pub(crate) fn may_post() -> bool {
        // SAFETY: a plain query.
        unsafe { CGPreflightPostEventAccess() }
    }

    fn post(event: &Owned) {
        if !event.0.is_null() {
            // SAFETY: an event this code created.
            unsafe { CGEventPost(HID_TAP, event.0) };
        }
    }

    fn pointer() -> Point {
        // SAFETY: a fresh event, owned and released; its location read.
        let e = Owned(unsafe { CGEventCreate(std::ptr::null()) });
        if e.0.is_null() {
            return Point::default();
        }
        // SAFETY: an event this code created.
        unsafe { CGEventGetLocation(e.0) }
    }

    fn mouse(kind: u32, at: Point) {
        // SAFETY: a fresh event, owned, posted and released.
        post(&Owned(unsafe {
            CGEventCreateMouseEvent(std::ptr::null(), kind, at, LEFT)
        }));
    }

    /// Move the pointer `dx` points sideways.
    pub(crate) fn nudge(dx: f64) {
        let at = pointer();
        mouse(
            MOUSE_MOVED,
            Point {
                x: at.x + dx,
                y: at.y,
            },
        );
    }

    /// Move to `at` and click there.
    pub(crate) fn click(at: Point) {
        mouse(MOUSE_MOVED, at);
        std::thread::sleep(std::time::Duration::from_millis(30));
        mouse(LEFT_DOWN, at);
        std::thread::sleep(std::time::Duration::from_millis(30));
        mouse(LEFT_UP, at);
    }

    /// Press the left button at `from`, move with it held to `to` and let it go there, as a
    /// person drags: a few points first (an `NSDraggingSession` starts once the pointer leaves its
    /// slop), then the way across in [`DRAG_MOVES`] steps, then still over `to` for
    /// [`DRAG_HOLDS`] steps so the place it goes sees the drag enter and settle before the drop.
    pub(crate) fn drag(from: Point, to: Point) {
        mouse(MOUSE_MOVED, from);
        std::thread::sleep(std::time::Duration::from_millis(60));
        mouse(LEFT_DOWN, from);
        std::thread::sleep(std::time::Duration::from_millis(120));
        for dx in [2.0, 4.0, 8.0] {
            mouse(
                LEFT_DRAGGED,
                Point {
                    x: from.x + dx,
                    y: from.y,
                },
            );
            std::thread::sleep(DRAG_STEP);
        }
        for k in 1..=DRAG_MOVES {
            let t = f64::from(k) / f64::from(DRAG_MOVES);
            let at = Point {
                x: from.x + (to.x - from.x) * t,
                y: from.y + (to.y - from.y) * t,
            };
            mouse(LEFT_DRAGGED, at);
            std::thread::sleep(DRAG_STEP);
        }
        for k in 0..DRAG_HOLDS {
            let wiggle = if k % 2 == 0 { 1.0 } else { 0.0 };
            mouse(
                LEFT_DRAGGED,
                Point {
                    x: to.x + wiggle,
                    y: to.y,
                },
            );
            std::thread::sleep(DRAG_STEP * 2);
        }
        mouse(LEFT_UP, to);
    }

    /// `code` going down or up, typing `text` when given, with the modifier `flags` (`None`: the
    /// flags it is made with, the modifiers the HID system holds; `Some`: exactly those).
    fn edge(code: u16, text: Option<u16>, down: bool, flags: Option<u64>) {
        // SAFETY: a fresh event, owned, posted and released; the text is one UTF-16 unit.
        let e = Owned(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, down) });
        if let Some(unit) = text
            && !e.0.is_null()
        {
            // SAFETY: an event this code created and one UTF-16 unit.
            unsafe { CGEventKeyboardSetUnicodeString(e.0, 1, &raw const unit) };
        }
        if let Some(flags) = flags
            && !e.0.is_null()
        {
            // SAFETY: an event this code created.
            unsafe { CGEventSetFlags(e.0, flags) };
        }
        post(&e);
        std::thread::sleep(std::time::Duration::from_millis(15));
    }

    /// A press of `code`, typing `text` when given, with the modifier `flags` held (0: none).
    fn key(code: u16, text: Option<u16>, flags: u64) {
        for down in [true, false] {
            edge(code, text, down, (flags != 0).then_some(flags));
        }
    }

    /// Type `text` as posted key presses.
    pub(crate) fn type_text(text: &str) {
        for unit in text.encode_utf16() {
            key(0, Some(unit), 0);
        }
    }

    /// Command-A, as a person presses it: Command down, A, Command up (its flags none), so no
    /// Command is left held for the keys after it, which take the modifiers the HID system holds
    /// (in a Golden Gate guest, 2026-10-28, its modifier state read Command held after the A and
    /// not after the release; a Command-A without the release lost every key typed after it).
    pub(crate) fn select_all() {
        edge(COMMAND_KEY, None, true, Some(COMMAND));
        key(A_KEY, None, COMMAND);
        edge(COMMAND_KEY, None, false, Some(0));
    }

    pub(crate) fn press_return() {
        key(RETURN_KEY, None, 0);
    }

    /// Command-Shift-G, an open panel's Go to Folder, as a person presses it: Command and Shift
    /// down, G, then both up (their flags none at the last), as [`select_all`] does Command.
    pub(crate) fn go_to_folder() {
        edge(COMMAND_KEY, None, true, Some(COMMAND));
        edge(SHIFT_KEY, None, true, Some(COMMAND | SHIFT));
        key(G_KEY, None, COMMAND | SHIFT);
        edge(SHIFT_KEY, None, false, Some(COMMAND));
        edge(COMMAND_KEY, None, false, Some(0));
    }
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "the Accessibility API: reading another app's dialogs"
)]
mod ax {
    //! Another app's controls, read through the Accessibility API.

    use std::ffi::c_void;

    use super::cg::{self, CfRef, Owned, Point, Size};

    const POINT: u32 = 1;
    const SIZE: u32 = 2;
    /// How deep a window's tree is read.
    const DEPTH: usize = 12;
    /// The most controls read from one app in a pass.
    const MOST: usize = 4000;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> CfRef;
        fn AXUIElementCopyAttributeValue(el: CfRef, attr: CfRef, out: *mut CfRef) -> i32;
        fn AXUIElementSetMessagingTimeout(el: CfRef, secs: f32) -> i32;
        fn AXValueGetValue(v: CfRef, kind: u32, out: *mut c_void) -> bool;
    }

    /// One control of a dialog.
    pub(crate) struct Node {
        pub role: String,
        pub subrole: String,
        /// Its title, description and value text, joined.
        pub text: String,
        pub value: Option<i64>,
        /// Its value when that is text (a text field's), a secure field's never read.
        pub value_text: Option<String>,
        pub enabled: bool,
        pub secure: bool,
        /// The index of its parent in the pass's list.
        pub parent: usize,
        pub frame: (f64, f64, f64, f64),
    }

    impl Node {
        pub(crate) fn centre(&self) -> Point {
            Point {
                x: self.frame.0 + self.frame.2 / 2.0,
                y: self.frame.1 + self.frame.3 / 2.0,
            }
        }
    }

    pub(crate) fn trusted() -> bool {
        // SAFETY: a plain query.
        unsafe { AXIsProcessTrusted() }
    }

    fn attr(el: CfRef, name: &str) -> Owned {
        let key = cg::string(name);
        let mut out: CfRef = std::ptr::null();
        // SAFETY: an AX element, an attribute name and an out pointer; the value is owned.
        let err = unsafe { AXUIElementCopyAttributeValue(el, key.0, &raw mut out) };
        Owned(if err == 0 { out } else { std::ptr::null() })
    }

    fn frame(el: CfRef) -> (f64, f64, f64, f64) {
        let (mut p, mut s) = (Point::default(), Size::default());
        let at = attr(el, "AXPosition");
        let size = attr(el, "AXSize");
        // SAFETY: AXValues of the kinds named (or null, refused), read into matching structs.
        unsafe {
            if !at.0.is_null() {
                AXValueGetValue(at.0, POINT, (&raw mut p).cast());
            }
            if !size.0.is_null() {
                AXValueGetValue(size.0, SIZE, (&raw mut s).cast());
            }
        }
        (p.x, p.y, s.w, s.h)
    }

    /// `pid`'s windows' controls, depth first, each with its parent.
    pub(crate) fn nodes(pid: i32) -> Vec<Node> {
        // SAFETY: an app element for a pid, owned; a short timeout for a busy app.
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return Vec::new();
        }
        // SAFETY: the element this code created.
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.5) };
        let windows = attr(app.0, "AXWindows");
        let mut out = Vec::new();
        for w in cg::items(windows.0) {
            walk(w, usize::MAX, 0, &mut out);
        }
        out
    }

    /// The control `pid` has the keyboard focus in (`AXFocusedUIElement`), alone (no children).
    pub(crate) fn focused(pid: i32) -> Option<Node> {
        // SAFETY: an app element for a pid, owned; a short timeout for a busy app.
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return None;
        }
        // SAFETY: the element this code created.
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.5) };
        let el = attr(app.0, "AXFocusedUIElement");
        if el.0.is_null() {
            return None;
        }
        let mut out = Vec::new();
        // At the deepest level read, so its children are not.
        walk(el.0, usize::MAX, DEPTH, &mut out);
        out.into_iter().next()
    }

    fn walk(el: CfRef, parent: usize, depth: usize, out: &mut Vec<Node>) {
        if depth > DEPTH || out.len() >= MOST {
            return;
        }
        let role = cg::text(attr(el, "AXRole").0).unwrap_or_default();
        let subrole = cg::text(attr(el, "AXSubrole").0).unwrap_or_default();
        let value = attr(el, "AXValue");
        let text = ["AXTitle", "AXDescription"]
            .iter()
            .filter_map(|a| cg::text(attr(el, a).0))
            .chain(cg::text(value.0))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let at = out.len();
        let secure = subrole == "AXSecureTextField";
        out.push(Node {
            secure,
            enabled: cg::number(attr(el, "AXEnabled").0) != Some(0),
            value: cg::number(value.0),
            value_text: if secure { None } else { cg::text(value.0) },
            frame: frame(el),
            role,
            subrole,
            text,
            parent,
        });
        let children = attr(el, "AXChildren");
        for child in cg::items(children.0) {
            walk(child, at, depth + 1, out);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod cg {
    //! Elsewhere there is no window list and nothing is posted.

    use super::Window;

    #[derive(Clone, Copy, Default)]
    pub(crate) struct Point;

    pub(crate) fn on_screen() -> Option<Vec<Window>> {
        None
    }
    pub(crate) fn may_post() -> bool {
        false
    }
    pub(crate) fn hid_idle_ns() -> Option<u64> {
        None
    }
    pub(crate) fn nudge(_dx: f64) {}
    pub(crate) fn click(_at: Point) {}
    pub(crate) fn drag(_from: Point, _to: Point) {}
    pub(crate) fn go_to_folder() {}
    pub(crate) fn select_all() {}
    pub(crate) fn type_text(_text: &str) {}
    pub(crate) fn press_return() {}
}

#[cfg(not(target_os = "macos"))]
mod ax {
    //! Elsewhere no other app's controls are read.

    use super::cg::Point;

    pub(crate) struct Node {
        pub role: String,
        pub subrole: String,
        pub text: String,
        pub value: Option<i64>,
        pub value_text: Option<String>,
        pub enabled: bool,
        pub secure: bool,
        pub parent: usize,
        pub frame: (f64, f64, f64, f64),
    }

    impl Node {
        pub(crate) fn centre(&self) -> Point {
            Point
        }
    }

    pub(crate) fn trusted() -> bool {
        false
    }
    pub(crate) fn nodes(_pid: i32) -> Vec<Node> {
        Vec::new()
    }
    pub(crate) fn focused(_pid: i32) -> Option<Node> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_person_waits_for_the_idle_clock_to_run_its_quiet_before_an_input() {
        let ns = |d: Duration| Some(u64::try_from(d.as_nanos()).expect("in range"));
        assert_eq!(
            quiet_left(ns(Duration::from_millis(350))),
            Some(QUIET_BEFORE - Duration::from_millis(350)),
            "just after a reset: the rest of the quiet"
        );
        assert_eq!(quiet_left(Some(0)), Some(QUIET_BEFORE));
        assert_eq!(quiet_left(ns(QUIET_BEFORE)), None, "quiet enough");
        assert_eq!(quiet_left(ns(Duration::from_secs(120))), None);
        assert_eq!(
            quiet_left(None),
            None,
            "an unreadable clock is not waited on"
        );
    }

    /// A control under `parent`: a text field's text is its value.
    fn node(role: &str, subrole: &str, text: &str, parent: usize) -> ax::Node {
        let field = role == "AXTextField";
        ax::Node {
            role: role.to_owned(),
            subrole: subrole.to_owned(),
            text: text.to_owned(),
            value: None,
            value_text: (field && subrole != "AXSecureTextField").then(|| text.to_owned()),
            enabled: true,
            secure: subrole == "AXSecureTextField",
            parent,
            frame: (0.0, 0.0, 0.0, 0.0),
        }
    }

    /// A System Settings window with a search field, the authorization sheet a standard account's
    /// Full Disk Access switch raised in a Golden Gate guest (`lab mac nux --standard-user
    /// --person`, 2026-10-28: its controls as the person described them, with `words` for its
    /// second line and its name field showing `name`, `None` for no name field) and a text field
    /// of the pane behind the sheet.
    fn sheet(words: &str, name: Option<&str>) -> (Vec<ax::Node>, usize) {
        let mut nodes = vec![
            node(
                "AXWindow",
                "AXStandardWindow",
                "System Settings",
                usize::MAX,
            ),
            node("AXTextField", "AXSearchField", "", 0),
            node("AXSheet", "", "", 0),
            node("AXGroup", "AXHostingView", "", 2),
            node("AXImage", "", "Lock", 3),
            node(
                "AXStaticText",
                "",
                "Privacy & Security is trying to modify your system settings.",
                3,
            ),
            node("AXStaticText", "", words, 3),
        ];
        if let Some(name) = name {
            nodes.push(node("AXTextField", "", name, 3));
        }
        let secure = nodes.len();
        nodes.push(node("AXTextField", "AXSecureTextField", "•••", 3));
        nodes.push(node("AXButton", "", "Modify Settings", 3));
        nodes.push(node("AXButton", "", "Cancel", 3));
        nodes.push(node("AXButton", "", "General", 0));
        nodes.push(node("AXTextField", "", "Full Disk Access", 0));
        (nodes, secure)
    }

    /// A System Settings window with `buttons` (each a title and whether it is enabled) under the
    /// words of the sheet macOS shows once an access switch for a running app changed, in an
    /// `AXSheet` (`in_sheet`) or straight in a group of the window, and the pane's aterm switch
    /// behind it. The words are macOS's form of them, not a recording: no row had read the sheet
    /// before this fixture was written.
    fn reopen_fixture(buttons: &[(&str, bool)], in_sheet: bool) -> Vec<ax::Node> {
        let mut nodes = vec![
            node(
                "AXWindow",
                "AXStandardWindow",
                "System Settings",
                usize::MAX,
            ),
            node("AXTextField", "AXSearchField", "", 0),
            node(if in_sheet { "AXSheet" } else { "AXGroup" }, "", "", 0),
            node(
                "AXStaticText",
                "",
                "“aterm” won’t have full disk access until it is quit.",
                2,
            ),
            node(
                "AXStaticText",
                "",
                "You can choose to quit “aterm” now, or do it on your own later.",
                2,
            ),
        ];
        for (title, enabled) in buttons {
            let mut button = node("AXButton", "", title, 2);
            button.enabled = *enabled;
            nodes.push(button);
        }
        nodes.push(node("AXCheckBox", "AXSwitch", "aterm", 0));
        nodes
    }

    #[test]
    fn the_person_finds_the_quit_and_reopen_sheet_by_its_role_and_its_buttons() {
        let who = standard();
        let nodes = reopen_fixture(&[("Later", true), ("Quit & Reopen", true)], true);
        let found = reopen_sheet(&nodes).expect("the sheet");
        assert_eq!(
            found,
            ReopenSheet {
                dialog: 2..7,
                reopen: 6,
                later: 5
            }
        );
        assert_eq!(
            describe(&nodes, found.dialog.clone(), &who),
            "AXSheet; AXStaticText \"“aterm” won’t have full disk access until it is quit.\"; \
             AXStaticText \"You can choose to quit “aterm” now, or do it on your own later.\"; \
             AXButton \"Later\"; AXButton \"Quit & Reopen\""
        );
        assert_eq!(found.button(Sheet::Reopen), 6, "the default answer");
        assert_eq!(found.button(Sheet::Later), 5, "--sheet later");
        // The host reads the press by these words (`mac_nux_guest::SHEET_PRESSES`).
        assert_eq!(one_line(Sheet::Reopen.title()), "Quit_&_Reopen");
        assert_eq!(one_line(Sheet::Later.title()), "Later");
        assert!(
            reopen_sheet(&reopen_fixture(
                &[("later", true), ("Quit  &  Reopen", true)],
                true
            ))
            .is_some(),
            "spacing and case aside"
        );
        // Negative controls: each one thing short of the sheet.
        for (why, buttons, in_sheet) in [
            (
                "the buttons in a window, no sheet",
                &[("Later", true), ("Quit & Reopen", true)][..],
                false,
            ),
            ("no Later", &[("Quit & Reopen", true)][..], true),
            ("no Quit & Reopen", &[("Later", true)][..], true),
            ("a Quit alone", &[("Later", true), ("Quit", true)][..], true),
            (
                "Quit & Reopen disabled",
                &[("Later", true), ("Quit & Reopen", false)][..],
                true,
            ),
            (
                "Later disabled",
                &[("Later", false), ("Quit & Reopen", true)][..],
                true,
            ),
        ] {
            assert_eq!(
                reopen_sheet(&reopen_fixture(buttons, in_sheet)),
                None,
                "{why}"
            );
        }
        let (password, _) = sheet("Enter your password to continue.", None);
        assert_eq!(
            reopen_sheet(&password),
            None,
            "the authorization sheet is not it"
        );
    }

    /// The app's alert with `words` and `buttons` (each a title and whether it is enabled), in its
    /// own window (an `AXWindow`, as `NSAlert`'s modal run shows it), beside the app's terminal
    /// window. The words are `quit_safety::confirm_prompt`'s for a quit, not a recording: no row
    /// had read the alert before this fixture was written.
    fn quit_fixture(words: &str, buttons: &[(&str, bool)]) -> Vec<ax::Node> {
        let mut nodes = vec![
            node("AXWindow", "AXStandardWindow", "aterm", usize::MAX),
            // The terminal window's own Quit-titled control is no alert: its window does not read
            // the question.
            node("AXButton", "", "Quit", 0),
            node("AXWindow", "AXDialog", "", usize::MAX),
            node("AXImage", "", "", 2),
            node("AXStaticText", "", words, 2),
            node(
                "AXStaticText",
                "",
                "Every shell ends and its scrollback is lost; windows and tabs come back when \
                 aterm starts.",
                2,
            ),
        ];
        for (title, enabled) in buttons {
            let mut button = node("AXButton", "", title, 2);
            button.enabled = *enabled;
            nodes.push(button);
        }
        nodes
    }

    #[test]
    fn the_person_answers_the_apps_quit_question_only_after_its_own_quit_and_reopen() {
        let who = standard();
        let asked = quit_fixture(QUIT_QUESTION, &[("Quit", true), ("Cancel", true)]);
        let found = quit_question(&asked).expect("the question");
        assert_eq!(
            found,
            QuitQuestion {
                dialog: 2..8,
                quit: 6
            }
        );
        assert_eq!(
            describe(&asked, found.dialog.clone(), &who),
            "AXWindow/AXDialog; AXImage; AXStaticText \"Are you sure you want to quit aterm?\"; \
             AXStaticText \"Every shell ends and its scrollback is lost; windows and tabs come back \
             when ate\"; AXButton \"Quit\"; AXButton \"Cancel\"",
            "each control's text cut at 80 characters"
        );
        let mut twice = quit_fixture(QUIT_QUESTION, &[("Cancel", true), ("quit", true)]);
        twice[4].text = format!("{QUIT_QUESTION} {QUIT_QUESTION}");
        assert_eq!(
            quit_question(&twice).map(|q| q.quit),
            Some(7),
            "case aside, the question read from its title and its value"
        );
        // Negative controls: each one thing short of the question.
        for (why, words, buttons) in [
            (
                "a Cancel-only alert",
                QUIT_QUESTION,
                &[("Cancel", true)][..],
            ),
            ("no Cancel", QUIT_QUESTION, &[("Quit", true)][..]),
            (
                "Quit disabled",
                QUIT_QUESTION,
                &[("Quit", false), ("Cancel", true)][..],
            ),
            (
                "Cancel disabled",
                QUIT_QUESTION,
                &[("Quit", true), ("Cancel", false)][..],
            ),
            (
                "another question",
                "A process is still running in this window.",
                &[("Quit", true), ("Cancel", true)][..],
            ),
            (
                "more words than the question",
                "Are you sure you want to quit aterm? It is not.",
                &[("Quit", true), ("Cancel", true)][..],
            ),
            (
                "the app's own page: its Continue and Open",
                QUIT_QUESTION,
                &[("Continue", true), ("Open", true), ("Cancel", true)][..],
            ),
            (
                "a Quit & Reopen, not a Quit",
                QUIT_QUESTION,
                &[("Quit & Reopen", true), ("Cancel", true)][..],
            ),
        ] {
            assert_eq!(quit_question(&quit_fixture(words, buttons)), None, "{why}");
        }
        assert_eq!(
            quit_question(&reopen_fixture(
                &[("Later", true), ("Quit & Reopen", true)],
                true
            )),
            None,
            "System Settings' sheet is not the app's question"
        );
        // When: within a minute of the person's own Quit & Reopen press, and never with Later.
        let soon = Some(Duration::from_secs(6));
        assert!(answers_quit(Sheet::Reopen, soon));
        assert!(!answers_quit(Sheet::Reopen, None), "no earlier sheet press");
        assert!(!answers_quit(Sheet::Later, soon), "--sheet later");
        assert!(!answers_quit(Sheet::Reopen, Some(QUIT_ASKED_WITHIN)));
        let at = Instant::now();
        let mut pressed: BTreeMap<String, Instant> = [
            ("812 sheet Later".to_owned(), at),
            ("812 Allow".to_owned(), at),
            ("812 switch 10,20".to_owned(), at),
            ("793 quit-confirm".to_owned(), at),
        ]
        .into();
        assert_eq!(
            reopen_pressed(&pressed),
            None,
            "a Later, an Allow, a switch and the question itself are no Quit & Reopen press"
        );
        pressed.insert(sheet_key(812, Sheet::Reopen), at);
        assert_eq!(reopen_pressed(&pressed), Some(at));
        // The person's ordinary passes never act in the app: its own buttons are pressed only here.
        assert!(!ACTS_ON.iter().any(|o| o.to_lowercase().contains("aterm")));
        assert!(!ALLOW.contains(&"Quit"));
    }

    /// `n` with its frame at `frame` (x, y, width, height; top-left origin, as AX gives it).
    fn framed(mut n: ax::Node, frame: (f64, f64, f64, f64)) -> ax::Node {
        n.frame = frame;
        n
    }

    /// System Settings' window on a page with words `words` above a list holding `rows` (each an
    /// app's name and its switch's value), its window titled `title`, with the sidebar's outline
    /// at the left, the buttons `buttons` (each a title) just below the list, and the page's
    /// Processor Trace switch, off, below them. The words and layout are macOS's form of the
    /// Developer Tools page, not a recording: no row had read the page before this fixture was
    /// written.
    fn devtools_fixture(
        title: &str,
        words: &str,
        rows: &[(&str, i64)],
        buttons: &[&str],
    ) -> Vec<ax::Node> {
        let mut nodes = vec![
            framed(
                node("AXWindow", "AXStandardWindow", title, usize::MAX),
                (100.0, 50.0, 715.0, 600.0),
            ),
            framed(node("AXGroup", "", "", 0), (100.0, 50.0, 715.0, 600.0)),
            // The sidebar: an outline whose rows name the pages, Privacy & Security among them.
            framed(
                node("AXScrollArea", "", "", 1),
                (100.0, 100.0, 200.0, 540.0),
            ),
            framed(node("AXOutline", "", "", 2), (100.0, 100.0, 200.0, 540.0)),
            node("AXRow", "", "", 3),
            node("AXStaticText", "", "Privacy & Security", 4),
            // The page, in a scroll area that starts above its words.
            framed(node("AXScrollArea", "", "", 1), (310.0, 90.0, 500.0, 550.0)),
            framed(node("AXHeading", "", title, 6), (330.0, 100.0, 200.0, 20.0)),
            framed(
                node("AXStaticText", "", words, 6),
                (330.0, 130.0, 460.0, 32.0),
            ),
            framed(
                node("AXScrollArea", "", "", 6),
                (330.0, 170.0, 460.0, 120.0),
            ),
            framed(node("AXTable", "", "", 9), (330.0, 170.0, 460.0, 120.0)),
        ];
        let mut y = 170.0;
        for (name, on) in rows {
            let row = nodes.len();
            nodes.push(framed(node("AXRow", "", "", 10), (330.0, y, 460.0, 30.0)));
            nodes.push(node("AXCell", "", "", row));
            nodes.push(node("AXImage", "", "", row + 1));
            nodes.push(node("AXStaticText", "", name, row + 1));
            nodes.push(node("AXCell", "", "", row));
            let mut switch = framed(
                node("AXCheckBox", "AXSwitch", "", row + 4),
                (750.0, y + 5.0, 30.0, 20.0),
            );
            switch.value = Some(*on);
            nodes.push(switch);
            y += 30.0;
        }
        let mut x = 332.0;
        for title in buttons {
            nodes.push(framed(
                node("AXButton", "", title, 6),
                (x, 292.0, 22.0, 22.0),
            ));
            x += 24.0;
        }
        nodes.push(framed(
            node("AXStaticText", "", "Processor Trace", 6),
            (330.0, 340.0, 200.0, 20.0),
        ));
        let mut trace = framed(
            node("AXCheckBox", "AXSwitch", "Processor Trace", 6),
            (750.0, 340.0, 30.0, 20.0),
        );
        trace.value = Some(0);
        nodes.push(trace);
        nodes
    }

    const DEVTOOLS_LINE: &str = "Allow the applications below to run software locally that does \
                                 not meet the system’s security policy.";

    #[test]
    fn the_person_finds_the_developer_tools_page_its_list_and_the_plus_below_it() {
        let lacks = devtools_fixture(
            "Developer Tools",
            DEVTOOLS_LINE,
            &[("Terminal", 1)],
            &["Add", "Remove"],
        );
        let pane = devtools_pane(&lacks).expect("the page");
        assert_eq!(
            pane,
            DevtoolsPane {
                anchor: 8,
                list: Some(9),
                holds: false,
                switch: None,
                plus: Some(17),
            },
            "the list's scroll area, under the words; Terminal's switch is not aterm's"
        );
        assert_eq!(lacks[17].text, "Add");
        assert_eq!(pane_state(&lacks, &pane), "list=lacks switch=none");
        // aterm on the list, its switch off: its row's switch, not Terminal's or Processor Trace's.
        let holds = devtools_fixture(
            "Developer Tools",
            DEVTOOLS_LINE,
            &[("Terminal", 1), ("aterm", 0)],
            &["+", "−"],
        );
        let pane = devtools_pane(&holds).expect("the page");
        assert!(pane.holds);
        let switch = pane.switch.expect("aterm's switch");
        assert_eq!(holds[holds[switch].parent].role, "AXCell");
        assert_eq!(holds[switch].frame.1, 205.0, "the second row's");
        assert_eq!(pane.plus.map(|k| holds[k].text.as_str()), Some("+"));
        assert_eq!(pane_state(&holds, &pane), "list=holds switch=off");
        // Untitled + and −: the left one.
        let bare = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["", ""]);
        let plus = devtools_pane(&bare).and_then(|p| p.plus).expect("a +");
        assert_eq!(bare[plus].frame.0, 332.0);
        // Words that read otherwise: the window's title and its heading.
        let titled = devtools_fixture("Developer Tools", "", &[("Terminal", 1)], &["Add"]);
        assert_eq!(
            devtools_pane(&titled).map(|p| (p.anchor, p.list)),
            Some((7, Some(9)))
        );
        assert!(names_aterm("“aterm”") && names_aterm("aterm.app") && names_aterm("ATERM"));
        // Negative controls.
        assert!(!names_aterm("aterms") && !names_aterm("Waterm") && !names_aterm("aterm-lab"));
        let fda = devtools_fixture(
            "Full Disk Access",
            "Allow the applications below to access data like Mail, Messages, Safari, Home, Time \
             Machine backups and certain administrative settings for all users on this Mac.",
            &[("aterm", 0)],
            &["Add", "Remove"],
        );
        assert_eq!(devtools_pane(&fda), None, "Full Disk Access is not it");
        let mut privacy = devtools_fixture("Privacy & Security", "", &[], &[]);
        privacy.push(node("AXButton", "", "Developer Tools", 6));
        privacy.push(node("AXStaticText", "", "Developer Tools", 6));
        assert_eq!(
            devtools_pane(&privacy),
            None,
            "Privacy & Security's own row named Developer Tools is not it"
        );
        let removes = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["Remove", "−"]);
        assert_eq!(
            devtools_pane(&removes).map(|p| p.plus),
            Some(None),
            "a titled Remove or − is never +"
        );
        let mut far = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["Add"]);
        let add = far.len() - 3;
        far[add].frame.1 = 600.0;
        assert_eq!(
            devtools_pane(&far).map(|p| p.plus),
            Some(None),
            "an Add far below the list"
        );
        let mut disabled = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["Add"]);
        let add = disabled.len() - 3;
        disabled[add].enabled = false;
        assert_eq!(devtools_pane(&disabled).map(|p| p.plus), Some(None));
        // No list under the words: the page reads, its list does not.
        let mut listless = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &[]);
        listless.truncate(9);
        let pane = devtools_pane(&listless).expect("the page");
        assert_eq!((pane.list, pane.plus), (None, None));
        assert_eq!(pane_state(&listless, &pane), "list=unread switch=none");
    }

    /// aterm's windows: its terminal window with its alert's icon, and its helper panel headed
    /// `heading` with the tile of aterm (an `AXImage` labelled aterm, as
    /// `aterm_gui::devtools_helper`'s tile sets it) at `tile`.
    fn helper_fixture(heading: &str, tile: (f64, f64, f64, f64)) -> Vec<ax::Node> {
        vec![
            node("AXWindow", "AXStandardWindow", "aterm", usize::MAX),
            framed(
                node("AXImage", "", "aterm alert", 0),
                (10.0, 10.0, 64.0, 64.0),
            ),
            node("AXWindow", "AXFloatingWindow", "aterm", usize::MAX),
            node("AXStaticText", "", heading, 2),
            node("AXStaticText", "", "Step 1 of 2", 2),
            framed(node("AXImage", "", "aterm", 2), tile),
            node(
                "AXStaticText",
                "",
                "Drag aterm into the Developer Tools list",
                2,
            ),
        ]
    }

    #[test]
    fn the_person_drags_aterm_only_from_the_helper_panels_developer_tools_page() {
        let shown = (850.0, 200.0, 64.0, 64.0);
        let nodes = helper_fixture(DEVTOOLS_HELPER_TITLE, shown);
        assert_eq!(devtools_tile(&nodes), Some(5));
        assert_eq!(centre_words(nodes[5].frame), "882,232");
        // Negative controls: the Full Disk Access page's tile, a tile with no frame, the app's
        // alert icon.
        assert_eq!(
            devtools_tile(&helper_fixture("Turn on Full Disk Access for aterm", shown)),
            None
        );
        assert_eq!(
            devtools_tile(&helper_fixture(DEVTOOLS_HELPER_TITLE, (0.0, 0.0, 0.0, 0.0))),
            None
        );
        let mut alert_only = helper_fixture(DEVTOOLS_HELPER_TITLE, shown);
        alert_only[5].text = "aterm alert".to_owned();
        assert_eq!(devtools_tile(&alert_only), None);
        // The person adds at most ADDS times, each ADD_SETTLE after the last and after a password
        // typed since it.
        let s = |secs| Some(Duration::from_secs(secs));
        assert!(may_add(0, None, None));
        assert!(
            may_add(0, None, s(1)),
            "the Full Disk Access password before the page"
        );
        assert!(!may_add(1, s(5), None), "too soon after a try");
        assert!(may_add(1, s(25), None));
        assert!(!may_add(1, s(25), s(5)), "too soon after a password");
        assert!(may_add(2, s(25), s(21)));
        assert!(!may_add(ADDS, s(600), None), "no more tries");
    }

    /// An open panel as a sheet of System Settings' window: its file view, a Go to field and its
    /// buttons (each a title and whether it is enabled). macOS's form, not a recording.
    fn panel_fixture(view: &str, buttons: &[(&str, bool)]) -> Vec<ax::Node> {
        let mut nodes = vec![
            node(
                "AXWindow",
                "AXStandardWindow",
                "Developer Tools",
                usize::MAX,
            ),
            node("AXSheet", "", "", 0),
            node("AXGroup", "AXHostingView", "", 1),
            node("AXPopUpButton", "", "Applications", 2),
            node(view, "", "", 2),
            node("AXTextField", "AXSearchField", "", 2),
        ];
        for (title, enabled) in buttons {
            let mut button = node("AXButton", "", title, 2);
            button.enabled = *enabled;
            nodes.push(button);
        }
        nodes
    }

    #[test]
    fn the_person_finds_the_open_panel_and_types_the_path_only_in_a_field_in_clear() {
        let panel = panel_fixture("AXBrowser", &[("Cancel", true), ("Open", false)]);
        assert_eq!(
            open_panel(&panel),
            Some(OpenPanel {
                dialog: 1..8,
                open: 7,
                cancel: 6
            }),
            "Open is off until something is chosen"
        );
        assert!(
            open_panel(&panel_fixture(
                "AXOutline",
                &[("Open", true), ("Cancel", true)]
            ))
            .is_some()
        );
        // Negative controls: Gatekeeper's alert has Open and Cancel and no files; Cancel off; no
        // Cancel; the app's quit question.
        assert_eq!(
            open_panel(&panel_fixture(
                "AXStaticText",
                &[("Cancel", true), ("Open", true)]
            )),
            None
        );
        assert_eq!(
            open_panel(&panel_fixture(
                "AXBrowser",
                &[("Cancel", false), ("Open", true)]
            )),
            None
        );
        assert_eq!(
            open_panel(&panel_fixture("AXBrowser", &[("Open", true)])),
            None
        );
        assert_eq!(
            open_panel(&quit_fixture(
                QUIT_QUESTION,
                &[("Quit", true), ("Cancel", true), ("Open", true)]
            )),
            None,
            "the quit question, were it to show an Open: no files"
        );
        assert_eq!(
            open_panel(&devtools_fixture(
                "Developer Tools",
                DEVTOOLS_LINE,
                &[],
                &["Add", "Open"]
            )),
            None,
            "the page itself, were it to show an Open: its table is no panel's, with no Cancel"
        );
        let field = node("AXTextField", "", "/Applications", 0);
        assert!(goes_to(Some(&field)));
        assert!(goes_to(Some(&node("AXComboBox", "", "", 0))));
        // Negative controls: a secure field, a button, an unreadable focus.
        assert!(!goes_to(Some(&node(
            "AXTextField",
            "AXSecureTextField",
            "",
            0
        ))));
        assert!(!goes_to(Some(&node("AXButton", "", "Open", 0))));
        assert!(!goes_to(None));
        assert!(
            !goes_to(Some(&node("AXTextField", "AXSearchField", "", 0))),
            "the panel's search field: Command-Shift-G did not take"
        );
        assert_eq!(
            bundle_of("/Applications/aterm.app/Contents/MacOS/aterm"),
            Some("/Applications/aterm.app")
        );
        assert_eq!(bundle_of("/usr/local/bin/aterm"), None);
        assert_eq!(
            bundle_of("/Applications/aterm.app/Contents/MacOS/x/aterm"),
            None
        );
        assert_eq!(bundle_of("/opt/aterm/Contents/MacOS/aterm"), None);
        // The host reads the person's acts by these words (`mac_nux_guest::DEVTOOLS_*`).
        assert_eq!(
            [
                DEVTOOLS_DRAG,
                DEVTOOLS_PLUS,
                DEVTOOLS_GOTO,
                DEVTOOLS_OPEN,
                DEVTOOLS_CANCEL,
                DEVTOOLS_SWITCH
            ],
            [
                "devtools-drag",
                "devtools-plus",
                "devtools-goto",
                "devtools-open",
                "devtools-cancel",
                "devtools-switch"
            ]
        );
    }

    #[test]
    fn a_sheet_over_the_page_is_never_its_list_its_switch_or_its_plus() {
        // The open panel as a sheet of System Settings' window, its file view listing aterm.app
        // between the page's words and its list, with a switch named aterm and an Add of its own.
        let with_sheet = |role: &str, buttons: &[&str]| {
            let mut nodes = devtools_fixture(
                "Developer Tools",
                DEVTOOLS_LINE,
                &[("Terminal", 1)],
                buttons,
            );
            let sheet = nodes.len();
            nodes.push(framed(node(role, "", "", 0), (300.0, 60.0, 520.0, 400.0)));
            nodes.push(framed(
                node("AXScrollArea", "", "", sheet),
                (330.0, 164.0, 460.0, 120.0),
            ));
            nodes.push(framed(
                node("AXOutline", "", "", sheet + 1),
                (330.0, 164.0, 460.0, 120.0),
            ));
            nodes.push(framed(
                node("AXRow", "", "", sheet + 2),
                (330.0, 164.0, 460.0, 20.0),
            ));
            nodes.push(node("AXStaticText", "", "aterm.app", sheet + 3));
            let mut switch = framed(
                node("AXCheckBox", "AXSwitch", "aterm", sheet + 3),
                (750.0, 166.0, 30.0, 16.0),
            );
            switch.value = Some(0);
            nodes.push(switch);
            nodes.push(framed(
                node("AXButton", "", "Add", sheet),
                (332.0, 292.0, 22.0, 22.0),
            ));
            nodes
        };
        let nodes = with_sheet("AXSheet", &[]);
        let pane = devtools_pane(&nodes).expect("the page");
        assert_eq!(
            (pane.list, pane.holds, pane.switch, pane.plus),
            (Some(9), false, None, None),
            "the page's own list, which lacks aterm; the sheet's switch and Add are not its"
        );
        assert_eq!(pane_state(&nodes, &pane), "list=lacks switch=none");
        // Negative control: the same controls in a group of the window are the page's.
        let nodes = with_sheet("AXGroup", &[]);
        let pane = devtools_pane(&nodes).expect("the page");
        assert!(pane.holds && pane.switch.is_some() && pane.plus.is_some());
        assert_ne!(pane.list, Some(9));
    }

    #[test]
    fn the_person_cancels_the_go_to_sheet_before_the_open_panel_it_stands_in() {
        // The open panel with the Go to sheet still up inside it, ahead of the panel's buttons.
        let mut nodes = panel_fixture("AXBrowser", &[]);
        nodes.push(node("AXSheet", "", "", 1));
        nodes.push(node("AXTextField", "", "/Applications/aterm.app", 6));
        nodes.push(node("AXButton", "", "Cancel", 6));
        nodes.push(node("AXButton", "", "Go", 6));
        nodes.push(node("AXButton", "", "Cancel", 2));
        nodes.push(node("AXButton", "", "Open", 2));
        let panel = open_panel(&nodes).expect("the panel");
        assert_eq!(
            (panel.dialog.clone(), panel.open, panel.cancel),
            (1..12, 11, 10),
            "the panel's own Cancel, not the Go to sheet's"
        );
        assert_eq!(inner_cancel(&nodes, &panel), Some(8));
        // Negative control: with the Go to sheet gone, nothing inside the panel to cancel first.
        let bare = panel_fixture("AXBrowser", &[("Cancel", true), ("Open", false)]);
        let panel = open_panel(&bare).expect("the panel");
        assert_eq!(inner_cancel(&bare, &panel), None);
    }

    #[test]
    fn the_person_takes_the_panel_for_gone_only_on_two_misses_or_no_window() {
        let mut misses = 0;
        assert_eq!(
            panel_read(Some(false), true, true, &mut misses),
            PanelRead::Unsure
        );
        assert_eq!(
            panel_read(None, true, true, &mut misses),
            PanelRead::Unsure,
            "one miss"
        );
        assert_eq!(
            panel_read(Some(false), true, true, &mut misses),
            PanelRead::Unsure
        );
        assert_eq!(misses, 0, "a match starts the count again");
        assert_eq!(panel_read(None, true, true, &mut misses), PanelRead::Unsure);
        assert_eq!(
            panel_read(None, false, true, &mut misses),
            PanelRead::Unsure,
            "empty read"
        );
        assert_eq!(panel_read(None, true, true, &mut misses), PanelRead::Gone);
        assert_eq!(
            panel_read(Some(true), true, true, &mut misses),
            PanelRead::Open
        );
        let mut misses = 0;
        assert_eq!(
            panel_read(None, false, false, &mut misses),
            PanelRead::Gone,
            "its process shows no window"
        );
    }

    #[test]
    fn the_person_acts_on_the_developer_tools_page_only_when_nothing_stands_over_it() {
        let s = |secs| Some(Duration::from_secs(secs));
        let lacks = DevSweep {
            list: true,
            ..DevSweep::default()
        };
        assert_eq!(next(&lacks), DevAct::Add);
        // No add while a sheet or the panel service's window stands, or the list did not read.
        assert_eq!(
            next(&DevSweep {
                sheet_up: true,
                ..lacks
            }),
            DevAct::Wait
        );
        assert_eq!(
            next(&DevSweep {
                list: false,
                ..lacks
            }),
            DevAct::Wait
        );
        assert_eq!(
            next(&DevSweep {
                adds: 1,
                last_add: s(5),
                ..lacks
            }),
            DevAct::Wait,
            "too soon after a try"
        );
        // An open panel: waited on, then cancelled once it has stood PANEL_STRAY, again only
        // PANEL_STRAY after that Cancel.
        let panel = |stood, cancelled| DevSweep {
            panel_for: Some(stood),
            cancelled,
            sheet_up: true,
            ..lacks
        };
        assert_eq!(next(&panel(Duration::from_secs(2), None)), DevAct::Wait);
        assert_eq!(next(&panel(PANEL_STRAY, None)), DevAct::CancelPanel);
        assert_eq!(next(&panel(PANEL_STRAY, s(2))), DevAct::Wait);
        assert_eq!(
            next(&panel(PANEL_STRAY, Some(PANEL_STRAY))),
            DevAct::CancelPanel
        );
        // aterm on the list: its switch pressed while it reads off, once per AGAIN; never under a
        // sheet; and no add.
        let holds = DevSweep {
            holds: true,
            switch_off: true,
            ..lacks
        };
        assert_eq!(next(&holds), DevAct::Switch);
        assert_eq!(
            next(&DevSweep {
                switched: s(1),
                ..holds
            }),
            DevAct::Wait
        );
        assert_eq!(
            next(&DevSweep {
                switched: Some(AGAIN),
                ..holds
            }),
            DevAct::Switch
        );
        assert_eq!(
            next(&DevSweep {
                sheet_up: true,
                ..holds
            }),
            DevAct::Wait
        );
        assert_eq!(
            next(&DevSweep {
                switch_off: false,
                ..holds
            }),
            DevAct::Wait,
            "on already, or no switch: and no add, though none was made"
        );
        // How it adds: a drag on the first try that finds the tile, waiting for it on the first
        // try up to TILE_WITHIN, else +.
        let early = Duration::from_secs(1);
        assert_eq!(add_how(0, false, true, early), Some(AddBy::Drag));
        assert_eq!(add_how(0, false, false, early), None, "the tile not up yet");
        assert_eq!(add_how(0, false, false, TILE_WITHIN), Some(AddBy::Plus));
        assert_eq!(
            add_how(1, false, true, TILE_WITHIN),
            Some(AddBy::Drag),
            "a later try that finds the tile"
        );
        assert_eq!(
            add_how(1, true, true, TILE_WITHIN),
            Some(AddBy::Plus),
            "one drag at most"
        );
        assert_eq!(add_how(1, false, false, early), Some(AddBy::Plus));
    }

    #[test]
    fn the_person_answers_a_dialog_that_asks_for_an_administrator_as_the_administrator() {
        let account = ["nux".to_owned(), "NUX Standard".to_owned()];
        let asks =
            "Enter an administrator’s name and password to continue with Privacy & Security.";
        let own = "Enter your password to continue with Privacy & Security.";
        let admin = Some("admin");
        let whose = |words: &str, name: Option<&str>, admin: Option<&str>| {
            let (nodes, secure) = sheet(words, name);
            answer(&nodes, secure, &account, admin)
        };
        let rename = Answer::Administrator { rename: true };
        // The administrator's name goes into an empty name field (as the guest showed it) and over
        // the account's, were the dialog to show it though it asks for an administrator.
        assert_eq!(whose(asks, Some(""), admin), (rename, Some(7)));
        assert_eq!(whose(asks, Some("NUX Standard"), admin), (rename, Some(7)));
        assert_eq!(whose(own, Some(""), admin), (rename, Some(7)));
        assert_eq!(whose(own, Some("someone"), admin), (rename, Some(7)));
        // A name field already showing the administrator keeps it.
        assert_eq!(
            whose(asks, Some("Admin"), admin),
            (Answer::Administrator { rename: false }, Some(7))
        );
        // The account's own password: a prompt for it (no name field, or one showing the account,
        // its words naming no administrator), and every dialog without an administrator at hand.
        assert_eq!(
            whose(own, Some("NUX Standard"), admin),
            (Answer::Account, Some(7))
        );
        assert_eq!(whose(own, Some("nux"), admin), (Answer::Account, Some(7)));
        assert_eq!(
            whose(asks, None, admin),
            (Answer::Account, None),
            "the window's fields outside the sheet are no name field"
        );
        assert_eq!(
            whose(asks, Some("NUX Standard"), None),
            (Answer::Account, Some(7))
        );
        assert_eq!(whose(own, None, None), (Answer::Account, None));
        // A name field whose text did not read shows neither the account nor the administrator:
        // the words alone decide.
        let unread = |words: &str| {
            let (mut nodes, secure) = sheet(words, Some(""));
            nodes[7].value_text = None;
            answer(&nodes, secure, &account, admin)
        };
        assert_eq!(unread(own), (Answer::Account, Some(7)));
        assert_eq!(unread(asks), (rename, Some(7)));
        // A dialog window of its own (SecurityAgent's) is the dialog, whole.
        let window = vec![
            node("AXWindow", "AXDialog", "", usize::MAX),
            node("AXStaticText", "", asks, 0),
            node("AXTextField", "", "NUX Standard", 0),
            node("AXTextField", "AXSecureTextField", "", 0),
            node("AXButton", "", "OK", 0),
        ];
        assert_eq!(answer(&window, 3, &account, admin), (rename, Some(2)));
        assert_eq!(dialog_around(&window, 3), 0..5);
    }

    /// A standard account's person, with the image's administrator at hand.
    fn standard() -> Credentials {
        Credentials {
            password: "nux-lane-2026".to_owned(),
            admin: Some(("admin".to_owned(), "admin".to_owned())),
            account: vec!["nux".to_owned(), "NUX Standard".to_owned()],
        }
    }

    #[test]
    fn a_described_dialog_never_shows_a_secure_fields_text() {
        let who = standard();
        let (nodes, secure) = sheet("Enter an administrator’s name", Some(""));
        assert_eq!(dialog_around(&nodes, secure), 2..11);
        assert_eq!(
            describe(&nodes, dialog_around(&nodes, secure), &who),
            "AXSheet; AXGroup/AXHostingView; AXImage \"Lock\"; AXStaticText \"Privacy & Security is \
             trying to modify your system settings.\"; AXStaticText \"Enter an administrator’s \
             name\"; AXTextField; AXTextField/AXSecureTextField (secure); AXButton \"Modify \
             Settings\"; AXButton \"Cancel\""
        );
        assert_eq!(token(Some("NUX Standard"), &who), "NUX_Standard");
        assert_eq!(token(Some(" "), &who), "(empty)");
        assert_eq!(token(None, &who), "(unread)");
    }

    #[test]
    fn a_password_that_went_into_a_field_not_secure_is_never_recorded() {
        let who = standard();
        // Keys that went astray: the administrator's password after its name, the account's after
        // its full name, the administrator's over a search.
        for astray in [
            "adminadmin",
            "NUX Standardnux-lane-2026",
            "Full Disk Accessadmin",
        ] {
            assert!(who.holds_password(astray), "{astray}");
            assert_eq!(token(Some(astray), &who), "(a password)", "{astray}");
            let (mut nodes, secure) = sheet("Enter an administrator’s name", Some(astray));
            nodes[1].value_text = Some(astray.to_owned());
            nodes[1].text = astray.to_owned();
            let field = describe(&nodes, dialog_around(&nodes, secure), &who);
            assert!(field.contains("AXTextField \"(a password)\""), "{field}");
            assert!(!field.contains(astray), "{field}");
            assert_eq!(
                part(&nodes[1], &who),
                "AXTextField/AXSearchField \"(a password)\""
            );
        }
        // The names the person reads and types are shown, though the administrator's is its
        // password too; the dialog's words are no field, so "administrator" stays.
        for name in ["admin", "NUX Standard", "nux", "Managed via Tart", ""] {
            assert!(!who.holds_password(name), "{name}");
        }
        let (mut nodes, secure) = sheet("Enter an administrator’s name", Some("admin"));
        // A static text's value is its words, as the Accessibility read gives it.
        nodes[6].value_text = Some(nodes[6].text.clone());
        assert_eq!(
            describe(&nodes, dialog_around(&nodes, secure), &who),
            "AXSheet; AXGroup/AXHostingView; AXImage \"Lock\"; AXStaticText \"Privacy & Security is \
             trying to modify your system settings.\"; AXStaticText \"Enter an administrator’s \
             name\"; AXTextField \"admin\"; AXTextField/AXSecureTextField (secure); AXButton \
             \"Modify Settings\"; AXButton \"Cancel\""
        );
        // The admin lane's person: the account is the administrator, `admin`.
        let admin = Credentials {
            password: "admin".to_owned(),
            admin: None,
            account: vec!["admin".to_owned(), "Managed via Tart".to_owned()],
        };
        assert!(!admin.holds_password("admin"));
        assert!(!admin.holds_password("Managed via Tart"));
        assert!(admin.holds_password("Managed via Tartadmin"));
    }

    #[test]
    fn the_person_types_a_password_only_where_the_focus_is_not_a_field_in_clear() {
        let at = |role: &str, subrole: &str, frame: (f64, f64, f64, f64)| ax::Node {
            frame,
            ..node(role, subrole, "", usize::MAX)
        };
        let secure = at(
            "AXTextField",
            "AXSecureTextField",
            (100.0, 200.0, 220.0, 24.0),
        );
        let name = at("AXTextField", "", (100.0, 160.0, 220.0, 24.0));
        let search = at("AXTextField", "AXSearchField", (10.0, 10.0, 180.0, 28.0));
        // The keys go into the secure field, or where the focus did not read, or a control that
        // takes no text: never into the name field or the window's search field.
        assert!(may_type(Some(&secure)));
        assert!(may_type(None));
        assert!(may_type(Some(&at("AXButton", "", (0.0, 0.0, 80.0, 20.0)))));
        assert!(!may_type(Some(&name)));
        assert!(!may_type(Some(&search)));
        assert!(!may_type(Some(&at("AXTextArea", "", (0.0, 0.0, 9.0, 9.0)))));
        // The focus is in the field clicked when it reads as secure as it, at its place.
        assert!(is_at(&secure, &secure));
        assert!(is_at(&name, &name));
        assert!(
            !is_at(&name, &secure),
            "the name field is not the secure field"
        );
        assert!(!is_at(&secure, &name));
        assert!(
            !is_at(&search, &name),
            "the search field is not the name field"
        );
        let unplaced = at("AXTextField", "", (0.0, 0.0, 0.0, 0.0));
        assert!(
            !is_at(&unplaced, &unplaced),
            "a frame that did not read places nothing"
        );
    }

    #[test]
    fn an_administrators_password_replaces_what_an_earlier_try_left_in_the_field() {
        let rename = Answer::Administrator { rename: true };
        let kept = Answer::Administrator { rename: false };
        for answer in [rename, kept] {
            assert_eq!(
                password_keys(answer, "admin"),
                [Key::SelectAll, Key::Text("admin"), Key::Return]
            );
        }
        // The account's own password, typed once per dialog: its keys alone, its dialog not
        // watched nor answered again.
        assert_eq!(
            password_keys(Answer::Account, "nux-lane-2026"),
            [Key::Text("nux-lane-2026"), Key::Return]
        );
        assert!(!Answer::Account.watched());
        assert!(rename.watched() && kept.watched());
    }

    #[test]
    fn the_person_types_an_administrators_name_and_password_only_as_the_administrator() {
        // The image's administrator is `admin`, password `admin`: told apart here, so a name typed
        // as a password, or one person's password for another's, shows.
        let who = Credentials {
            password: "nux-lane-2026".to_owned(),
            admin: Some(("boss".to_owned(), "boss-pw".to_owned())),
            account: vec!["nux".to_owned(), "NUX Standard".to_owned()],
        };
        let rename = Answer::Administrator { rename: true };
        let kept = Answer::Administrator { rename: false };
        assert_eq!(
            credentials(Answer::Account, &who),
            (None, "nux-lane-2026"),
            "the account's own password, its name field left alone"
        );
        assert_eq!(credentials(rename, &who), (Some("boss"), "boss-pw"));
        assert_eq!(
            credentials(kept, &who),
            (None, "boss-pw"),
            "a name field already showing the administrator keeps it"
        );
        // With no administrator at hand, the account's own password whatever the dialog asks.
        let alone = Credentials { admin: None, ..who };
        for answer in [Answer::Account, rename, kept] {
            assert_eq!(
                credentials(answer, &alone),
                (None, "nux-lane-2026"),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn a_password_dialog_went_when_its_windows_left_the_screen_or_its_secure_field_went() {
        let dialog = [41, 42];
        let listed = |ids: &[u64]| ids.iter().copied().collect::<BTreeSet<u64>>();
        let (asking, _) = sheet("Enter an administrator’s name", Some(""));
        let window = vec![
            node(
                "AXWindow",
                "AXStandardWindow",
                "System Settings",
                usize::MAX,
            ),
            node("AXTextField", "AXSearchField", "", 0),
        ];
        // SecurityAgent's dialog, its only window: once it closes no window of it is listed and
        // its controls read nothing.
        assert!(gone(&dialog, Some(&listed(&[7, 99])), &[]));
        assert!(gone(&dialog, Some(&listed(&[])), &[]));
        // A dialog the app shows after it is another, with windows of its own.
        assert!(gone(&dialog, Some(&listed(&[43])), &asking));
        // System Settings' sheet: its window stays listed, its controls read with no secure field.
        assert!(gone(&dialog, Some(&listed(&[41, 42])), &window));
        assert!(gone(&dialog, None, &window));
        // No sign it went: a secure field still read, or nothing read while a window of it is
        // listed (a busy app) or the list did not read.
        assert!(!gone(&dialog, Some(&listed(&[41, 42])), &asking));
        assert!(
            !gone(&dialog, Some(&listed(&[42, 99])), &[]),
            "an empty read while one of its windows is listed"
        );
        assert!(!gone(&dialog, None, &[]), "a window list that did not read");
        assert!(!gone(&dialog, None, &asking));
    }

    #[test]
    fn a_dialogs_going_is_timed_from_its_return_not_from_the_read_that_showed_the_return() {
        // The idle clock can show a Return long after it went out (17.5 s at load 62).
        let ago = |d: Duration| {
            Instant::now()
                .checked_sub(d)
                .expect("the clock ran that long")
        };
        let ms = gone_after(ago(Duration::from_secs(2)), || true);
        assert!(ms.is_some_and(|ms| ms >= 2000), "{ms:?}");
        // The watch ends GONE_WITHIN after the Return, after one look at least: after a Return
        // shown later than that the person looks once, and a dialog that look sees gone went.
        let late = GONE_WITHIN + Duration::from_secs(1);
        let mut looks = 0;
        let stayed = gone_after(ago(late), || {
            looks += 1;
            false
        });
        assert_eq!((stayed, looks), (None, 1));
        let ms = gone_after(ago(late), || true);
        assert!(ms.is_some_and(|ms| ms >= late.as_millis()), "{ms:?}");
    }
}
