// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `aterm-lab-guest` (`src/guest/main.rs`) writes in a `lab mac nux` guest, read here: the
//! dialog census ([`read_census`], [`dialogs`]), the person at the console ([`read_person`],
//! [`hid_proof`], [`click_latencies`]) and the setup record's watch ([`read_watch`],
//! [`stage_starts`]); and the wait on setup ([`Stall`]), which reads `setup=` from the guest's own
//! `aterm ctl status` and the progress record, and stops a minute after either last moved.
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

/// One input the person posted, with the idle clock read just before and after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub at_ms: u64,
    pub what: String,
    pub before_ns: Option<u64>,
    pub after_ns: Option<u64>,
}

/// The person's record.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// Accessibility and the event-post grant, as the helper found them.
    pub trusted: Option<bool>,
    pub may_post: Option<bool>,
    pub inputs: Vec<Input>,
    /// Each dialog window it saw first: when, its number, its owner.
    pub seen: Vec<(u64, u64, String)>,
    /// Each press: when, the owner, what it pressed.
    pub clicks: Vec<(u64, String, String)>,
    /// Each password typed: when, the owner.
    pub typed: Vec<(u64, String)>,
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
            }
            "input" => p.inputs.push(Input {
                at_ms: at,
                what: rest.split_whitespace().next().unwrap_or("").to_owned(),
                before_ns: ns(rest, "idle_before_ns"),
                after_ns: ns(rest, "idle_after_ns"),
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
                let mut parts = rest.splitn(2, ' ');
                let _pid = parts.next();
                if let Some((owner, _chars)) = parts.next().and_then(|r| r.rsplit_once(' ')) {
                    p.typed.push((at, owner.to_owned()));
                }
            }
            _ => {}
        }
    }
    p
}

/// The idle clock right after an input that reached the HID system: it restarted from 0 then.
pub const RESET_NS: u64 = 200_000_000;

/// Whether the person's inputs reset `HIDIdleTime`, as each input's readings show.
#[derive(Debug, PartialEq, Eq)]
pub enum HidProof {
    /// Every input reset it: how many, and the largest reading just after one.
    Reset { inputs: usize, worst_after_ns: u64 },
    /// Not shown, and why: never a pass.
    NotMeasured(String),
}

/// [`HidProof`] over a person record: proven only when every input's reading after it is under
/// [`RESET_NS`] and below its reading before.
#[must_use]
pub fn hid_proof(p: &Person) -> HidProof {
    if p.inputs.is_empty() {
        return HidProof::NotMeasured("the person posted no input".into());
    }
    let mut worst = 0;
    let mut stuck = 0;
    for input in &p.inputs {
        let (Some(before), Some(after)) = (input.before_ns, input.after_ns) else {
            return HidProof::NotMeasured("the HID idle clock was unreadable".into());
        };
        if after >= RESET_NS || after >= before {
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

/// Where setup stands in one look at the guest: `setup=` (8b NUX-5) and the progress record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    pub setup: Option<String>,
    pub record: Option<Record>,
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

/// The wait on setup: done, or stopped once setup stood still for its limit. Its clock is the
/// caller's (ms), so a test gives it one.
#[derive(Debug)]
pub struct Stall {
    limit_ms: u64,
    last: Option<(String, u64)>,
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
            Some((last, since)) if *last == said => {
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
                self.last = Some((said, now_ms));
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
                    input 200 nudge idle_before_ns=4800000000 idle_after_ns=21000000\n\
                    seen 300 77 nehelper\n\
                    input 900 click idle_before_ns=700000000 idle_after_ns=19000000\n\
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
            "input 200 nudge idle_before_ns=4800000000 idle_after_ns=4820000000\n\
             input 5200 nudge idle_before_ns=9800000000 idle_after_ns=9820000000\n",
        );
        assert!(
            matches!(hid_proof(&stuck), HidProof::NotMeasured(why) if why.starts_with("2 of 2"))
        );
        assert!(matches!(
            hid_proof(&read_person("input 1 nudge idle_before_ns=? idle_after_ns=?\n")),
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
                record: completed.clone()
            }
            .done(),
            "a build before setup= is read by its record"
        );
        assert!(
            !Reading {
                setup: Some("pending:fda".into()),
                record: completed
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
        };
        assert_eq!(stages.see(0, &running(1)), Waited::Moving);
        assert_eq!(stages.see(50_000, &running(2)), Waited::Moving);
        assert_eq!(stages.see(100_000, &running(2)), Waited::Moving);
        assert!(matches!(
            stages.see(110_000, &running(2)),
            Waited::Stalled { .. }
        ));
    }
}
