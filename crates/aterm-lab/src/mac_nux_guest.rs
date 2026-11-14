// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `aterm-lab-guest` (`src/guest/main.rs`) writes in a `lab mac nux` guest, read here: the
//! dialog census ([`read_census`], [`dialogs`]), the reader's words of each dialog
//! ([`read_reader`]), the person at the console ([`read_person`],
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

/// The words of macOS's Local Network question, as the reader reads its alert: "Allow “<app>” to
/// find devices on local networks?" (macOS 27.2, `docs/measured/local-network-exemption-2026-11-05.md`).
pub const LOCAL_NETWORK_WORDS: &str = "find devices on local networks";

/// The app macOS's Local Network question names in a reading's `words` ([`LOCAL_NETWORK_WORDS`]):
/// what stands between `Allow ` and ` to find devices`, its quotes (curly, straight, or straight
/// escaped as the reader writes them) dropped; `None` when the words are no such question. Pure.
#[must_use]
pub fn local_network_asker(words: &str) -> Option<String> {
    let (before, _) = words.split_once(LOCAL_NETWORK_WORDS)?;
    let before = before.trim_end().strip_suffix(" to").unwrap_or(before);
    let (_, name) = before.rsplit_once("Allow ")?;
    let name =
        name.trim_matches(|c: char| c.is_whitespace() || matches!(c, '“' | '”' | '"' | '\\'));
    (!name.is_empty()).then(|| name.to_owned())
}

/// What kind of dialog an owner's window is, and its words when the reader read them (`words`):
/// macOS 27 shows its Local Network question in a UserNotificationCenter window, which is named
/// by its words, since that owner's other windows are consent alerts.
#[must_use]
pub fn dialog_kind(owner: &str, words: Option<&str>) -> &'static str {
    match owner {
        "nehelper" => "Local Network",
        "UserNotificationCenter" if words.is_some_and(|w| local_network_asker(w).is_some()) => {
            "Local Network"
        }
        "CoreServicesUIAgent" => "Gatekeeper",
        // macOS 27 names the installer's windows `Install Command Line Developer`, cut short
        // (nux round 9's devtools-control row, the census on a guest without the tools); an
        // older macOS names them whole.
        o if o.starts_with("Install Command Line") => APPLE_DEVTOOLS_DIALOG,
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

/// One dialog owner the census saw after a mark, of one kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub owner: String,
    pub kind: &'static str,
    /// Its first window, in ms after the mark.
    pub first_ms: u64,
    pub windows: usize,
}

/// The owners of windows that appeared at or after `since_ms` (epoch ms), each with when its first
/// came and how many came, the shell's own ([`SHELL_OWNERS`]) left out, in the order they came;
/// an owner's windows of two kinds are two dialogs. A window's kind reads the reader's words of
/// it when they are macOS's Local Network question (`read`, [`Reader::words`]): a reading of the
/// window's process taken while the window was up (from its coming to its going, or on, when it
/// never went). Pure.
#[must_use]
pub fn dialogs(census: &Census, since_ms: u64, read: &[Words]) -> Vec<Dialog> {
    let mut out: Vec<Dialog> = Vec::new();
    for w in census
        .new
        .iter()
        .filter(|w| w.at_ms >= since_ms && !SHELL_OWNERS.contains(&w.owner.as_str()))
    {
        let gone = census.gone.get(&w.id).copied();
        let words = read
            .iter()
            .filter(|r| r.pid == w.pid && r.at_ms >= w.at_ms && gone.is_none_or(|g| r.at_ms <= g))
            .map(|r| r.words.as_str())
            .find(|words| local_network_asker(words).is_some());
        let kind = dialog_kind(&w.owner, words);
        match out
            .iter_mut()
            .find(|d| d.owner == w.owner && d.kind == kind)
        {
            Some(d) => d.windows += 1,
            None => out.push(Dialog {
                owner: w.owner.clone(),
                kind,
                first_ms: w.at_ms - since_ms,
                windows: 1,
            }),
        }
    }
    out
}

/// The kind [`dialog_kind`] gives Apple's developer-tools install dialog, the one `/usr/bin/git`
/// and the other `/usr/bin` stubs raise on a Mac without the Command Line Tools.
pub const APPLE_DEVTOOLS_DIALOG: &str = "Xcode install stub";

/// The dialogs among `dialogs` that are Apple's developer-tools install dialog
/// ([`APPLE_DEVTOOLS_DIALOG`]). Pure.
#[must_use]
pub fn apple_devtools_dialogs(dialogs: &[Dialog]) -> Vec<&Dialog> {
    dialogs
        .iter()
        .filter(|d| d.kind == APPLE_DEVTOOLS_DIALOG)
        .collect()
}

/// The `setup=` rows only a person can settle: a privacy switch in System Settings (Full Disk
/// Access, Developer Tools, Accessibility), which aterm offers (`;offered:`), never answers, and
/// waits on only once a person asked for the batch; and the debugger's developer mode on an
/// account that may not turn it on, which needs an administrator. An admin account's debugger row
/// is neither offered nor waited on: it reads Waiting until setup's one Mac password (the Mac
/// Setup tab) turns developer mode on. macOS's Local Network question is none of them: aterm's own
/// LAN traffic goes through its root LAN broker, which macOS never asks about
/// (`docs/DESIGN-lan-broker-2026-11-05.md`), so a `lan` row waiting on a press is one aterm could
/// have spared.
pub const MACOS_PERSON_ROWS: [&str; 4] = ["fda", "devtools", "ax", "debugger"];

/// The `setup=` word of a System State Local Network row. A broker build has no such row
/// (`docs/DESIGN-lan-broker-2026-11-05.md`): one named in any reading is a Local Network step
/// System State should not have ([`Presses::local_network_step`]).
pub const LOCAL_NETWORK_ROW: &str = "lan";

/// The rows after `key:` in a `setup=` field (`;needs:<row,...>` or `;offered:<row,...>`,
/// aterm's `system_state_access::setup_field`). Pure.
#[must_use]
pub fn setup_rows<'a>(field: &'a str, key: &str) -> Vec<&'a str> {
    field
        .split(';')
        .filter_map(|part| part.strip_prefix(key)?.strip_prefix(':'))
        .flat_map(|rows| rows.split(','))
        .filter(|row| !row.is_empty())
        .collect()
}

/// What a first launch's `setup=` readings asked of the person, each row once in the order first
/// seen: the rows that waited on a press (`;needs:`) and the macOS grants on offer (`;offered:`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Presses {
    pub needs: Vec<String>,
    pub offered: Vec<String>,
    /// Every row any reading named (`pending:`, `unread:`, `asking:`, `needs:`, `offered:`).
    pub named: Vec<String>,
}

impl Presses {
    /// Take one `setup=` field into the tally.
    pub fn see(&mut self, field: &str) {
        for (key, into) in [("needs", &mut self.needs), ("offered", &mut self.offered)] {
            for row in setup_rows(field, key) {
                if !into.iter().any(|seen| seen == row) {
                    into.push(row.to_owned());
                }
            }
        }
        for key in ["pending", "unread", "asking", "needs", "offered"] {
            for row in setup_rows(field, key) {
                if !self.named.iter().any(|seen| seen == row) {
                    self.named.push(row.to_owned());
                }
            }
        }
    }

    /// Whether any reading named System State's Local Network row ([`LOCAL_NETWORK_ROW`]): a
    /// Local Network step a broker build should never show.
    #[must_use]
    pub fn local_network_step(&self) -> bool {
        self.named.iter().any(|row| row == LOCAL_NETWORK_ROW)
    }

    /// The presses aterm could have made itself: every row that waited on a press while nobody
    /// asked for anything (`person` false: a grant macOS requires is offered then, never waited
    /// on), or, once a person at the console asked for the batch, each such row but the grants
    /// only a person gives ([`MACOS_PERSON_ROWS`]).
    #[must_use]
    pub fn avoidable(&self, person: bool) -> Vec<&str> {
        self.needs
            .iter()
            .map(String::as_str)
            .filter(|row| !person || !MACOS_PERSON_ROWS.contains(row))
            .collect()
    }
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

/// One reading the reader wrote: when, the dialog's process and owner, its controls on one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    pub at_ms: u64,
    pub pid: i32,
    pub owner: String,
    pub words: String,
}

/// The reader's record (`aterm-lab-guest reader`): whether it had Accessibility, and the words
/// of each dialog it read, never answered.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reader {
    pub trusted: Option<bool>,
    /// Each reading, in the order written: the reader writes a process's reading again only when
    /// it changed, or when that process's windows went and came back.
    pub words: Vec<Words>,
}

/// Read the reader's record: a `reader trusted=…` line, then `words <pid> <owner>: <controls>`
/// lines (an owner is one word: the window list's owner names the lane reads have none with a
/// space but System Settings, which the reader never reads).
#[must_use]
pub fn read_reader(text: &str) -> Reader {
    let mut r = Reader::default();
    for (kind, at, rest) in text.lines().filter_map(split_line) {
        match kind {
            "reader" => {
                r.trusted = rest
                    .split_whitespace()
                    .find_map(|kv| kv.strip_prefix("trusted="))
                    .map(|v| v == "yes");
            }
            "words" => {
                if let Some((pid, rest)) = rest.split_once(' ')
                    && let Ok(pid) = pid.parse()
                    && let Some((owner, words)) = rest.split_once(": ")
                {
                    r.words.push(Words {
                        at_ms: at,
                        pid,
                        owner: owner.to_owned(),
                        words: words.to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    r
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

impl std::fmt::Display for Record {
    /// `state=<s> done=<stages|none> running=<stage|none>`, each stage by its name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let done: Vec<&str> = (0..6)
            .map(|i| 1u64 << i)
            .filter(|bit| self.done & bit != 0)
            .map(stage_name)
            .collect();
        write!(
            f,
            "state={} done={} running={}",
            self.state,
            if done.is_empty() {
                "none".to_owned()
            } else {
                done.join(",")
            },
            self.running.map_or("none", stage_name)
        )
    }
}

/// The last progress record the watch read, if it read one.
#[must_use]
pub fn last_record(looks: &[Look]) -> Option<Record> {
    looks
        .iter()
        .rev()
        .find_map(|l| l.text.as_deref().and_then(read_record))
}

/// The value of `setup=` in `aterm ctl status`' line; `None` from a build without the field.
#[must_use]
pub fn setup_field(status: &str) -> Option<&str> {
    status
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("setup="))
}

/// The guest's network place in the same status reading as `setup=`.
#[must_use]
pub fn network_field(status: &str) -> Option<&str> {
    status
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("net="))
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

/// The shells a first tab runs: what `program=` names for a tab at its prompt.
const SHELLS: [&str; 7] = ["zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh"];

/// Each listed session's sid and `program=` value, in `aterm ctl ls`' order ([`first_session`]'s
/// lines; `-` for a line without the field). Pure.
#[must_use]
pub fn session_programs(ls: &str) -> Vec<(&str, &str)> {
    ls.lines()
        .filter_map(|line| {
            let sid = first_session(line)?;
            let program = line
                .split_whitespace()
                .find_map(|kv| kv.strip_prefix("program="))
                .unwrap_or("-");
            Some((sid, program))
        })
        .collect()
}

/// The sid of the first listed session whose foreground is a shell ([`SHELLS`]): the tab a
/// typed line runs in: a tab listed before it may run another program (a Command Line Tools
/// tab running `aterm init`, an agent), which a typed line must never reach. Pure.
#[must_use]
pub fn shell_session(ls: &str) -> Option<&str> {
    session_programs(ls)
        .into_iter()
        .find(|(_, program)| SHELLS.contains(program))
        .map(|(sid, _)| sid)
}

/// Where setup stands in one look at the guest: `setup=` (8b NUX-5) and the progress record, and
/// how many distinct things the person did and the app's processes did so far ([`acts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    pub setup: Option<String>,
    pub network: Option<String>,
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
    /// Done: `setup=done`, `read-back:<rows>` (everything else as for `done`, those rows set up
    /// as last read longer ago than a reading stands, which is how a finished run nobody watches
    /// reads ten minutes on), or `asking:vault` when the same reading places the guest on a
    /// remote network or offline: no wait of the lab's answers that vault request. Other
    /// requests, and unknown network places, remain unfinished. From a build without the field,
    /// the record must have completed. Their tails (`;offered:`, `;asking:`, `;at-login:`,
    /// `;changed:`) hold nothing
    /// back. A head that names a step left or a row unread is not done.
    #[must_use]
    pub fn done(&self) -> bool {
        match &self.setup {
            Some(field) => {
                let head = field.split(';').next().unwrap_or_default();
                head == "done"
                    || head
                        .strip_prefix("read-back:")
                        .is_some_and(|rows| !rows.is_empty())
                    || (head == "asking:vault"
                        && matches!(self.network.as_deref(), Some("remote" | "offline")))
            }
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
        let mut said = format!(
            "setup={} ({record})",
            self.setup.as_deref().unwrap_or("absent")
        );
        if let Some(network) = &self.network {
            said.push_str(" net=");
            said.push_str(network);
        }
        said
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
#[path = "mac_nux_guest_tests.rs"]
mod tests;
