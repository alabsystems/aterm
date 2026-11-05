// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The guest side of `lab mac nux`'s milestone captures (`first-launch`): the window the person
//! sees, taken through the guest's own `aterm ctl image` at each milestone of a first launch
//! (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.9, check 3).
//!
//! - `captures EXE DIR OUT SOURCE=FILE...`: follows the lane's records, each looked at every
//!   [`FOLLOW_EVERY`] ([`Source`]: `watch=` the setup record's watch, `census=` the dialog census,
//!   `procs=` the app's process watch, `setup=` the `setup=` readings of the lane's wait on
//!   setup); the milestones the schedule names at one look ([`Schedule::see`]) are captured at
//!   once into DIR, in one frame ([`shoot`]): a capture in a busy guest takes a second or more,
//!   and a frame for each milestone that came meanwhile would show the page later and later.
//!   Runs until it is killed.
//! - `capture EXE DIR OUT KEY [WORDS...]`: one capture now, keyed KEY: the lane's own milestones
//!   (first present, setup's end), which it sees itself.
//!
//! Each capture writes OUT a line for each milestone it shows (`<kind> <epoch ms> <rest>`, as
//! every record of the helper): `capture <ms> <key> <file> <w>x<h> trigger=<ms> begun=<ms>
//! <words>` for a frame in DIR, the one file for every milestone of the frame, or
//! `capture-error <ms> <key> trigger=<ms> <why>`. The lane reads them back
//! (`mac_nux::capture::read_shots`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Out, now_ms};

/// How often `captures` looks at its records: the census and the person sweep every 250 ms, the
/// watches every 50 ms.
const FOLLOW_EVERY: Duration = Duration::from_millis(50);
/// One `aterm ctl image`: a hang detector. A capture with no kept frame waits for the window's
/// next present, which an occluded window may not make; a capture is tried again after it.
const CTL_WITHIN: Duration = Duration::from_secs(10);
/// How long a capture is tried again after the app refused it (a capture in flight holds the
/// artifact hand-off; a window not up yet): a hang detector.
const TRY_WITHIN: Duration = Duration::from_secs(10);
/// How long a relaunched app is asked before it answers: a hang detector (a cold launch of the
/// app in a loaded guest takes seconds).
const ANSWER_WITHIN: Duration = Duration::from_secs(60);
/// The pause between two tries of one capture.
const TRY_EVERY: Duration = Duration::from_millis(250);

/// The records `captures` follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The setup record's watch (`watch`): `at <ms> <the record on one line>`.
    Watch,
    /// The dialog census (`census`): `base`, `new` and `gone` lines.
    Census,
    /// The app's process watch (`procs`): `up` and `down` lines.
    Procs,
    /// The lane's `setup=` readings: `setup <ms> <the field>`.
    Setup,
}

impl Source {
    fn parse(name: &str) -> Option<Source> {
        match name {
            "watch" => Some(Source::Watch),
            "census" => Some(Source::Census),
            "procs" => Some(Source::Procs),
            "setup" => Some(Source::Setup),
            _ => None,
        }
    }
}

/// The dialogs a person answers in a first launch, by the window owner the census names: each
/// one's first window opens a person stage and its last window's going ends it.
pub(crate) const PERSON_STAGES: [(&str, &str); 10] = [
    ("System Settings", "the walk's System Settings page"),
    ("nehelper", "the Local Network ask"),
    (
        "SecurityAgent",
        "an authorization (the Command Line Tools, the debugger)",
    ),
    ("UserNotificationCenter", "a consent alert"),
    ("CoreServicesUIAgent", "a Gatekeeper alert"),
    ("universalAccessAuthWarn", "the Accessibility ask"),
    ("BackgroundTaskManagementAgent", "a Background Items notice"),
    (
        "Install Command Line Developer Tools",
        "Apple's Command Line Tools install dialog",
    ),
    // macOS 27's name for it, cut short (`mac_nux_guest::dialog_kind`).
    (
        "Install Command Line Developer",
        "Apple's Command Line Tools install dialog",
    ),
    (
        "Install Command Line Tools",
        "Apple's Command Line Tools install dialog",
    ),
];

/// The words of `setup=`'s `pending:` list whose rows wait on a person, each a card on the page
/// while it is named: the grants, the vault, the GitHub token, and each sync kind (`sync-…`). A
/// setting the run applies by itself (`dock`, `capslock`, …) is named there while the run still
/// differs from it, which is no card.
pub(crate) const CARD_WORDS: [&str; 7] =
    ["fda", "devtools", "debugger", "lan", "ax", "vault", "repos"];

/// Whether `word` of `setup=`'s `pending:` list is a card's ([`CARD_WORDS`]).
fn is_card(word: &str) -> bool {
    CARD_WORDS.contains(&word) || word.starts_with("sync-")
}

/// A stage's name by the bit the progress record stores for it (`initial.rs` `Stage`).
fn stage_name(bit: u64) -> &'static str {
    match bit {
        1 => "Workstation",
        2 => "Spotlight",
        4 => "Permissions",
        8 => "Secrets",
        16 => "Repositories",
        32 => "Accounts",
        _ => "unknown",
    }
}

/// One milestone the schedule names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Milestone {
    /// When its record's line says it came (the guest's clock, epoch ms).
    pub at_ms: u64,
    /// Its key, a file name's word: `stage-workstation`, `before-nehelper`, `needs-fda-2`.
    pub key: String,
    /// What it is, in words.
    pub why: String,
    /// Asked until the app answers ([`ANSWER_WITHIN`]): a relaunched app, before its control
    /// socket is up.
    pub until_answered: bool,
}

/// `name` as a key's word: lower case, each run of other characters one `-`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// What the schedule has seen of the records so far.
#[derive(Debug, Default)]
pub(crate) struct Schedule {
    /// The stage the setup record named running at its last look.
    running: Option<u64>,
    /// How many times each key was named: its second naming is `<key>-2`.
    named: BTreeMap<String, u32>,
    /// Each person stage's owner's windows on screen.
    open: BTreeMap<String, BTreeSet<u64>>,
    /// Each window the census named, by its number: its owner.
    owners: BTreeMap<u64, String>,
    /// The app's processes started so far.
    ups: u32,
    /// The card words `setup=` named at its last reading.
    cards: BTreeSet<String>,
}

impl Schedule {
    /// The milestone `base` names, counted: `base`, then `base-2`, `base-3`.
    fn milestone(
        &mut self,
        at_ms: u64,
        base: &str,
        why: String,
        until_answered: bool,
    ) -> Milestone {
        let n = self.named.entry(base.to_owned()).or_insert(0);
        *n += 1;
        Milestone {
            at_ms,
            key: if *n == 1 {
                base.to_owned()
            } else {
                format!("{base}-{n}")
            },
            why,
            until_answered,
        }
    }

    /// The milestones one line of `source`'s record names (pure):
    /// - the setup record naming a running stage other than at its last look: `stage-<name>`;
    /// - the census' first window of a person stage's owner ([`PERSON_STAGES`]) with none of its
    ///   windows on screen: `before-<owner>`; its last window going: `after-<owner>` (a window
    ///   on screen at the census' start opens nothing);
    /// - an app process starting after the first: `relaunch` (asked until the app answers);
    /// - `setup=` naming a card's word ([`CARD_WORDS`]) it did not name at its last reading:
    ///   `needs-<word>`; one it named there and no longer names: `cleared-<word>`.
    ///
    /// The first launch's first present and setup's end are the lane's own (`capture`).
    pub(crate) fn see(&mut self, source: Source, line: &str) -> Vec<Milestone> {
        let mut words = line.split_whitespace();
        let (Some(kind), Some(at_ms)) = (words.next(), words.next().and_then(|w| w.parse().ok()))
        else {
            return Vec::new();
        };
        let rest: Vec<&str> = words.collect();
        match (source, kind) {
            (Source::Watch, "at") => {
                let running = aterm_json::from_str::<aterm_json::Value>(&rest.join(" "))
                    .ok()
                    .and_then(|v| v.get("running").and_then(aterm_json::Value::as_u64));
                let before = std::mem::replace(&mut self.running, running);
                match running {
                    Some(bit) if Some(bit) != before => {
                        let name = stage_name(bit);
                        vec![self.milestone(
                            at_ms,
                            &format!("stage-{}", slug(name)),
                            format!("{name} started"),
                            false,
                        )]
                    }
                    _ => Vec::new(),
                }
            }
            (Source::Census, "base" | "new") => {
                let [id, _pid, _layer, _size, owner @ ..] = rest.as_slice() else {
                    return Vec::new();
                };
                let (Ok(id), owner) = (id.parse::<u64>(), owner.join(" ")) else {
                    return Vec::new();
                };
                let Some((_, what)) = PERSON_STAGES.iter().find(|(o, _)| *o == owner) else {
                    return Vec::new();
                };
                self.owners.insert(id, owner.clone());
                let windows = self.open.entry(owner.clone()).or_default();
                let first = windows.is_empty();
                windows.insert(id);
                if kind == "new" && first {
                    vec![self.milestone(
                        at_ms,
                        &format!("before-{}", slug(&owner)),
                        format!("{what} up ({owner})"),
                        false,
                    )]
                } else {
                    Vec::new()
                }
            }
            (Source::Census, "gone") => {
                let Some(owner) = rest
                    .first()
                    .and_then(|id| id.parse::<u64>().ok())
                    .and_then(|id| self.owners.remove(&id).map(|o| (id, o)))
                    .and_then(|(id, owner)| {
                        let windows = self.open.entry(owner.clone()).or_default();
                        windows.remove(&id);
                        windows.is_empty().then_some(owner)
                    })
                else {
                    return Vec::new();
                };
                let what = PERSON_STAGES
                    .iter()
                    .find(|(o, _)| *o == owner)
                    .map_or("a dialog", |(_, w)| *w);
                vec![self.milestone(
                    at_ms,
                    &format!("after-{}", slug(&owner)),
                    format!("{what} gone ({owner})"),
                    false,
                )]
            }
            (Source::Procs, "up") => {
                self.ups += 1;
                if self.ups > 1 {
                    let pid = rest.first().copied().unwrap_or("?");
                    vec![self.milestone(
                        at_ms,
                        "relaunch",
                        format!("the app started again (pid {pid})"),
                        true,
                    )]
                } else {
                    Vec::new()
                }
            }
            (Source::Setup, "setup") => {
                let field = rest.first().copied().unwrap_or("");
                let now: BTreeSet<String> = field
                    .strip_prefix("pending:")
                    .unwrap_or("")
                    .split(',')
                    .filter(|w| is_card(w))
                    .map(str::to_owned)
                    .collect();
                let came: Vec<String> = now.difference(&self.cards).cloned().collect();
                let went: Vec<String> = self.cards.difference(&now).cloned().collect();
                self.cards = now;
                let mut out = Vec::new();
                for word in came {
                    out.push(self.milestone(
                        at_ms,
                        &format!("needs-{}", slug(&word)),
                        format!("setup= names {word} ({field})"),
                        false,
                    ));
                }
                for word in went {
                    out.push(self.milestone(
                        at_ms,
                        &format!("cleared-{}", slug(&word)),
                        format!("setup= no longer names {word} ({field})"),
                        false,
                    ));
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

/// A record followed from where it was last read: its whole new lines.
struct Follow {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl Follow {
    fn new(path: &str) -> Follow {
        Follow {
            path: PathBuf::from(path),
            offset: 0,
            partial: String::new(),
        }
    }

    /// The whole lines written since the last read; a file that shrank (made fresh) is read from
    /// its start again.
    fn lines(&mut self) -> Vec<String> {
        let Ok((mut f, meta)) = aterm_uds::regular_file::open(&self.path) else {
            return Vec::new();
        };
        let len = meta.len();
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if f.seek(std::io::SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let rem = len.saturating_sub(self.offset).min(16 * 1024 * 1024);
        let mut bytes = Vec::with_capacity(usize::try_from(rem).unwrap_or(0).saturating_add(1));
        if f.take(16 * 1024 * 1024).read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        self.offset += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let mut out = Vec::new();
        while let Some(at) = self.partial.find('\n') {
            out.push(self.partial[..at].to_owned());
            self.partial.drain(..=at);
        }
        out
    }
}

/// `captures EXE DIR OUT SOURCE=FILE...` (the module docs).
///
/// # Errors
/// Usage, or OUT or DIR could not be made.
pub(crate) fn captures(args: &[String]) -> Result<(), String> {
    let usage = "captures EXE DIR OUT SOURCE=FILE... (SOURCE: watch, census, procs, setup)";
    let [exe, dir, out, sources @ ..] = args else {
        return Err(usage.to_owned());
    };
    let mut follows = Vec::new();
    for s in sources {
        let (name, file) = s.split_once('=').ok_or(usage)?;
        let source = Source::parse(name).ok_or_else(|| format!("{name}: {usage}"))?;
        follows.push((source, Follow::new(file)));
    }
    if follows.is_empty() {
        return Err(usage.to_owned());
    }
    let dir = Path::new(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let out = Out::open(out)?;
    out.line("captures", &format!("every={}", FOLLOW_EVERY.as_millis()));
    let mut schedule = Schedule::default();
    let mut shots = 0usize;
    loop {
        let mut due = Vec::new();
        for (source, follow) in &mut follows {
            for line in follow.lines() {
                due.extend(schedule.see(*source, &line));
            }
        }
        due.sort_by_key(|m| m.at_ms);
        if let Some(first) = due.first() {
            shots += 1;
            let stem = format!("{shots:02}-{}", first.key);
            shoot(exe, dir, &out, &stem, &due);
        }
        std::thread::sleep(FOLLOW_EVERY);
    }
}

/// `capture EXE DIR OUT KEY [WORDS...]` (the module docs).
///
/// # Errors
/// Usage, or OUT or DIR could not be made.
pub(crate) fn capture(args: &[String]) -> Result<(), String> {
    let usage = "capture EXE DIR OUT KEY [WORDS...]";
    let [exe, dir, out, key, words @ ..] = args else {
        return Err(usage.to_owned());
    };
    let dir = Path::new(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let m = Milestone {
        at_ms: now_ms(),
        key: slug(key),
        why: words.join(" "),
        until_answered: false,
    };
    let stem = format!("lane-{}", m.key);
    shoot(exe, dir, &Out::open(out)?, &stem, &[m]);
    Ok(())
}

/// Capture `batch` into `dir` as one frame, `<stem>.png` ([`ctl_image`]), tried again while the
/// app refuses it ([`TRY_WITHIN`], or [`ANSWER_WITHIN`] for a batch with a relaunch), and write
/// its lines to `out` ([`record_lines`]).
fn shoot(exe: &str, dir: &Path, out: &Out, stem: &str, batch: &[Milestone]) {
    let begun = now_ms();
    let started = Instant::now();
    let within = if batch.iter().any(|m| m.until_answered) {
        ANSWER_WITHIN
    } else {
        TRY_WITHIN
    };
    let file = format!("{stem}.png");
    let taken = loop {
        match ctl_image(exe, &dir.join(&file)) {
            Ok(size) => break Ok(size),
            Err(why) if started.elapsed() >= within => break Err(why),
            Err(_) => std::thread::sleep(TRY_EVERY),
        }
    };
    let taken = taken.as_ref().map(|size| (file.as_str(), size.as_str()));
    for (kind, rest) in record_lines(batch, taken.map_err(String::as_str), begun) {
        out.line(kind, &rest);
    }
}

/// The record's lines for one capture of `batch` begun at `begun` (pure): a `capture` line for
/// each milestone, every one naming the one frame (`taken`: its file and size), or a
/// `capture-error` line for each, with why it failed on one line.
fn record_lines(
    batch: &[Milestone],
    taken: Result<(&str, &str), &str>,
    begun: u64,
) -> Vec<(&'static str, String)> {
    batch
        .iter()
        .map(|m| match taken {
            Ok((file, size)) => (
                "capture",
                format!(
                    "{} {file} {size} trigger={} begun={begun} {}",
                    m.key, m.at_ms, m.why
                ),
            ),
            Err(why) => (
                "capture-error",
                format!(
                    "{} trigger={} {}",
                    m.key,
                    m.at_ms,
                    why.split_whitespace().collect::<Vec<_>>().join(" ")
                ),
            ),
        })
        .collect()
}

/// The reply of `aterm ctl image <name>`: `OK <w> <h> <path>`, its size and the file it wrote;
/// `None` for any other reply.
pub(crate) fn image_reply(reply: &str) -> Option<(String, PathBuf)> {
    let line = reply.lines().find(|l| l.starts_with("OK "))?;
    let mut parts = line.splitn(4, ' ');
    let _ok = parts.next()?;
    let w: u32 = parts.next()?.parse().ok()?;
    let h: u32 = parts.next()?.parse().ok()?;
    let path = parts.next()?.trim();
    (w > 0 && h > 0 && path.starts_with('/')).then(|| (format!("{w}x{h}"), PathBuf::from(path)))
}

/// One `aterm ctl image` of the app's front window into the app's own images folder, moved to
/// `to`: its size (`<w>x<h>`), or why not.
fn ctl_image(exe: &str, to: &Path) -> Result<String, String> {
    let name = format!(
        "nux-{}.png",
        to.file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    );
    let reply = aterm_uds::deadline::output_within(
        aterm_uds::spawnfd::command(exe).args(["ctl", "image", &name]),
        CTL_WITHIN,
    )
    .map_err(|e| format!("aterm ctl image: {e}"))?;
    let text = String::from_utf8_lossy(&reply.stdout);
    let Some((size, wrote)) = image_reply(&text) else {
        return Err(format!(
            "aterm ctl image: {} {}",
            text.trim(),
            String::from_utf8_lossy(&reply.stderr).trim()
        ));
    };
    let (bytes, _) = aterm_uds::atomic_file::read_regular(&wrote, 64 << 20)
        .map_err(|e| format!("{}: {e}", wrote.display()))?;
    aterm_uds::atomic_file::write(to, &bytes, 0o644)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    let _ = std::fs::remove_file(&wrote);
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each line through the schedule, each milestone as `(at, key)`.
    fn run(schedule: &mut Schedule, lines: &[(Source, &str)]) -> Vec<(u64, String)> {
        lines
            .iter()
            .flat_map(|(source, line)| schedule.see(*source, line))
            .map(|m| (m.at_ms, m.key))
            .collect()
    }

    /// A first launch with a person: the records as the lane's watches write them, interleaved
    /// in the order they came.
    fn first_launch() -> Vec<(Source, &'static str)> {
        vec![
            (Source::Census, "census 1000 every=250 windows=2"),
            (Source::Census, "base 1000 10 300 25 1512x33 Window Server"),
            (Source::Census, "base 1000 11 301 0 800x600 Finder"),
            (Source::Watch, "at 1000 absent"),
            (
                Source::Procs,
                "procs 1000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm",
            ),
            (Source::Procs, "up 1500 700 start=1490"),
            (Source::Census, "new 2100 40 700 0 900x644 aterm"),
            (Source::Setup, "setup 2200 -"),
            (
                Source::Watch,
                r#"at 2300 {"profile":"default","state":"pending","done":0,"running":1}"#,
            ),
            (Source::Setup, "setup 3200 pending:dock,desktop"),
            (
                Source::Watch,
                r#"at 3400 {"profile":"default","state":"pending","done":1,"running":1}"#,
            ),
            (
                Source::Watch,
                r#"at 3500 {"profile":"default","state":"pending","done":1,"running":2}"#,
            ),
            (
                Source::Watch,
                r#"at 3600 {"profile":"default","state":"pending","done":3,"running":4}"#,
            ),
            (Source::Census, "new 3700 41 120 0 420x210 nehelper"),
            (Source::Setup, "setup 4200 pending:fda,lan,dock"),
            (Source::Census, "new 4800 42 130 0 700x600 System Settings"),
            (Source::Census, "new 5000 43 130 8 400x200 System Settings"),
            (Source::Census, "gone 9000 41"),
            (Source::Census, "gone 12000 43"),
            (Source::Setup, "setup 13200 pending:fda"),
            (Source::Census, "gone 15000 42"),
            (Source::Procs, "down 15100 700"),
            (Source::Procs, "up 15600 701 start=15590"),
            (Source::Setup, "setup 18200 pending"),
            (Source::Census, "new 19000 44 140 0 400x300 SecurityAgent"),
            (Source::Census, "gone 25000 44"),
            (Source::Setup, "setup 26200 pending:fda"),
            (
                Source::Watch,
                r#"at 27000 {"profile":"default","state":"pending","done":7,"running":8}"#,
            ),
            (Source::Setup, "setup 30200 done"),
        ]
    }

    #[test]
    fn each_milestone_of_a_first_launch_is_named_once_in_the_order_it_came() {
        let mut schedule = Schedule::default();
        assert_eq!(
            run(&mut schedule, &first_launch()),
            [
                (2300, "stage-workstation"),
                (3500, "stage-spotlight"),
                (3600, "stage-permissions"),
                (3700, "before-nehelper"),
                (4200, "needs-fda"),
                (4200, "needs-lan"),
                (4800, "before-system-settings"),
                (9000, "after-nehelper"),
                (13200, "cleared-lan"),
                (15000, "after-system-settings"),
                (15600, "relaunch"),
                (18200, "cleared-fda"),
                (19000, "before-securityagent"),
                (25000, "after-securityagent"),
                (26200, "needs-fda-2"),
                (27000, "stage-secrets"),
                (30200, "cleared-fda-2"),
            ]
            .map(|(at, key)| (at, key.to_owned()))
        );
    }

    /// The negative control: lines that are no milestone name none, so the schedule above is
    /// not one that names every line.
    #[test]
    fn lines_that_are_no_milestone_name_none_and_the_same_lines_with_milestones_do() {
        let mut schedule = Schedule::default();
        let quiet = [
            // The app's own windows, the desktop's, and a window there before the census began.
            (Source::Census, "new 2100 40 700 0 900x644 aterm"),
            (Source::Census, "new 2150 45 90 0 1512x33 Dock"),
            (Source::Census, "base 1000 46 130 0 700x600 System Settings"),
            (Source::Census, "gone 2200 45"),
            // A window of an owner already on screen.
            (Source::Census, "new 2300 47 130 0 300x100 System Settings"),
            // The same stage looked at twice, the record's other changes, and no stage.
            (
                Source::Watch,
                r#"at 2400 {"state":"pending","done":0,"running":1}"#,
            ),
            (Source::Watch, r#"at 2500 {"state":"pending","done":0}"#),
            (Source::Watch, "at 2600 absent"),
            (Source::Watch, "at 2700 not json"),
            // The first launch's own process, and a process going.
            (Source::Procs, "up 1500 700 start=1490"),
            (Source::Procs, "down 2800 700"),
            // Settings the run applies by itself, and a reading that repeats.
            (Source::Setup, "setup 3000 pending:dock,desktop,capslock"),
            (Source::Setup, "setup 3100 pending:dock,desktop,capslock"),
            (Source::Setup, "setup 3200 pending"),
            // Torn and unknown lines.
            (Source::Setup, "setup"),
            (Source::Census, "new x 47"),
            (Source::Watch, "killed 2900 700 running=1 sent=yes"),
        ];
        let named = run(&mut schedule, &quiet);
        assert_eq!(
            named,
            [(2400, "stage-workstation".to_owned())],
            "only the first look at a running stage is one"
        );
        // The same lines with one milestone each among them do name it: the control can fail.
        let mut schedule = Schedule::default();
        let mut louder = quiet.to_vec();
        louder.push((Source::Census, "new 3300 48 120 0 420x210 nehelper"));
        louder.push((Source::Procs, "up 3400 702 start=3390"));
        louder.push((Source::Setup, "setup 3500 pending:dock,repos,sync-peers"));
        assert_eq!(
            run(&mut schedule, &louder)
                .into_iter()
                .map(|(_, key)| key)
                .collect::<Vec<_>>(),
            [
                "stage-workstation",
                "before-nehelper",
                "relaunch",
                "needs-repos",
                "needs-sync-peers"
            ]
        );
    }

    #[test]
    fn a_window_there_at_the_census_start_ends_its_stage_when_it_goes() {
        let mut schedule = Schedule::default();
        let lines = [
            (Source::Census, "base 1000 46 130 0 700x600 System Settings"),
            (Source::Census, "gone 4000 46"),
            (Source::Census, "new 5000 49 130 0 700x600 System Settings"),
        ];
        assert_eq!(
            run(&mut schedule, &lines),
            [
                (4000, "after-system-settings".to_owned()),
                (5000, "before-system-settings".to_owned())
            ]
        );
    }

    #[test]
    fn only_a_relaunch_is_asked_until_the_app_answers() {
        let mut schedule = Schedule::default();
        let mut all = Vec::new();
        for (source, line) in first_launch() {
            all.extend(schedule.see(source, line));
        }
        let waiting: Vec<&str> = all
            .iter()
            .filter(|m| m.until_answered)
            .map(|m| m.key.as_str())
            .collect();
        assert_eq!(waiting, ["relaunch"]);
    }

    /// One look's milestones are one frame: each its own line, all naming the one file; a failed
    /// capture is a failure line for each, naming no file.
    #[test]
    fn every_milestone_of_one_capture_names_its_one_frame() {
        let batch: Vec<Milestone> = [(5492, "stage-permissions-2"), (6835, "before-nehelper")]
            .into_iter()
            .map(|(at_ms, key)| Milestone {
                at_ms,
                key: key.to_owned(),
                why: format!("{key} came"),
                until_answered: false,
            })
            .collect();
        assert_eq!(
            record_lines(&batch, Ok(("04-stage-permissions-2.png", "1248x964")), 8669),
            [
                (
                    "capture",
                    "stage-permissions-2 04-stage-permissions-2.png 1248x964 trigger=5492 \
                     begun=8669 stage-permissions-2 came"
                ),
                (
                    "capture",
                    "before-nehelper 04-stage-permissions-2.png 1248x964 trigger=6835 \
                     begun=8669 before-nehelper came"
                ),
            ]
            .map(|(kind, rest)| (kind, rest.to_owned()))
        );
        assert_eq!(
            record_lines(&batch, Err("aterm ctl image:\n  refused"), 8669),
            [
                (
                    "capture-error",
                    "stage-permissions-2 trigger=5492 aterm ctl image: refused"
                ),
                (
                    "capture-error",
                    "before-nehelper trigger=6835 aterm ctl image: refused"
                ),
            ]
            .map(|(kind, rest)| (kind, rest.to_owned()))
        );
        assert!(record_lines(&[], Ok(("05-x.png", "1x1")), 1).is_empty());
    }

    #[test]
    fn a_key_is_a_file_names_word() {
        assert_eq!(slug("System Settings"), "system-settings");
        assert_eq!(
            slug("Install Command Line Developer Tools"),
            "install-command-line-developer-tools"
        );
        assert_eq!(slug("sync-peers"), "sync-peers");
        assert_eq!(slug("  a/b  "), "a-b");
    }

    #[test]
    fn only_an_ok_reply_with_a_size_and_a_path_is_a_frame() {
        assert_eq!(
            image_reply("OK 1800 1288 /Users//admin/Library/aterm/images/nux-01.png\n"),
            Some((
                "1800x1288".to_owned(),
                PathBuf::from("/Users//admin/Library/aterm/images/nux-01.png")
            ))
        );
        assert_eq!(image_reply("OK 0 0 /x.png\n"), None, "nothing captured");
        assert_eq!(image_reply("ERR image: no window to capture\n"), None);
        assert_eq!(image_reply("OK 10 10 relative.png\n"), None);
    }

    #[test]
    fn a_follow_reads_whole_lines_once_and_again_from_the_start_of_a_fresh_file() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("record.txt");
        std::fs::write(&path, "at 1 a\nat 2 b").expect("write");
        let mut follow = Follow::new(&path.to_string_lossy());
        assert_eq!(follow.lines(), ["at 1 a"]);
        assert!(follow.lines().is_empty(), "a line is read once");
        std::fs::write(&path, "at 1 a\nat 2 b\nat 3 c\n").expect("write");
        assert_eq!(
            follow.lines(),
            ["at 2 b", "at 3 c"],
            "the torn line, once whole"
        );
        std::fs::write(&path, "at 9 z\n").expect("write");
        assert_eq!(
            follow.lines(),
            ["at 9 z"],
            "a file made fresh is read from its start"
        );
    }
}
