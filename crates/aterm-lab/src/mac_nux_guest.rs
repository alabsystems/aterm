// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `aterm-lab-guest` (`src/guest/main.rs`) writes in a `lab mac nux` guest, read here: the
//! dialog census ([`read_census`], [`dialogs`]), the person at the console ([`read_person`],
//! [`hid_proof`], [`click_latencies`], [`sheet_presses`], [`quit_confirms`],
//! [`reopen_presses`]), the setup record's watch
//! ([`read_watch`], [`stage_starts`]) and the app's processes ([`read_procs`], [`runs`]); and the
//! wait on setup ([`Stall`]), which reads `setup=` from the guest's own
//! `aterm ctl status`, the progress record, and the person's presses and the app's processes
//! ([`acts`]), and stops a minute after any of them last moved.
//! Every line the helper writes is `<kind> <epoch ms> <rest>`, the guest's clock.

use std::collections::BTreeMap;

/// One window the census saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub at_ms: u64,
    pub id: u64,
    pub pid: i32,
    pub layer: i64,
    /// `<w>x<h>` in points.
    pub size: String,
    pub owner: String,
}

/// The census record: its windows at the start, those that came after, and when each went.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// The first sweep: its time and how many windows it saw.
    pub started: Option<(u64, usize)>,
    /// Why the window list could not be read (no window server session).
    pub error: Option<String>,
    pub base: Vec<Seen>,
    pub new: Vec<Seen>,
    /// Each window's number and when it went.
    pub gone: BTreeMap<u64, u64>,
}

/// `<kind> <ms> <rest>` of one line.
fn split_line(line: &str) -> Option<(&str, u64, &str)> {
    let mut parts = line.splitn(3, ' ');
    let kind = parts.next()?;
    let at = parts.next()?.parse().ok()?;
    Some((kind, at, parts.next().unwrap_or("").trim_end()))
}

/// `<id> <pid> <layer> <w>x<h> <owner>` at `at_ms`.
fn read_seen(at_ms: u64, rest: &str) -> Option<Seen> {
    let mut parts = rest.splitn(5, ' ');
    Some(Seen {
        at_ms,
        id: parts.next()?.parse().ok()?,
        pid: parts.next()?.parse().ok()?,
        layer: parts.next()?.parse().ok()?,
        size: parts.next()?.to_owned(),
        owner: parts.next()?.to_owned(),
    })
}

/// [`Census`] over the census record's lines.
#[must_use]
pub fn read_census(text: &str) -> Census {
    let mut c = Census::default();
    for (kind, at, rest) in text.lines().filter_map(split_line) {
        match kind {
            "census" => {
                let windows = rest
                    .split_whitespace()
                    .find_map(|kv| kv.strip_prefix("windows="))
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                c.started = Some((at, windows));
            }
            "census-error" => c.error = Some(rest.to_owned()),
            "base" => c.base.extend(read_seen(at, rest)),
            "new" => c.new.extend(read_seen(at, rest)),
            "gone" => {
                if let Ok(id) = rest.trim().parse() {
                    c.gone.entry(id).or_insert(at);
                }
            }
            _ => {}
        }
    }
    c
}

/// The owners whose windows are never a dialog: the app under test and the desktop's own parts.
pub const SHELL_OWNERS: [&str; 11] = [
    "aterm",
    "Window Server",
    "Dock",
    "Control Center",
    "SystemUIServer",
    "Spotlight",
    "TextInputMenuAgent",
    "Finder",
    "WindowManager",
    "Wallpaper",
    "Terminal",
];

/// What kind of dialog an owner's window is.
#[must_use]
pub fn dialog_kind(owner: &str) -> &'static str {
    match owner {
        "nehelper" => "Local Network",
        "CoreServicesUIAgent" => "Gatekeeper",
        "Install Command Line Developer Tools" | "Install Command Line Tools" => {
            "Xcode install stub"
        }
        "SecurityAgent" => "authorization",
        "UserNotificationCenter" => "consent",
        "universalAccessAuthWarn" => "Accessibility",
        "BackgroundTaskManagementAgent" => "Background Items",
        "NotificationCenter" | "Notification Center" => "notification",
        "System Settings" => "System Settings",
        "loginwindow" => "login window",
        _ => "other",
    }
}

/// One dialog owner the census saw after a mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub owner: String,
    pub kind: &'static str,
    /// Its first window, in ms after the mark.
    pub first_ms: u64,
    pub windows: usize,
}

/// The owners of windows that appeared at or after `since_ms` (epoch ms), each with when its first
/// came and how many came, the shell's own ([`SHELL_OWNERS`]) left out, in the order they came.
#[must_use]
pub fn dialogs(census: &Census, since_ms: u64) -> Vec<Dialog> {
    let mut out: Vec<Dialog> = Vec::new();
    for w in census
        .new
        .iter()
        .filter(|w| w.at_ms >= since_ms && !SHELL_OWNERS.contains(&w.owner.as_str()))
    {
        match out.iter_mut().find(|d| d.owner == w.owner) {
            Some(d) => d.windows += 1,
            None => out.push(Dialog {
                owner: w.owner.clone(),
                kind: dialog_kind(&w.owner),
                first_ms: w.at_ms - since_ms,
                windows: 1,
            }),
        }
    }
    out
}

/// One input the person posted, with the idle clock read just before and after it, and the time
/// from just before the post to just after the second read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub at_ms: u64,
    pub what: String,
    pub before_ns: Option<u64>,
    pub after_ns: Option<u64>,
    pub window_ns: Option<u64>,
}

/// The person's record.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// Accessibility and the event-post grant, as the helper found them.
    pub trusted: Option<bool>,
    pub may_post: Option<bool>,
    /// Whether the person was given a password to type (`--password`): one with none reads no
    /// password dialog.
    pub password: Option<bool>,
    pub inputs: Vec<Input>,
    /// Each dialog window it saw first: when, its number, its owner.
    pub seen: Vec<(u64, u64, String)>,
    /// Each press: when, the owner, what it pressed.
    pub clicks: Vec<(u64, String, String)>,
    /// Each password typed.
    pub typed: Vec<Typed>,
    /// What the person read of each password dialog it typed into, for the run's record: the
    /// dialog's controls (`dialog`), the control with the keyboard focus after a step (`focus`),
    /// and each try that typed no password because the focus read a text field not secure
    /// (`skip`); the controls of each Quit & Reopen sheet it answered (`sheet`); and what it read
    /// and did on System Settings' Developer Tools page (`devtools`): when, the kind, the reading.
    pub dialogs: Vec<(u64, String, String)>,
}

/// One password the person typed: when, the owner, whose (`account` or `administrator`), the
/// dialog's name field (`-` when it had none, its text as shown, and `shown>after` when the person
/// put the administrator's name there; each a word, spaces as `_`), and what became of the dialog
/// after its Return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub at_ms: u64,
    pub owner: String,
    pub whose: String,
    pub name: String,
    pub went: Went,
}

/// What the person saw of a password dialog after its Return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Went {
    /// Not watched: the account's own password, typed once per dialog.
    Unwatched,
    /// Still there when the person stopped watching.
    Stayed,
    /// Gone this many ms after the Return.
    After(u64),
}

/// [`Person`] over the person record's lines.
#[must_use]
pub fn read_person(text: &str) -> Person {
    let mut p = Person::default();
    let yes = |rest: &str, key: &str| {
        rest.split_whitespace()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v == "yes")
    };
    let ns = |rest: &str, key: &str| {
        rest.split_whitespace()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .and_then(|v| v.parse().ok())
    };
    for (kind, at, rest) in text.lines().filter_map(split_line) {
        match kind {
            "person" => {
                p.trusted = yes(rest, "trusted");
                p.may_post = yes(rest, "post");
                p.password = yes(rest, "password");
            }
            "input" => p.inputs.push(Input {
                at_ms: at,
                what: rest.split_whitespace().next().unwrap_or("").to_owned(),
                before_ns: ns(rest, "idle_before_ns"),
                after_ns: ns(rest, "idle_after_ns"),
                window_ns: ns(rest, "window_ns"),
            }),
            "seen" => {
                if let Some((id, owner)) = rest.split_once(' ')
                    && let Ok(id) = id.parse()
                {
                    p.seen.push((at, id, owner.to_owned()));
                }
            }
            "click" => {
                let mut parts = rest.splitn(2, ' ');
                let _pid = parts.next();
                if let Some((owner, what)) = parts.next().and_then(|r| r.rsplit_once(' ')) {
                    p.clicks.push((at, owner.to_owned(), what.to_owned()));
                }
            }
            "typed" => {
                let parts: Vec<&str> = rest.splitn(6, ' ').collect();
                if let [_pid, whose, _chars, name, went, owner] = parts[..] {
                    p.typed.push(Typed {
                        at_ms: at,
                        owner: owner.to_owned(),
                        whose: whose.to_owned(),
                        name: name.to_owned(),
                        went: match went.strip_prefix("went:").and_then(|ms| ms.parse().ok()) {
                            Some(ms) => Went::After(ms),
                            None if went == "unwatched" => Went::Unwatched,
                            None => Went::Stayed,
                        },
                    });
                }
            }
            "dialog" | "focus" | "skip" | "sheet" | "devtools" => {
                if let Some((_pid, rest)) = rest.split_once(' ') {
                    p.dialogs.push((at, kind.to_owned(), rest.to_owned()));
                }
            }
            _ => {}
        }
    }
    p
}

/// Whether the person's inputs reset `HIDIdleTime`, as each input's readings show.
#[derive(Debug, PartialEq, Eq)]
pub enum HidProof {
    /// Every input reset it: how many, and the largest reading just after one.
    Reset { inputs: usize, worst_after_ns: u64 },
    /// Not shown, and why: never a pass.
    NotMeasured(String),
}

/// [`HidProof`] over a person record: proven only when every input's reading after it is within
/// its window, the time since just before the post: an input the HID system took restarted the
/// clock inside that window, and posted input that never reaches the HID system (no event-post
/// grant, no console session) leaves it counting from before the window opened.
#[must_use]
pub fn hid_proof(p: &Person) -> HidProof {
    if p.inputs.is_empty() {
        return HidProof::NotMeasured("the person posted no input".into());
    }
    let mut worst = 0;
    let mut stuck = 0;
    for input in &p.inputs {
        let (Some(after), Some(window)) = (input.after_ns, input.window_ns) else {
            return HidProof::NotMeasured("the HID idle clock was unreadable".into());
        };
        if after > window {
            stuck += 1;
        }
        worst = worst.max(after);
    }
    if stuck > 0 {
        return HidProof::NotMeasured(format!(
            "{stuck} of {} posted inputs left HIDIdleTime running: they did not reach the HID system",
            p.inputs.len()
        ));
    }
    HidProof::Reset {
        inputs: p.inputs.len(),
        worst_after_ns: worst,
    }
}

/// Each press with its latency: from the owner's last dialog window first seen at or before it, to
/// the press.
#[must_use]
pub fn click_latencies(p: &Person) -> Vec<(String, String, u64)> {
    p.clicks
        .iter()
        .filter_map(|(at, owner, what)| {
            let seen = p
                .seen
                .iter()
                .filter(|(s, _, o)| o == owner && s <= at)
                .map(|(s, _, _)| *s)
                .max()?;
            Some((owner.clone(), what.clone(), at - seen))
        })
        .collect()
}

/// The words of the person's presses on System Settings' Quit & Reopen sheet, as its `click`
/// lines carry them (the button's title, spaces as `_`): Quit & Reopen, and Later.
pub const SHEET_PRESSES: [&str; 2] = ["Quit_&_Reopen", "Later"];

/// Each press the person gave the Quit & Reopen sheet: when (epoch ms) and the button's title.
#[must_use]
pub fn sheet_presses(p: &Person) -> Vec<(u64, String)> {
    p.clicks
        .iter()
        .filter(|(_, _, what)| SHEET_PRESSES.contains(&what.as_str()))
        .map(|(at, _, what)| (*at, what.replace('_', " ")))
        .collect()
}

/// The word of the person's press on Quit in the app's own question before it quits, as its `click`
/// line carries it: the helper presses it only after its own Quit & Reopen press.
pub const QUIT_CONFIRM: &str = "quit-confirm";

/// Each press the person gave the app's question before it quits ([`QUIT_CONFIRM`]): when (epoch
/// ms).
#[must_use]
pub fn quit_confirms(p: &Person) -> Vec<u64> {
    p.clicks
        .iter()
        .filter(|(_, _, what)| what == QUIT_CONFIRM)
        .map(|(at, _, _)| *at)
        .collect()
}

/// The person's answers across System Settings' Quit & Reopen: its presses on the sheet
/// ([`sheet_presses`]) and on Quit in the app's question that follows ([`quit_confirms`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReopenPresses {
    pub sheet: Vec<(u64, String)>,
    pub quit: Vec<u64>,
}

/// [`ReopenPresses`] over a person record.
#[must_use]
pub fn reopen_presses(p: &Person) -> ReopenPresses {
    ReopenPresses {
        sheet: sheet_presses(p),
        quit: quit_confirms(p),
    }
}

/// The words of the person's acts on System Settings' Developer Tools page, as its `click` lines
/// carry them (the guest helper's `DEVTOOLS_*`): the helper panel's tile dragged onto the list, +
/// pressed below it, the app's path typed into the open panel's Go to field and its Return, the
/// panel's Open, its Cancel, and aterm's switch on the list.
pub const DEVTOOLS_DRAG: &str = "devtools-drag";
pub const DEVTOOLS_PLUS: &str = "devtools-plus";
pub const DEVTOOLS_GOTO: &str = "devtools-goto";
pub const DEVTOOLS_OPEN: &str = "devtools-open";
pub const DEVTOOLS_CANCEL: &str = "devtools-cancel";
pub const DEVTOOLS_SWITCH: &str = "devtools-switch";

/// How the person added aterm to the Developer Tools list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Add {
    /// The helper panel's tile dragged onto the list.
    Drag,
    /// + below the list, and the app chosen in the open panel.
    Plus,
}

impl Add {
    /// The words a cell says it with.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Add::Drag => "drag",
            Add::Plus => "+",
        }
    }
}

/// What the person read and did on System Settings' Developer Tools page, each when (epoch ms).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DevtoolsActs {
    /// The person's Accessibility grant ([`Person::trusted`]): without it, it reads nothing.
    pub trusted: Option<bool>,
    /// Whether the person had a password to type ([`Person::password`]).
    pub password: Option<bool>,
    /// The page's controls around its words, as the person described them when the last state
    /// it said of the page was a list that did not read: `None` when the last state read one.
    pub unread: Option<String>,
    /// Each time the person read a System Settings password dialog it was to type into.
    pub asked: Vec<u64>,
    /// Each time it typed nothing into one, the focus reading a text field not secure.
    pub skipped: Vec<u64>,
    /// Each state of the page as the person said it when it changed (`list=… switch=…`).
    pub panes: Vec<(u64, String)>,
    /// Each add the person began: its drag, or its press of +.
    pub adds: Vec<(u64, Add)>,
    /// Each step of an add with + after the press: the path's Return, Open, Cancel.
    pub steps: Vec<(u64, String)>,
    /// Each press of aterm's switch on the list.
    pub switches: Vec<u64>,
    /// Each password the person typed into System Settings.
    pub passwords: Vec<u64>,
    /// The rest the person said of the page: where a drag went, the open panel, and why a try
    /// did not go on.
    pub notes: Vec<(u64, String)>,
}

/// [`DevtoolsActs`] over a person record.
#[must_use]
pub fn devtools_acts(p: &Person) -> DevtoolsActs {
    let mut acts = DevtoolsActs::default();
    for (at, kind, reading) in &p.dialogs {
        if kind != "devtools" {
            continue;
        }
        match reading.strip_prefix("pane ") {
            Some(said) => {
                let (state, described) = said.split_once(':').unwrap_or((said, ""));
                let state = state.trim().to_owned();
                acts.unread = state
                    .starts_with("list=unread")
                    .then(|| described.trim().to_owned());
                acts.panes.push((*at, state));
            }
            None => acts.notes.push((*at, reading.clone())),
        }
    }
    acts.trusted = p.trusted;
    acts.password = p.password;
    for (at, kind, reading) in &p.dialogs {
        if !reading.starts_with("System Settings:") {
            continue;
        }
        match kind.as_str() {
            "dialog" => acts.asked.push(*at),
            "skip" => acts.skipped.push(*at),
            _ => {}
        }
    }
    for (at, _, what) in &p.clicks {
        match what.as_str() {
            DEVTOOLS_DRAG => acts.adds.push((*at, Add::Drag)),
            DEVTOOLS_PLUS => acts.adds.push((*at, Add::Plus)),
            DEVTOOLS_SWITCH => acts.switches.push(*at),
            DEVTOOLS_GOTO | DEVTOOLS_OPEN | DEVTOOLS_CANCEL => {
                acts.steps.push((*at, what.clone()));
            }
            _ => {}
        }
    }
    acts.passwords = p
        .typed
        .iter()
        .filter(|t| t.owner == "System Settings")
        .map(|t| t.at_ms)
        .collect();
    acts
}

/// The process watch's record (`procs`): when it started, why the process table did not read,
/// and each app process's coming and going.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Procs {
    pub started: Option<u64>,
    pub error: Option<String>,
    /// `(seen at, pid, Some(the kernel's start, epoch ms))` for each `up`, `(seen at, pid, None)`
    /// for each `down`, in order.
    pub events: Vec<(u64, u32, Option<u64>)>,
}

/// [`Procs`] over the process watch's lines.
#[must_use]
pub fn read_procs(text: &str) -> Procs {
    let mut p = Procs::default();
    for (kind, at, rest) in text.lines().filter_map(split_line) {
        let mut words = rest.split_whitespace();
        match kind {
            "procs" => p.started = Some(at),
            "procs-error" => p.error = Some(rest.to_owned()),
            "up" => {
                let pid = words.next().and_then(|w| w.parse().ok());
                let start = words
                    .find_map(|kv| kv.strip_prefix("start="))
                    .and_then(|v| v.parse().ok());
                if let (Some(pid), Some(start)) = (pid, start) {
                    p.events.push((at, pid, Some(start)));
                }
            }
            "down" => {
                if let Some(pid) = words.next().and_then(|w| w.parse().ok()) {
                    p.events.push((at, pid, None));
                }
            }
            _ => {}
        }
    }
    p
}

/// One app process the watch saw: its pid, the kernel's start (epoch ms), and when the watch saw
/// it gone (`None`: it ran to the record's end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub pid: u32,
    pub start_ms: u64,
    pub down_ms: Option<u64>,
}

/// Each app process of the watch's record, in the order they started.
#[must_use]
pub fn runs(p: &Procs) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for &(at, pid, start) in &p.events {
        match start {
            Some(start_ms) => out.push(Run {
                pid,
                start_ms,
                down_ms: None,
            }),
            None => {
                if let Some(run) = out
                    .iter_mut()
                    .rev()
                    .find(|r| r.pid == pid && r.down_ms.is_none())
                {
                    run.down_ms = Some(at);
                }
            }
        }
    }
    out.sort_by_key(|r| r.start_ms);
    out
}

/// One look of the watch: the file's text on one line, `None` when it was absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    pub at_ms: u64,
    pub text: Option<String>,
}

/// A watch's looks and the kills it sent, each kill `(when in epoch ms, the pid, the stage bit)`.
pub type Watch = (Vec<Look>, Vec<(u64, i32, u64)>);

/// The watch record: each change of the file, and each kill it sent (when, the pid, the stage).
#[must_use]
pub fn read_watch(text: &str) -> Watch {
    let mut looks = Vec::new();
    let mut kills = Vec::new();
    for (kind, at, rest) in text.lines().filter_map(split_line) {
        match kind {
            "at" => looks.push(Look {
                at_ms: at,
                text: (rest != "absent").then(|| rest.to_owned()),
            }),
            "killed" => {
                let mut parts = rest.split_whitespace();
                let pid = parts.next().and_then(|p| p.parse().ok());
                let bit = parts
                    .find_map(|kv| kv.strip_prefix("running="))
                    .and_then(|b| b.parse().ok());
                if let (Some(pid), Some(bit)) = (pid, bit) {
                    kills.push((at, pid, bit));
                }
            }
            _ => {}
        }
    }
    (looks, kills)
}

/// What a progress record (`initial-setup.json`) says: its state, done bits and running stage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub state: String,
    pub done: u64,
    pub running: Option<u64>,
}

/// [`Record`] of a progress record's text; `None` when it is no such record.
#[must_use]
pub fn read_record(text: &str) -> Option<Record> {
    let v: aterm_json::Value = aterm_json::from_str(text.trim()).ok()?;
    Some(Record {
        state: v.get("state")?.as_str()?.to_owned(),
        done: v
            .get("done")
            .and_then(aterm_json::Value::as_u64)
            .unwrap_or(0),
        running: v.get("running").and_then(aterm_json::Value::as_u64),
    })
}

/// A stage's name by the bit the progress record stores for it (`initial.rs` `Stage`).
#[must_use]
pub fn stage_name(bit: u64) -> &'static str {
    match bit {
        1 => "Workstation",
        2 => "Spotlight",
        4 => "Permissions",
        8 => "Secrets",
        16 => "Repositories",
        32 => "Accounts",
        _ => "?",
    }
}

/// Each stage start the watch saw: each look whose record names a running stage other than the
/// look before's, with its time.
#[must_use]
pub fn stage_starts(looks: &[Look]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut before = None;
    for look in looks {
        let running = look
            .text
            .as_deref()
            .and_then(read_record)
            .and_then(|r| r.running);
        if let Some(bit) = running
            && running != before
        {
            out.push((look.at_ms, bit));
        }
        before = running;
    }
    out
}

/// When the watch first saw the record completed.
#[must_use]
pub fn completed_at(looks: &[Look]) -> Option<u64> {
    looks
        .iter()
        .find(|l| {
            l.text
                .as_deref()
                .and_then(read_record)
                .is_some_and(|r| r.state == "completed")
        })
        .map(|l| l.at_ms)
}

/// The value of `setup=` in `aterm ctl status`' line; `None` from a build without the field.
#[must_use]
pub fn setup_field(status: &str) -> Option<&str> {
    status
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("setup="))
}

/// The sid of the first session in `aterm ctl ls`' lines (`<pid> <local> <sid> …`); `None` when
/// no line is one.
#[must_use]
pub fn first_session(ls: &str) -> Option<&str> {
    ls.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.parse::<u32>().ok();
        let local = fields.next()?.parse::<u32>().ok();
        pid.and(local).and(fields.next())
    })
}

/// Where setup stands in one look at the guest: `setup=` (8b NUX-5) and the progress record, and
/// how many distinct things the person did and the app's processes did so far ([`acts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    pub setup: Option<String>,
    pub record: Option<Record>,
    pub acts: usize,
}

/// How many things the guest's records hold that move a first launch on while `setup=` and the
/// progress record stand still: the person's presses (`click`) and typings (`typed`), and the
/// app's processes starting (`up`) and going (`down`), each read without its time. A press the
/// person repeats on the same control with nothing between (an Allow that stays, pressed again
/// every three seconds) counts once, so it never keeps a stuck row waiting; the same press after
/// another act (a second dialog's Allow) counts again. The nudges and other lines count not at
/// all. `text`: the person's record, then the process watch's, each in its own order. Pure.
#[must_use]
pub fn acts(text: &str) -> usize {
    let mut last: Option<String> = None;
    text.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let kind = words.next()?;
            let _at = words.next()?;
            matches!(kind, "click" | "typed" | "up" | "down")
                .then(|| format!("{kind} {}", words.collect::<Vec<_>>().join(" ")))
        })
        .filter(|act| {
            let again = last.as_ref() == Some(act);
            last = Some(act.clone());
            !again
        })
        .count()
}

impl Reading {
    /// Done: `setup=done`, or, from a build without the field, the record completed.
    #[must_use]
    pub fn done(&self) -> bool {
        match &self.setup {
            Some(field) => field == "done",
            None => self.record.as_ref().is_some_and(|r| r.state == "completed"),
        }
    }

    /// What it says, on one line: what [`Stall`] compares.
    #[must_use]
    pub fn said(&self) -> String {
        let record = self.record.as_ref().map_or_else(
            || "no record".to_owned(),
            |r| {
                format!(
                    "{} done={} running={}",
                    r.state,
                    r.done,
                    r.running
                        .map_or_else(|| "none".to_owned(), |b| stage_name(b).to_owned())
                )
            },
        );
        format!(
            "setup={} ({record})",
            self.setup.as_deref().unwrap_or("absent")
        )
    }
}

/// How long setup may stand still before the wait stops: the lane's one-minute hang detector.
pub const STALL_MS: u64 = 60_000;

/// What one look told the wait.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited {
    Done,
    /// Not done; it moved within the last [`STALL_MS`] (or this is the first look).
    Moving,
    /// It said the same for [`STALL_MS`]: what it said, and since when (ms on the wait's clock).
    Stalled {
        stood: String,
        since_ms: u64,
    },
}

/// The wait on setup: done, or stopped once setup stood still for its limit, with nothing new
/// from the person or in the app's processes ([`Reading::acts`]) in that time either. Its clock
/// is the caller's (ms), so a test gives it one.
#[derive(Debug)]
pub struct Stall {
    limit_ms: u64,
    last: Option<(String, usize, u64)>,
}

impl Stall {
    #[must_use]
    pub const fn new(limit_ms: u64) -> Stall {
        Stall {
            limit_ms,
            last: None,
        }
    }

    /// Take one look, made at `now_ms`.
    pub fn see(&mut self, now_ms: u64, reading: &Reading) -> Waited {
        if reading.done() {
            return Waited::Done;
        }
        let said = reading.said();
        match &self.last {
            Some((last, acts, since)) if *last == said && *acts == reading.acts => {
                if now_ms.saturating_sub(*since) >= self.limit_ms {
                    Waited::Stalled {
                        stood: said,
                        since_ms: *since,
                    }
                } else {
                    Waited::Moving
                }
            }
            _ => {
                self.last = Some((said, reading.acts, now_ms));
                Waited::Moving
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_census_reads_its_start_each_window_and_when_each_went() {
        let text = "census 1000 every=250 windows=2\n\
                    base 1000 10 300 25 1512x33 Window Server\n\
                    base 1000 11 301 0 800x600 Finder\n\
                    new 4000 40 800 0 900x644 aterm\n\
                    new 5250 41 120 0 420x210 nehelper\n\
                    new 5500 42 121 0 260x300 SecurityAgent\n\
                    gone 7000 41\n\
                    new 9000 43 120 0 420x210 nehelper\n\
                    new 9100 44 77 0 300x80 Install Command Line Developer Tools\n\
                    torn line\n";
        let c = read_census(text);
        assert_eq!(c.started, Some((1000, 2)));
        assert_eq!(c.base.len(), 2);
        assert_eq!(c.new.len(), 5);
        assert_eq!(c.new[1].owner, "nehelper");
        assert_eq!(c.new[4].owner, "Install Command Line Developer Tools");
        assert_eq!(c.gone.get(&41), Some(&7000));
        let d = dialogs(&c, 4000);
        assert_eq!(
            d.iter()
                .map(|d| (d.owner.as_str(), d.kind, d.first_ms, d.windows))
                .collect::<Vec<_>>(),
            [
                ("nehelper", "Local Network", 1250, 2),
                ("SecurityAgent", "authorization", 1500, 1),
                (
                    "Install Command Line Developer Tools",
                    "Xcode install stub",
                    5100,
                    1
                ),
            ],
            "the app's own window and the baseline are never dialogs"
        );
        assert!(dialogs(&c, 9200).is_empty(), "nothing came after the mark");
        let failed = read_census("census-error 5 the window list is unreadable\n");
        assert_eq!(
            failed.error.as_deref(),
            Some("the window list is unreadable")
        );
        assert_eq!(failed.started, None);
    }

    #[test]
    fn every_kind_of_dialog_the_target_counts_is_named() {
        for (owner, kind) in [
            ("nehelper", "Local Network"),
            ("CoreServicesUIAgent", "Gatekeeper"),
            ("SecurityAgent", "authorization"),
            ("UserNotificationCenter", "consent"),
            ("BackgroundTaskManagementAgent", "Background Items"),
            ("NotificationCenter", "notification"),
            ("universalAccessAuthWarn", "Accessibility"),
            ("Something New", "other"),
        ] {
            assert_eq!(dialog_kind(owner), kind, "{owner}");
        }
    }

    #[test]
    fn an_input_proves_presence_only_when_it_reset_the_idle_clock() {
        let text = "person 100 trusted=yes post=yes\n\
                    input 200 nudge idle_before_ns=4800000000 idle_after_ns=21000000 window_ns=21500000\n\
                    seen 300 77 nehelper\n\
                    input 900 click idle_before_ns=700000000 idle_after_ns=19000000 window_ns=20900000\n\
                    click 950 120 nehelper Allow\n\
                    gone 1300 77\n";
        let p = read_person(text);
        assert_eq!((p.trusted, p.may_post), (Some(true), Some(true)));
        assert_eq!(
            hid_proof(&p),
            HidProof::Reset {
                inputs: 2,
                worst_after_ns: 21_000_000
            }
        );
        assert_eq!(
            click_latencies(&p),
            [("nehelper".to_owned(), "Allow".to_owned(), 650)]
        );
        // Posted events that never reached the HID system leave the clock running: never a pass.
        let stuck = read_person(
            "input 200 nudge idle_before_ns=4800000000 idle_after_ns=4820000000 window_ns=20400000\n\
             input 5200 nudge idle_before_ns=9800000000 idle_after_ns=9820000000 window_ns=20300000\n",
        );
        assert!(
            matches!(hid_proof(&stuck), HidProof::NotMeasured(why) if why.starts_with("2 of 2"))
        );
        // A key straight after a click, with the clock barely restarted before it, and a read a
        // loaded guest took 300 ms to make, each restarted the clock inside its window.
        let close = read_person(
            "input 900 click idle_before_ns=700000000 idle_after_ns=20000000 window_ns=20800000\n\
             input 960 key idle_before_ns=19000000 idle_after_ns=24000000 window_ns=61000000\n\
             input 6000 nudge idle_before_ns=5000000000 idle_after_ns=310000000 window_ns=320000000\n",
        );
        assert_eq!(
            hid_proof(&close),
            HidProof::Reset {
                inputs: 3,
                worst_after_ns: 310_000_000
            }
        );
        // A key the HID system never took leaves the clock counting from the click before it,
        // past the key's window.
        let missed = read_person(
            "input 900 click idle_before_ns=700000000 idle_after_ns=20000000 window_ns=20800000\n\
             input 960 key idle_before_ns=19000000 idle_after_ns=62000000 window_ns=40000000\n",
        );
        assert!(
            matches!(hid_proof(&missed), HidProof::NotMeasured(why) if why.starts_with("1 of 2"))
        );
        assert!(matches!(
            hid_proof(&read_person("input 1 nudge idle_before_ns=? idle_after_ns=? window_ns=20000000\n")),
            HidProof::NotMeasured(why) if why.contains("unreadable")
        ));
        assert!(matches!(
            hid_proof(&Person::default()),
            HidProof::NotMeasured(_)
        ));
    }

    #[test]
    fn the_watch_names_each_stage_start_the_completion_and_a_kill() {
        let rec = |state: &str, done: u64, running: Option<u64>| {
            let running = running.map_or("null".to_owned(), |b| b.to_string());
            format!(
                r#"{{"profile":"macos-workstation-v1","state":"{state}","done":{done},"running":{running},"revision":1}}"#
            )
        };
        let text = format!(
            "at 1000 absent\nat 2000 {}\nat 2500 {}\nat 2600 {}\nat 4000 {}\nkilled 4001 812 running=4 sent=yes\nat 9000 {}\nat 12000 {}\n",
            rec("pending", 0, None),
            rec("pending", 0, Some(1)),
            rec("pending", 0, Some(1)),
            rec("pending", 1, Some(4)),
            rec("pending", 1, Some(4)),
            rec("completed", 7, None),
        );
        let (looks, kills) = read_watch(&text);
        assert_eq!(looks.first().map(|l| l.text.clone()), Some(None));
        assert_eq!(kills, [(4001, 812, 4)]);
        assert_eq!(
            stage_starts(&looks),
            [(2500, 1), (4000, 4)],
            "a stage still running across looks started once"
        );
        assert_eq!(completed_at(&looks), Some(12000));
        assert_eq!(stage_name(4), "Permissions");
        let (resumed, _) = read_watch(&format!(
            "at 1 {}\nat 2 {}\nat 3 {}\n",
            rec("pending", 0, Some(4)),
            rec("pending", 0, None),
            rec("pending", 0, Some(4))
        ));
        assert_eq!(
            stage_starts(&resumed),
            [(1, 4), (3, 4)],
            "a resumed stage starts again"
        );
    }

    #[test]
    fn the_process_watch_names_each_app_process_its_start_and_its_going() {
        let p = read_procs(
            "procs 1000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm\n\
             up 1050 793 start=1020\n\
             down 92400 793\n\
             up 92600 1201 start=92550\n\
             torn line\n\
             up 92700 x start=1\n",
        );
        assert_eq!((p.started, p.error.as_deref()), (Some(1000), None));
        assert_eq!(
            runs(&p),
            [
                Run {
                    pid: 793,
                    start_ms: 1020,
                    down_ms: Some(92400)
                },
                Run {
                    pid: 1201,
                    start_ms: 92550,
                    down_ms: None
                }
            ]
        );
        // A pid used again: its going closes only the process still up; a going of a pid never
        // seen up is no process.
        let reused = read_procs(
            "procs 1 every=50 exe=x\nup 2 7 start=2\ndown 3 7\nup 4 7 start=4\ndown 5 9\n",
        );
        assert_eq!(
            runs(&reused),
            [
                Run {
                    pid: 7,
                    start_ms: 2,
                    down_ms: Some(3)
                },
                Run {
                    pid: 7,
                    start_ms: 4,
                    down_ms: None
                }
            ]
        );
        let failed = read_procs(
            "procs 1 every=50 exe=x\nprocs-error 2 the kernel's process table did not read\n",
        );
        assert_eq!(
            failed.error.as_deref(),
            Some("the kernel's process table did not read")
        );
        assert!(runs(&failed).is_empty());
    }

    #[test]
    fn the_sheet_presses_are_the_persons_quit_and_reopen_or_later_and_nothing_else() {
        let p = read_person(
            "seen 100 77 System Settings\n\
             click 200 120 System Settings switch:aterm\n\
             sheet 290 120 System Settings: AXSheet; AXButton \"Later\"; AXButton \"Quit & Reopen\"\n\
             click 300 120 System Settings Quit_&_Reopen\n\
             click 400 121 UserNotificationCenter Allow\n\
             click 500 120 System Settings Later\n",
        );
        assert_eq!(
            sheet_presses(&p),
            [(300, "Quit & Reopen".to_owned()), (500, "Later".to_owned())]
        );
        assert_eq!(
            p.dialogs,
            [(
                290,
                "sheet".to_owned(),
                "System Settings: AXSheet; AXButton \"Later\"; AXButton \"Quit & Reopen\""
                    .to_owned()
            )],
            "the sheet's controls are kept for the run's record"
        );
        // Negative control: a switch, an Allow and a Quit that is not the sheet's are no press on it.
        let none = read_person(
            "click 200 120 System Settings switch:aterm\n\
             click 400 121 UserNotificationCenter Allow\n\
             click 450 121 CoreServicesUIAgent Quit\n",
        );
        assert!(sheet_presses(&none).is_empty());
    }

    #[test]
    fn the_quit_confirms_are_the_persons_quit_on_the_apps_question_and_nothing_else() {
        let p = read_person(
            "click 300 120 System Settings Quit_&_Reopen\n\
             sheet 5900 793 aterm (dev): AXWindow/AXDialog; AXStaticText \"Are you sure you want to quit aterm?\"\n\
             click 6000 793 aterm (dev) quit-confirm\n",
        );
        assert_eq!(quit_confirms(&p), [6000]);
        assert_eq!(
            p.clicks[1],
            (6000, "aterm (dev)".to_owned(), QUIT_CONFIRM.to_owned()),
            "an owner with spaces keeps them"
        );
        assert_eq!(
            reopen_presses(&p),
            ReopenPresses {
                sheet: vec![(300, "Quit & Reopen".to_owned())],
                quit: vec![6000],
            },
            "the question's press is no press on the sheet"
        );
        assert_eq!(
            p.dialogs[0].1, "sheet",
            "its controls are kept for the record"
        );
        // Negative control: the sheet's own presses, an Allow and a Quit button elsewhere.
        let none = read_person(
            "click 300 120 System Settings Quit_&_Reopen\n\
             click 400 120 System Settings Later\n\
             click 450 121 CoreServicesUIAgent Quit\n\
             click 500 121 UserNotificationCenter Allow\n",
        );
        assert!(quit_confirms(&none).is_empty());
    }

    #[test]
    fn the_developer_tools_acts_are_the_persons_on_that_page_and_nothing_else() {
        let p = read_person(
            "click 200 120 System Settings switch:aterm\n\
             devtools 1000 120 pane list=lacks switch=none\n\
             devtools 1100 120 drag tile=793 from=900,300 to=500,400\n\
             click 7000 120 System Settings devtools-drag\n\
             typed 9000 120 account 5 Managed_via_Tart unwatched System Settings\n\
             devtools 30000 120 open-panel 120 System Settings: AXSheet; AXButton \"Open\"\n\
             click 31000 120 System Settings devtools-plus\n\
             click 52000 120 System Settings devtools-goto\n\
             click 58000 120 System Settings devtools-open\n\
             devtools 60000 120 pane list=holds switch=off\n\
             click 66000 120 System Settings devtools-switch\n\
             devtools 66500 120 pane list=holds switch=on\n\
             typed 70000 1173 account 5 - unwatched SecurityAgent\n",
        );
        let acts = devtools_acts(&p);
        assert_eq!(
            acts.panes,
            [
                (1000, "list=lacks switch=none".to_owned()),
                (60000, "list=holds switch=off".to_owned()),
                (66500, "list=holds switch=on".to_owned()),
            ]
        );
        assert_eq!(acts.adds, [(7000, Add::Drag), (31000, Add::Plus)]);
        assert_eq!(
            acts.steps,
            [
                (52000, DEVTOOLS_GOTO.to_owned()),
                (58000, DEVTOOLS_OPEN.to_owned())
            ]
        );
        assert_eq!(acts.switches, [66000]);
        assert_eq!(acts.passwords, [9000], "System Settings' password alone");
        assert_eq!(acts.notes.len(), 2, "{:?}", acts.notes);
        assert!(acts.notes[0].1.starts_with("drag tile=793"));
        // A page the list did not read keeps its state, not its description.
        let unread = devtools_acts(&read_person(
            "devtools 1000 120 pane list=unread switch=none: AXGroup; AXStaticText \"Developer Tools\"\n",
        ));
        assert_eq!(unread.panes, [(1000, "list=unread switch=none".to_owned())]);
        // Negative control: another page's switch, the sheet's presses and an Allow are none of it.
        let none = devtools_acts(&read_person(
            "click 200 120 System Settings switch:aterm\n\
             click 300 120 System Settings Quit_&_Reopen\n\
             click 400 121 UserNotificationCenter Allow\n\
             sheet 290 120 System Settings: AXSheet\n",
        ));
        assert_eq!(
            none,
            DevtoolsActs::default(),
            "nothing of the Developer Tools page"
        );
    }

    #[test]
    fn the_status_read_names_the_first_session_listed() {
        // A first-launch guest's tabs, listed while System State fronts the window.
        let ls = "763 0 s-e19818197180bf468468 - alive ~ meta=0 window=0 active=0 wfocus=1\n\
                  763 1 s-0a1b2c3d4e5f60718293 - alive ~ meta=0 window=0 active=- wfocus=1 *\n";
        assert_eq!(first_session(ls), Some("s-e19818197180bf468468"));
        assert_eq!(first_session("aterm-ctl: ERR no instance answered\n"), None);
        assert_eq!(first_session(""), None);
    }

    #[test]
    fn setup_reads_its_status_field_and_falls_back_to_the_record() {
        let line = "OK sid=1 fleet=off setup=pending:fda,devtools fabric=absent";
        assert_eq!(setup_field(line), Some("pending:fda,devtools"));
        assert_eq!(setup_field("OK sid=1 fleet=off"), None);
        let completed = Some(Record {
            state: "completed".into(),
            done: 7,
            running: None,
        });
        assert!(
            Reading {
                setup: None,
                record: completed.clone(),
                acts: 0,
            }
            .done(),
            "a build before setup= is read by its record"
        );
        assert!(
            !Reading {
                setup: Some("pending:fda".into()),
                record: completed,
                acts: 0,
            }
            .done(),
            "setup= wins once it is there"
        );
        assert_eq!(
            read_record(r#"{"state":"pending","done":3,"running":8}"#),
            Some(Record {
                state: "pending".into(),
                done: 3,
                running: Some(8)
            })
        );
        assert_eq!(read_record("not json"), None);
    }

    #[test]
    fn the_wait_stops_a_minute_after_setup_last_moved_and_never_before() {
        let at = |setup: &str| Reading {
            setup: Some(setup.to_owned()),
            record: None,
            acts: 0,
        };
        let mut wait = Stall::new(STALL_MS);
        assert_eq!(wait.see(0, &at("pending")), Waited::Moving);
        assert_eq!(wait.see(59_999, &at("pending")), Waited::Moving);
        // It moved: the minute starts again from the move.
        assert_eq!(wait.see(60_000, &at("pending:fda")), Waited::Moving);
        assert_eq!(wait.see(119_999, &at("pending:fda")), Waited::Moving);
        assert_eq!(
            wait.see(120_000, &at("pending:fda")),
            Waited::Stalled {
                stood: "setup=pending:fda (no record)".into(),
                since_ms: 60_000
            }
        );
        let mut done = Stall::new(STALL_MS);
        assert_eq!(done.see(5, &at("done")), Waited::Done);
        // A stage moving under an unchanged setup= is progress too.
        let mut stages = Stall::new(STALL_MS);
        let running = |bit| Reading {
            setup: Some("pending".into()),
            record: Some(Record {
                state: "pending".into(),
                done: 0,
                running: Some(bit),
            }),
            acts: 0,
        };
        assert_eq!(stages.see(0, &running(1)), Waited::Moving);
        assert_eq!(stages.see(50_000, &running(2)), Waited::Moving);
        assert_eq!(stages.see(100_000, &running(2)), Waited::Moving);
        assert!(matches!(
            stages.see(110_000, &running(2)),
            Waited::Stalled { .. }
        ));
    }

    /// The person's and the processes' records as the guest helper writes them, from a run
    /// shaped like 2026-10-28's 20:00 first launch: setup= stood still from 154.6 s while the
    /// person turned Full Disk Access on, typed the password, answered the Quit & Reopen sheet
    /// and aterm's question, and aterm quit and opened again.
    const RECORDS: &str = "procs 1000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm\n\
                           up 1100 890 start=1050\n\
                           person 1000 trusted=yes post=yes\n\
                           input 2000 nudge idle_before_ns=5 idle_after_ns=1 window_ns=1\n\
                           input 193500 click idle_before_ns=5 idle_after_ns=1 window_ns=1\n\
                           click 193500 1331 System Settings switch:\n\
                           typed 226500 1331 account 5 Managed_via_Tart unwatched System Settings\n\
                           sheet 262000 1331 System Settings: AXSheet; AXButton \"Quit & Reopen\"\n\
                           click 263500 1331 System Settings Quit_&_Reopen\n\
                           click 274100 890 aterm quit-confirm\n\
                           down 280400 890\n\
                           up 280700 1995 start=280650\n";

    #[test]
    fn the_persons_presses_and_the_apps_processes_are_progress_but_a_repeated_press_is_not() {
        // Every line up to a time, as the wait reads the records then.
        let by = |ms: u64| -> String {
            RECORDS
                .lines()
                .filter(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .and_then(|t| t.parse::<u64>().ok())
                        .is_some_and(|t| t <= ms)
                })
                .map(|l| format!("{l}\n"))
                .collect()
        };
        assert_eq!(
            acts(&by(2_000)),
            1,
            "the first process alone: nudges count not"
        );
        assert_eq!(acts(&by(193_500)), 2);
        assert_eq!(acts(RECORDS), 7);
        assert_eq!(acts(""), 0);
        assert_eq!(acts("click\ntorn\n"), 0);
        let at = |ms: u64| Reading {
            setup: Some("pending:devtools,debugger,vault".into()),
            record: None,
            acts: acts(&by(ms)),
        };
        // The run's looks: setup= never moved from 154.6 s, and the wait went on through each
        // press and the relaunch, each less than a minute after the one before.
        let mut wait = Stall::new(STALL_MS);
        for ms in (154_600..=341_000).step_by(1_000) {
            assert_eq!(wait.see(ms, &at(ms)), Waited::Moving, "at {ms} ms");
        }
        assert_eq!(
            wait.see(341_600, &at(341_600)),
            Waited::Stalled {
                stood: "setup=pending:devtools,debugger,vault (no record)".into(),
                since_ms: 281_600
            },
            "a minute after the look that saw the reopened process, nothing new: stalled"
        );
        // Negative control: the same records without the person's presses or the processes'
        // goings and comings stall a minute after setup= last moved, as before.
        let mut still = Stall::new(STALL_MS);
        let bare = Reading {
            acts: 1,
            ..at(2_000)
        };
        assert_eq!(still.see(154_600, &bare), Waited::Moving);
        assert!(matches!(
            still.see(214_600, &bare),
            Waited::Stalled {
                since_ms: 154_600,
                ..
            }
        ));
        // A press repeated on the same control (an Allow that stays) is no progress: the
        // minute runs from its first press.
        let again = |n: u64| -> String {
            (0..=n)
                .map(|k| {
                    format!(
                        "click {} 1249 UserNotificationCenter Allow\n",
                        1_000 + 3_000 * k
                    )
                })
                .collect()
        };
        assert_eq!(acts(&again(0)), acts(&again(40)));
        // The same press after another act is another dialog's: it counts again.
        let between = format!(
            "{}typed 5000 1331 account 5 x unwatched System Settings\n{}",
            again(0),
            again(0)
        );
        assert_eq!(acts(&between), 3);
        let mut stuck = Stall::new(STALL_MS);
        let pressing = |n: u64| Reading {
            acts: acts(&again(n)),
            ..at(0)
        };
        assert_eq!(stuck.see(1_000, &pressing(0)), Waited::Moving);
        assert_eq!(stuck.see(60_999, &pressing(19)), Waited::Moving);
        assert!(matches!(
            stuck.see(61_000, &pressing(20)),
            Waited::Stalled {
                since_ms: 1_000,
                ..
            }
        ));
    }
}
