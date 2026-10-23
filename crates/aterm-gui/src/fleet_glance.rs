// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET GLANCE (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3, S10).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_types::needs::NeedWord;

/// How often the issuer (or a `[fleet] watch` Mac) reads the fleet.
pub(crate) const POLL_EVERY: Duration = Duration::from_secs(60);
/// On menu open, a read younger than this is fresh enough.
pub(crate) const MENU_FRESH: Duration = Duration::from_secs(15);
/// A fleet need counts on the badge only from a read this young.
pub(crate) const COUNT_FRESH: Duration = Duration::from_secs(300);
/// The most told fleet needs a seamless update carries (`SessionHandoff::fleet_told`).
pub(crate) const FLEET_TOLD_MAX: usize = 512;
/// One read's whole bound: the child's own deadline is 3 s a peer, so this is the kill, never a wait it is expected to reach.
pub(crate) const READ_CAP: Duration = Duration::from_secs(10);
/// The most fleet peers one snapshot keeps (`aterm_net`'s book is bounded
const MACS_MAX: usize = 256;

/// One need on a fleet peer, from its facts row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FleetNeed {
    /// The session's sid there.
    pub(crate) sid: String,
    /// Its published need.
    pub(crate) word: NeedWord,
    /// Who it is: the title a person gave it where the peer allows content, else its role, else its program.
    pub(crate) who: String,
    /// "worker of s-1e91": the task's `from=` where it names a sid.
    pub(crate) worker_of: Option<String>,
    /// How long ago the fact it stands on moved, as of the read.
    pub(crate) age_ms: Option<u64>,
    /// The need's own words where the peer serves content ([`content_what`]): an ask's text, a box's kind, a wall's kind and reset.
    pub(crate) what: Option<String>,
}

/// One session on a fleet peer that this Mac holds over the fleet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FleetHeld {
    /// The session's sid there.
    pub(crate) sid: String,
    /// Who it is, named as a need is ([`FleetNeed::who`]).
    pub(crate) who: String,
}

/// One manager on a fleet peer: a sid its live tasks' `from=` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FleetManager {
    /// Its label there (role, else program, else short sid).
    pub(crate) label: String,
    /// Its sid.
    pub(crate) sid: String,
    /// How many workers name it.
    pub(crate) workers: usize,
}

/// One fleet peer that answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FleetMac {
    /// How its rows name it: its book name's first label ([`short_name`]).
    pub(crate) name: String,
    /// What `aterm ctl survey --host` is given for it: the name the book knows it by ([`dial_of`]).
    pub(crate) dial: String,
    /// Its node id (its header's `node=`), when it sent one and proved it ([`Self::unproven`]'s is its claim, never kept).
    pub(crate) node: Option<String>,
    /// Where Screen Sharing and ssh go: the peer's own `host=` (a proven peer's only), else the host its book name names.
    pub(crate) host: Option<String>,
    /// Its rows carry titles and words (`[fleet] allow = "content"`).
    pub(crate) content: bool,
    /// It lets this Mac drive it (its header's `drive=1`, the step-in).
    pub(crate) drive: bool,
    /// Its rows publish no `needs=`: an older aterm, listed and never counted.
    pub(crate) older: bool,
    /// Answered without proving a node key this Mac binds (`"unproven":true`, [`BookFacts::unbound_issuer`]).
    pub(crate) unproven: bool,
    /// The sessions its reply holds rows for.
    pub(crate) sessions: usize,
    /// Of them, the agents.
    pub(crate) agents: usize,
    /// Of them, the agents at work.
    pub(crate) working: usize,
    /// Rows its reply cap left out (`sessions=` past every session row sent, exited ones included: the header counts them too).
    pub(crate) left_out: usize,
    /// Its needs.
    pub(crate) needs: Vec<FleetNeed>,
    /// Its sessions this Mac holds over the fleet, whatever their need: a hold is never a need, and stands until this Mac hands it back.
    pub(crate) held_here: Vec<FleetHeld>,
    /// Its managers.
    pub(crate) managers: Vec<FleetManager>,
    /// When ITS rows were read (unix ms): the snapshot's read for a Mac that answered it, an earlier one for a [`Self::carried`] Mac.
    pub(crate) read_at_ms: u64,
    /// It did not answer this read: its rows are its last answer's, kept while they still count ([`carry`]).
    pub(crate) carried: bool,
}

impl FleetMac {
    /// Who it is across reads: its node id, else the name the book knows it by — never the address that answered, which DHCP moves.
    pub(crate) fn ident(&self) -> &str {
        self.node.as_deref().unwrap_or(&self.dial)
    }

    /// Whether its needs are this Mac's to count and tell, and its sessions its to hold and hand back.
    pub(crate) fn counts(&self) -> bool {
        !self.older && !self.unproven
    }

    /// Whether its needs count on the badge at `now_ms`: its rows were read no longer than [`COUNT_FRESH`] ago.
    pub(crate) fn fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.read_at_ms) <= millis(COUNT_FRESH)
    }
}

/// One fleet peer that did not answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Unanswered {
    /// How rows name it ([`short_name`]).
    pub(crate) name: String,
    /// The name the book knows it by ([`dial_of`]).
    pub(crate) dial: String,
    /// Why, in the fleet client's words.
    pub(crate) why: String,
}

/// One fleet read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FleetSnapshot {
    /// When it was read (unix milliseconds).
    pub(crate) read_at_ms: u64,
    /// The peers that answered, in the read's order.
    pub(crate) macs: Vec<FleetMac>,
    /// The peers that did not, and why.
    pub(crate) unanswered: Vec<Unanswered>,
    /// Every peer failed at once with no route: a Mac's local-network privacy, until the person allows aterm.
    pub(crate) no_route: bool,
}

impl FleetSnapshot {
    /// Whether the read itself is no older than [`COUNT_FRESH`] at `now_ms` (each Mac's own rows: [`FleetMac::fresh`]).
    pub(crate) fn fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.read_at_ms) <= millis(COUNT_FRESH)
    }

    /// The Macs that answered this read (a [`FleetMac::carried`] one did not).
    pub(crate) fn answered(&self) -> usize {
        self.macs.iter().filter(|m| !m.carried).count()
    }

    /// The agents it read, on every Mac that answered it.
    pub(crate) fn agents(&self) -> usize {
        self.macs
            .iter()
            .filter(|m| !m.carried)
            .map(|m| m.agents)
            .sum()
    }

    /// The first instant after `now_ms` (unix ms) at which the read, or one Mac's rows in it, stops counting.
    pub(crate) fn next_lapse(&self, now_ms: u64) -> Option<u64> {
        std::iter::once(self.read_at_ms)
            .chain(self.macs.iter().map(|m| m.read_at_ms))
            .map(|at| at.saturating_add(millis(COUNT_FRESH)).saturating_add(1))
            .filter(|lapse| *lapse > now_ms)
            .min()
    }
}

/// A READ CARRIES A SILENT MAC'S LAST ROWS while they still count.
pub(crate) fn carry(
    previous: Option<&FleetSnapshot>,
    mut next: FleetSnapshot,
    now_ms: u64,
) -> FleetSnapshot {
    let Some(previous) = previous else {
        return next;
    };
    let room = MACS_MAX.saturating_sub(next.macs.len());
    let carried: Vec<FleetMac> = next
        .unanswered
        .iter()
        .filter(|silent| !next.macs.iter().any(|m| m.dial == silent.dial))
        .filter_map(|silent| {
            previous
                .macs
                .iter()
                .find(|m| m.dial == silent.dial && m.fresh(now_ms))
        })
        .map(|last| FleetMac {
            carried: true,
            ..last.clone()
        })
        .take(room)
        .collect();
    next.macs.extend(carried);
    next
}

/// A span in milliseconds.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `name` when it can stand in a `vnc://` URL or an `ssh` line: letters, digits, `.`, `-` and `_`, starting with a letter or digit.
pub(crate) fn host_word(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name.len() <= 253
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    ok.then(|| name.to_string())
}

/// THE NAME THE BOOK KNOWS A PEER BY, from its survey label.
pub(crate) fn dial_of(label: &str) -> &str {
    label.split_once(" at ").map_or(label, |(name, _)| name)
}

/// `dial`'s host: without a trailing `:port`; an address whole.
fn host_part(dial: &str) -> &str {
    if dial.parse::<std::net::IpAddr>().is_ok() {
        return dial;
    }
    match dial.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => dial,
    }
}

/// How a row names the peer the book knows as `dial`: its host's first label (`mac-17` for `mac-17.local`), an address whole.
pub(crate) fn short_name(dial: &str) -> String {
    let host = host_part(dial);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_string();
    }
    host.split('.')
        .next()
        .filter(|first| !first.is_empty())
        .unwrap_or(host)
        .to_string()
}

/// The `aterm` argv of a fleet row's read: `ctl survey --host <dial>`, and `--sid <sid>` for one session.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) fn fleet_read_args(dial: &str, sid: Option<&str>) -> Option<Vec<String>> {
    if dial.is_empty() || dial.starts_with('-') || dial.chars().any(char::is_whitespace) {
        return None;
    }
    let mut args: Vec<String> = ["ctl", "survey", "--host", dial]
        .map(str::to_string)
        .to_vec();
    if let Some(sid) = sid {
        if !sid.starts_with("s-") || sid.chars().any(char::is_whitespace) {
            return None;
        }
        args.extend(["--sid".to_string(), sid.to_string()]);
    }
    Some(args)
}

/// The argv after the program of a fleet need's Take Over (`on`) or Hand Back on the Mac the book knows as `dial`.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) fn fleet_hold_args(dial: &str, sid: &str, on: bool) -> Option<Vec<String>> {
    let plain = |word: &str| {
        !word.is_empty() && !word.starts_with('-') && !word.chars().any(char::is_whitespace)
    };
    if !plain(dial) || !sid.starts_with("s-") || !plain(sid) {
        return None;
    }
    Some(
        [
            "ctl",
            "--host",
            dial,
            "hold",
            sid,
            if on { "on" } else { "off" },
        ]
        .map(str::to_string)
        .to_vec(),
    )
}

/// A member of a JSON object as a string, `None` for `-`, empty or absent.
fn field<'a>(object: &'a aterm_json::Map, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(aterm_json::Value::as_str)
        .filter(|v| !v.is_empty() && *v != "-")
}

/// One fleet peer's answer, labelled `label`: its header and its session
fn mac_of(
    label: &str,
    instance: &aterm_json::Map,
    sessions: &[aterm_json::Value],
    own_node: Option<&str>,
    unproven: bool,
) -> FleetMac {
    let sent: Vec<&aterm_json::Map> = sessions
        .iter()
        .filter_map(aterm_json::Value::as_object)
        .collect();
    let rows: Vec<&aterm_json::Map> = sent
        .iter()
        .copied()
        .filter(|row| field(row, "state") != Some("exited"))
        .collect();
    let older = rows.iter().any(|row| !row.contains_key("needs"));
    let content = field(instance, "level") == Some("content");
    let label_of = |row: &aterm_json::Map| {
        field(row, "role")
            .or_else(|| field(row, "program"))
            .map_or_else(
                || crate::presence::short_sid(field(row, "sid").unwrap_or("")),
                str::to_string,
            )
    };
    let dial = dial_of(label);
    let proven = |key: &str| (!unproven).then(|| field(instance, key)).flatten();
    let mut mac = FleetMac {
        name: short_name(dial),
        dial: dial.to_string(),
        node: proven("node").map(str::to_ascii_lowercase),
        host: proven("host")
            .and_then(host_word)
            .or_else(|| host_word(host_part(dial))),
        content,
        drive: proven("drive") == Some("1"),
        older,
        unproven,
        sessions: rows.len(),
        ..FleetMac::default()
    };
    let claimed = field(instance, "sessions").and_then(|n| n.parse::<usize>().ok());
    mac.left_out = claimed.map_or(0, |n| n.saturating_sub(sent.len()));
    for row in &rows {
        let agent = field(row, "program_kind") == Some("agent");
        mac.agents += usize::from(agent);
        let word = field(row, "agent").unwrap_or("-");
        mac.working += usize::from(agent && word != "idle");
        if let Some(from) = field(row, "task_from").filter(|f| f.starts_with("s-")) {
            match mac.managers.iter_mut().find(|m| m.sid == from) {
                Some(manager) => manager.workers += 1,
                None => {
                    let label = rows
                        .iter()
                        .find(|r| field(r, "sid") == Some(from))
                        .map_or_else(|| crate::presence::short_sid(from), |r| label_of(r));
                    mac.managers.push(FleetManager {
                        label,
                        sid: from.to_string(),
                        workers: 1,
                    });
                }
            }
        }
        if older {
            continue;
        }
        let Some(sid) = field(row, "sid") else {
            continue;
        };
        let content_field = |key: &str| content.then(|| field(row, key)).flatten();
        let who = crate::status_item::tab_name(
            field(row, "role"),
            content_field("user_title"),
            content_field("title").unwrap_or(""),
        )
        .unwrap_or_else(|| crate::needs::fold_clip(&label_of(row), 40));
        let held_from = field(row, "held_from").map(str::to_ascii_lowercase);
        if own_node.is_some() && held_from.as_deref() == own_node {
            mac.held_here.push(FleetHeld {
                sid: sid.to_string(),
                who: who.clone(),
            });
        }
        let Some(need) = field(row, "needs").and_then(NeedWord::parse) else {
            continue;
        };
        let age_key = match need {
            NeedWord::Stalled => Some("task_ms"),
            NeedWord::Ask => None,
            NeedWord::Frozen | NeedWord::Stopped if !agent => None,
            _ => Some("agent_ms"),
        };
        mac.needs.push(FleetNeed {
            sid: sid.to_string(),
            word: need,
            who,
            worker_of: field(row, "task_from")
                .filter(|f| f.starts_with("s-"))
                .map(crate::presence::short_sid),
            age_ms: age_key
                .and_then(|key| field(row, key))
                .and_then(|ms| ms.parse().ok()),
            what: content.then(|| content_what(need, row)).flatten(),
        });
    }
    mac
}

/// A content peer's words for its row's need `word`, as a local row has
fn content_what(word: NeedWord, row: &aterm_json::Map) -> Option<String> {
    use crate::needs::{HarnessAsk, ROW_WHAT_MAX, fold_clip, need_words};
    match word {
        NeedWord::Ask => field(row, "attention")
            .map(|text| text.trim().trim_start_matches('\u{26a0}').trim_start())
            .map(|text| {
                need_words(text).map_or_else(
                    || fold_clip(text, ROW_WHAT_MAX),
                    |words| HarnessAsk::of(words).line(ROW_WHAT_MAX),
                )
            })
            .filter(|what| !what.is_empty()),
        NeedWord::Approval | NeedWord::Question => {
            let detail = field(row, "agent_detail");
            Some(crate::presence::prompt_band_word(detail))
        }
        NeedWord::Login | NeedWord::Memory | NeedWord::Wall => {
            let kind = field(row, "agent")?.strip_prefix("wall:")?;
            Some(crate::needs::wall_what(kind, field(row, "agent_detail")))
        }
        NeedWord::Frozen | NeedWord::Stopped | NeedWord::Stalled => None,
    }
}

/// THE FLEET READ, PARSED.
pub(crate) fn parse(
    json: &str,
    no_route: bool,
    own_node: Option<&str>,
    now_ms: u64,
) -> Result<FleetSnapshot, String> {
    let value: aterm_json::Value =
        aterm_json::from_str(json).map_err(|e| format!("the fleet read is no survey: {e}"))?;
    let hosts = value
        .as_array()
        .ok_or_else(|| "the fleet read is no list".to_string())?;
    let mut snapshot = FleetSnapshot {
        read_at_ms: now_ms,
        no_route,
        ..FleetSnapshot::default()
    };
    for host in hosts {
        let Some(host) = host.as_object() else {
            continue;
        };
        if host.get("fleet").and_then(aterm_json::Value::as_bool) != Some(true) {
            continue;
        }
        let label = field(host, "label").unwrap_or("a peer").to_string();
        let unanswered = |why: &str| Unanswered {
            name: short_name(dial_of(&label)),
            dial: dial_of(&label).to_string(),
            why: why.to_string(),
        };
        if let Some(why) = field(host, "error") {
            snapshot.unanswered.push(unanswered(why));
            continue;
        }
        let Some(instance) = host.get("instance").and_then(aterm_json::Value::as_object) else {
            snapshot
                .unanswered
                .push(unanswered("its answer had no header"));
            continue;
        };
        let unproven = host.get("unproven").and_then(aterm_json::Value::as_bool) == Some(true);
        if own_node.is_some() && !unproven && field(instance, "node") == own_node {
            continue;
        }
        if snapshot.macs.len() >= MACS_MAX {
            snapshot
                .unanswered
                .push(unanswered("past the peers one read keeps"));
            continue;
        }
        let sessions = host
            .get("sessions")
            .and_then(aterm_json::Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        snapshot.macs.push(FleetMac {
            read_at_ms: now_ms,
            ..mac_of(&label, instance, sessions, own_node, unproven)
        });
    }
    Ok(snapshot)
}

/// What this Mac's fleet files say of it, read off the event loop ([`read_book`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BookFacts {
    /// It holds a fleet key: it is in a fleet.
    pub(crate) in_fleet: bool,
    /// Its book has peers that joined it: it is the fleet's issuer, and its read is the whole fleet.
    pub(crate) issuer: bool,
    /// The issuer a member's book names, where it is all the book holds and no copy of that issuer's book was read.
    pub(crate) issuer_label: Option<String>,
    /// The issuer a member's book names with no key bound to it (it joined before node keys came, or its issuer proved none then).
    pub(crate) unbound_issuer: Option<String>,
}

/// WHETHER THIS MAC IS IN A FLEET, the one answer every surface reads (the fleet glance's book here, the Agents rows' and Summarize's `summarize::in_fleet`).
pub(crate) fn holds_fleet_key(state: &Path) -> bool {
    std::fs::symlink_metadata(state.join(aterm_net::fleet::KEY_FILE)).is_ok()
}

/// [`BookFacts`] of the state directory `state` (`fleet.key` and the book of peers).
pub(crate) fn read_book(state: &Path) -> BookFacts {
    let in_fleet = holds_fleet_key(state);
    if !in_fleet {
        return BookFacts::default();
    }
    read_book_peers(state, in_fleet)
}

/// [`read_book`] off unix, where there is no book to read
#[cfg(not(unix))]
fn read_book_peers(_state: &Path, in_fleet: bool) -> BookFacts {
    BookFacts {
        in_fleet,
        ..BookFacts::default()
    }
}

/// [`read_book`]'s book: whether it names peers that joined this Mac, or
#[cfg(unix)]
fn read_book_peers(state: &Path, in_fleet: bool) -> BookFacts {
    let book = aterm_net::fleet_member::read_peers(state).unwrap_or_default();
    let issuer = book
        .iter()
        .any(|p| p.via == aterm_net::fleet_member::Via::Joined);
    let issuer_label = book
        .iter()
        .all(|p| p.via == aterm_net::fleet_member::Via::Issuer)
        .then(|| book.first())
        .flatten()
        .filter(|first| {
            !first
                .node
                .as_deref()
                .is_some_and(|node| aterm_net::fleet_member::holds_issuer_book(state, node))
        })
        .map(|first| first.host.clone());
    let unbound_issuer = book
        .iter()
        .find(|p| p.via == aterm_net::fleet_member::Via::Issuer)
        .filter(|p| p.key.is_none())
        .map(|p| p.host.clone());
    BookFacts {
        in_fleet,
        issuer,
        issuer_label,
        unbound_issuer,
    }
}

/// WHEN TO READ THE FLEET: never in no fleet.
pub(crate) fn should_poll(
    book: &BookFacts,
    watch: bool,
    last_ms: Option<u64>,
    now_ms: u64,
    menu_opening: bool,
) -> bool {
    if !book.in_fleet {
        return false;
    }
    let older_than =
        |span: Duration| last_ms.is_none_or(|at| now_ms.saturating_sub(at) >= millis(span));
    (menu_opening && older_than(MENU_FRESH)) || ((book.issuer || watch) && older_than(POLL_EVERY))
}

/// The shared slot the reader writes and the status item reads.
#[derive(Clone, Debug, Default)]
pub(crate) struct FleetSlot {
    /// Moves with every snapshot written: a Fleet submenu click names it.
    pub(crate) generation: u16,
    /// The last good read.
    pub(crate) snapshot: Option<Arc<FleetSnapshot>>,
    /// The read before it, and its generation: a click on a submenu painted from it still resolves.
    pub(crate) previous: Option<(u16, Arc<FleetSnapshot>)>,
    /// Why the last read failed, when it did (cleared by a good one).
    pub(crate) error: Option<String>,
    /// A read is in flight.
    pub(crate) reading: bool,
    /// What the book said at the last look.
    pub(crate) book: BookFacts,
    /// When the last read was tried (unix ms), good or not.
    pub(crate) tried_ms: Option<u64>,
}

impl FleetSlot {
    /// The snapshot a click with `generation` was painted from.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn snapshot_at(&self, generation: u16) -> Option<&Arc<FleetSnapshot>> {
        if self.generation == generation {
            return self.snapshot.as_ref();
        }
        self.previous
            .as_ref()
            .filter(|(g, _)| *g == generation)
            .map(|(_, s)| s)
    }

    /// `chrome`'s `fleet=` word.
    pub(crate) fn word(&self) -> String {
        if !self.book.in_fleet {
            "off".to_string()
        } else if self.reading {
            "reading".to_string()
        } else if self.error.is_some() {
            "failed".to_string()
        } else {
            self.snapshot.as_ref().map_or_else(
                || "unread".to_string(),
                |s| format!("as_of:{}", s.read_at_ms),
            )
        }
    }
}

/// What a read answered: the child's JSON and stderr, or why there is none.
pub(crate) type ReadAnswer = Result<(String, String), String>;

/// The read itself, injected so a test drives the thread with no child.
pub(crate) trait FleetRunner: Send {
    /// Read the fleet once.
    fn run(&mut self) -> ReadAnswer;
}

/// THE REAL READ: `<program> <args>` as a child, SIGKILLed at `cap`.
pub(crate) struct ChildRunner {
    /// The `aterm` binary (`ledger_key::aterm_cli`).
    pub(crate) program: PathBuf,
    /// `ctl survey --fleet --json --timeout 3`.
    pub(crate) args: Vec<String>,
    /// The kill.
    pub(crate) cap: Duration,
}

impl ChildRunner {
    /// The fleet read the status item makes.
    pub(crate) fn fleet() -> Self {
        Self {
            program: crate::ledger_key::aterm_cli(),
            args: ["ctl", "survey", "--fleet", "--json", "--timeout", "3"]
                .map(str::to_string)
                .to_vec(),
            cap: READ_CAP,
        }
    }
}

impl FleetRunner for ChildRunner {
    fn run(&mut self) -> ReadAnswer {
        let ran = crate::bounded_child::run_bounded(&self.program, &self.args, Some(self.cap))?;
        if ran.stdout.trim().is_empty() {
            let why = ran.stderr.lines().next().unwrap_or("no answer").to_string();
            return Err(crate::needs::fold_clip(&why, 120));
        }
        Ok((ran.stdout, ran.stderr))
    }
}

/// What the reader thread is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kick {
    /// The status menu is opening: read if the last read is stale.
    MenuOpening,
    /// Read the Fleet Now: read whatever the last read's age.
    Now,
}

/// The reader thread's inputs besides its runner.
pub(crate) struct ReaderSeams {
    /// The book's facts at each look.
    pub(crate) book: Box<dyn Fn() -> BookFacts + Send>,
    /// `[fleet] watch`, current.
    pub(crate) watch: Arc<std::sync::atomic::AtomicBool>,
    /// This machine's node id, to drop its own answer and to know the holds its drives placed ([`FleetMac::held_here`]).
    pub(crate) own_node: Option<String>,
    /// Now, unix ms.
    pub(crate) now_ms: Box<dyn Fn() -> u64 + Send>,
    /// Told after every write to the slot (`Wake::FleetRead`); `false` when nobody listens any more, which ends the thread.
    pub(crate) post: Box<dyn Fn() -> bool + Send>,
    /// How long the thread waits for a kick before it looks again.
    pub(crate) tick: Duration,
}

/// The fleet glance's handle: the slot and the kick channel.
pub(crate) struct FleetGlance {
    /// The shared slot.
    pub(crate) slot: Arc<Mutex<FleetSlot>>,
    /// The reader's kicks.
    kick: Sender<Kick>,
    /// `[fleet] watch`, which the App keeps current.
    watch: Arc<std::sync::atomic::AtomicBool>,
}

impl FleetGlance {
    /// Ask the reader for `kick` (a no-op once the thread is gone).
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn kick(&self, kick: Kick) {
        let _ = self.kick.send(kick);
    }

    /// Follow `[fleet] watch`.
    pub(crate) fn set_watch(&self, on: bool) {
        self.watch.store(on, std::sync::atomic::Ordering::Release);
    }

    /// The slot as it stands, copied out under its lock (poison recovered: the slot is plain data, its snapshots shared, so a copy is cheap).
    pub(crate) fn copy(&self) -> FleetSlot {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// START THE READER on thread `aterm-fleet-glance` with `runner` and `seams`, its watch switch starting at `watch`.
pub(crate) fn spawn(runner: Box<dyn FleetRunner>, seams: ReaderSeams) -> Option<FleetGlance> {
    let slot = Arc::new(Mutex::new(FleetSlot::default()));
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = Arc::clone(&seams.watch);
    let thread_slot = Arc::clone(&slot);
    std::thread::Builder::new()
        .name("aterm-fleet-glance".to_string())
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            reader_loop(runner, &seams, &thread_slot, &rx);
        })
        .ok()?;
    Some(FleetGlance {
        slot,
        kick: tx,
        watch,
    })
}

/// The reader: look, read when [`should_poll`] (or a [`Kick::Now`]) says,
fn reader_loop(
    mut runner: Box<dyn FleetRunner>,
    seams: &ReaderSeams,
    slot: &Arc<Mutex<FleetSlot>>,
    rx: &Receiver<Kick>,
) {
    let lock = || slot.lock().unwrap_or_else(|p| p.into_inner());
    let mut kick: Option<Kick> = None;
    let mut first = true;
    let mut lapse_at: Option<u64> = None;
    let mut told_ms: u64 = 0;
    loop {
        let book = (seams.book)();
        let now = (seams.now_ms)();
        let tried = lock().tried_ms;
        let watch = seams.watch.load(std::sync::atomic::Ordering::Acquire);
        let read = book.in_fleet
            && (kick == Some(Kick::Now)
                || should_poll(&book, watch, tried, now, kick == Some(Kick::MenuOpening)));
        let book_moved = {
            let mut slot = lock();
            let moved = slot.book != book;
            slot.book = book;
            slot.reading = read;
            moved
        };
        let lapsed = lapse_at.is_some_and(|at| now >= at);
        let first_look = std::mem::take(&mut first);
        if read || book_moved || lapsed || first_look {
            if !(seams.post)() {
                return;
            }
            told_ms = now;
        }
        if read {
            let answer = runner.run();
            let now = (seams.now_ms)();
            let parsed = answer.and_then(|(json, stderr)| {
                let no_route = stderr.contains(aterm_ctl::NO_ROUTE_NOTE);
                parse(&json, no_route, seams.own_node.as_deref(), now)
            });
            {
                let mut slot = lock();
                slot.reading = false;
                slot.tried_ms = Some(now);
                match parsed {
                    Ok(snapshot) => {
                        let snapshot = carry(slot.snapshot.as_deref(), snapshot, now);
                        if let Some(old) = slot.snapshot.take() {
                            slot.previous = Some((slot.generation, old));
                        }
                        slot.generation = slot.generation.wrapping_add(1);
                        slot.snapshot = Some(Arc::new(snapshot));
                        slot.error = None;
                    }
                    Err(why) => slot.error = Some(why),
                }
            }
            lock().book = (seams.book)();
            while rx.try_recv().is_ok() {}
            if !(seams.post)() {
                return;
            }
            told_ms = now;
        }
        let now = (seams.now_ms)();
        lapse_at = lock()
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.next_lapse(told_ms));
        let wait = lapse_at.map_or(seams.tick, |at| {
            Duration::from_millis(at.saturating_sub(now)).min(seams.tick)
        });
        kick = match rx.recv_timeout(wait) {
            Ok(kick) => Some(kick),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

/// The herald's key for a fleet need.
pub(crate) fn herald_key(mac: &str, sid: &str) -> u64 {
    aterm_hash::fnv1a_64(format!("{mac}\u{1f}{sid}").as_bytes()) | (1 << 63)
}

/// Whether `key` is a fleet need's ([`herald_key`]).
pub(crate) fn is_herald_key(key: u64) -> bool {
    key & (1 << 63) != 0
}

/// A fleet need's transition key within its slot: its word's hash — a facts
fn need_key(word: NeedWord) -> u64 {
    aterm_hash::fnv1a_64(word.word().as_bytes())
}

/// A fleet need as the herald reads it.
pub(crate) fn escalation(mac: &FleetMac, need: &FleetNeed) -> crate::status_item::Escalation {
    let what = need
        .what
        .as_deref()
        .unwrap_or_else(|| crate::status_item::need_words(need.word));
    crate::status_item::Escalation {
        session: herald_key(mac.ident(), &need.sid),
        kind: crate::status_item::EscalationKind::of(need.word),
        word: need.word,
        key: need_key(need.word),
        label: format!("{} \u{b7} {} \u{2014} {what}", mac.name, need.who),
        body: crate::needs::fold_clip(&format!("{} \u{b7} {}: {what}", mac.name, need.who), 120),
        headline: crate::status_item::headline(need.word),
    }
}

/// One told fleet need as the park writes it (`SessionHandoff::fleet_told`): its herald key in hex and its word.
#[cfg(any(unix, test))]
pub(crate) fn told_row(key: u64, word: NeedWord) -> String {
    format!("{key:016x}={}", word.word())
}

/// The told fleet needs a seamless update carried ([`told_row`]'s rows), by herald key; a row that does not read.
pub(crate) fn parse_told(rows: &[String]) -> std::collections::HashMap<u64, NeedWord> {
    rows.iter()
        .filter_map(|row| {
            let (key, word) = row.split_once('=')?;
            let key = u64::from_str_radix(key, 16)
                .ok()
                .filter(|k| is_herald_key(*k))?;
            Some((key, NeedWord::parse(word)?))
        })
        .take(FLEET_TOLD_MAX)
        .collect()
}

/// THE FLEET'S DIGEST ENTRY (`fleet.unreadable.no-route`, the whole answer of the level `Family::FleetUnreadable`).
pub(crate) fn no_route_entry() -> aterm_messages::exceptions::Entry {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Resurface, Then, Why};
    Entry::new(
        Family::FleetUnreadable,
        "",
        "no-route",
        "No route to any fleet Mac",
        Why::OnlyYou(Reason::new(
            "every peer refused at once with no route: local-network privacy",
        )),
        Then::step("Allow aterm in System Settings ▸ Privacy & Security ▸ Local Network"),
    )
    .severity(aterm_messages::Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line("fleet unreadable no-route")
}

/// The Local Network privacy pane's URL.
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) const LOCAL_NETWORK_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork";

impl crate::App {
    /// START THE FLEET GLANCE with the status item (the first OS window's attach).
    pub(crate) fn start_fleet_glance(&mut self) {
        if !self.fleet_glance_may_start() {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let state = aterm_types::dirs::state_dir();
        #[cfg(unix)]
        let own_node = state
            .as_deref()
            .and_then(|dir| aterm_net::fleet_member::read_node_id(dir).ok().flatten());
        #[cfg(not(unix))]
        let own_node = None;
        let watch = Arc::new(std::sync::atomic::AtomicBool::new(
            self.config
                .fleet
                .as_ref()
                .is_some_and(crate::app_config::FleetConfig::watch_or_default),
        ));
        let seams = ReaderSeams {
            book: Box::new(move || state.as_deref().map(read_book).unwrap_or_default()),
            watch,
            own_node,
            now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
            post: Box::new(move || proxy.send_event(crate::Wake::FleetRead).is_ok()),
            tick: POLL_EVERY,
        };
        self.start_fleet_glance_with(Box::new(ChildRunner::fleet()), seams);
    }

    /// Whether a fleet glance may start here: never headless (no child, no
    fn fleet_glance_may_start(&self) -> bool {
        !self.headless && cfg!(target_os = "macos") && self.fleet_glance.is_none()
    }

    /// Start the reader with `runner` and `seams` where [`Self::fleet_glance_may_start`] allows, and hand its slot to the status item for the Fleet submenu.
    pub(crate) fn start_fleet_glance_with(
        &mut self,
        runner: Box<dyn FleetRunner>,
        seams: ReaderSeams,
    ) {
        if !self.fleet_glance_may_start() {
            return;
        }
        self.fleet_glance = spawn(runner, seams);
        #[cfg(target_os = "macos")]
        if let (Some(handle), Some(fleet)) = (&self._status_item, &self.fleet_glance) {
            crate::status_item::attach_fleet(handle, Arc::clone(&fleet.slot));
        }
    }

    /// `[fleet] watch` moved (a config commit): the reader follows it.
    pub(crate) fn follow_fleet_watch(&self) {
        if let Some(fleet) = &self.fleet_glance {
            fleet.set_watch(
                self.config
                    .fleet
                    .as_ref()
                    .is_some_and(crate::app_config::FleetConfig::watch_or_default),
            );
        }
    }

    /// The `Wake::FleetRead` arm: the fleet glance wrote its slot.
    pub(crate) fn on_fleet_read(&mut self) {
        self.mark_status_dirty();
        let no_route = self.fleet_glance.as_ref().is_some_and(|f| {
            let slot = f.copy();
            slot.book.in_fleet && slot.snapshot.as_ref().is_some_and(|s| s.no_route)
        });
        let entries = if no_route {
            vec![no_route_entry()]
        } else {
            Vec::new()
        };
        self.except_replace(
            aterm_messages::exceptions::Family::FleetUnreadable,
            &entries,
        );
        self.herald_fleet();
    }

    /// THE FLEET'S NOTICES: one per fleet need's transition, through the herald's rate limits, from rows no older than [`COUNT_FRESH`] only.
    pub(crate) fn herald_fleet(&mut self) {
        if self.headless {
            return;
        }
        let snapshot = self.fleet_glance.as_ref().and_then(|f| {
            let slot = f.copy();
            slot.book.in_fleet.then(|| slot.snapshot.clone()).flatten()
        });
        let now_ms = aterm_time::unix_millis().unwrap_or(0);
        let Some(snap) = snapshot.filter(|s| s.fresh(now_ms)) else {
            let keys: Vec<u64> = self.fleet_heralded.keys().copied().collect();
            for key in keys {
                self.presence.herald.forgo(key);
            }
            return;
        };
        let mut current: Vec<(crate::status_item::Escalation, String, NeedWord)> = Vec::new();
        for mac in snap.macs.iter().filter(|m| m.counts() && m.fresh(now_ms)) {
            for need in &mac.needs {
                current.push((escalation(mac, need), mac.dial.clone(), need.word));
            }
        }
        let answered: std::collections::HashSet<&str> = snap
            .macs
            .iter()
            .filter(|m| !m.carried)
            .map(|m| m.dial.as_str())
            .collect();
        let silent: std::collections::HashSet<&str> =
            snap.unanswered.iter().map(|u| u.dial.as_str()).collect();
        let mut gone: Vec<u64> = Vec::new();
        let mut quiet: Vec<u64> = Vec::new();
        for (key, (dial, _)) in &self.fleet_heralded {
            if current.iter().any(|(e, _, _)| e.session == *key) {
                continue;
            }
            if silent.contains(dial.as_str()) && !answered.contains(dial.as_str()) {
                quiet.push(*key);
            } else {
                gone.push(*key);
            }
        }
        for key in gone {
            self.herald_deliver(key, None, false);
            self.presence.herald.retire(key);
            self.fleet_heralded.remove(&key);
        }
        for key in quiet {
            self.presence.herald.forgo(key);
        }
        if let Some(told) = self.fleet_adopt.take() {
            let now_unix = i64::try_from(now_ms / 1000).unwrap_or(i64::MAX);
            for (esc, _, word) in &current {
                if told.get(&esc.session) != Some(word) {
                    continue;
                }
                self.presence.herald.adopt(
                    esc.session,
                    crate::status_item::StatusCarry {
                        need: Some((*word, now_unix)),
                        told: true,
                        ..crate::status_item::StatusCarry::default()
                    },
                    now_unix,
                    std::time::Instant::now(),
                );
            }
        }
        for (esc, dial, word) in &current {
            self.fleet_heralded
                .insert(esc.session, (dial.clone(), *word));
            self.herald_deliver(esc.session, Some(esc), false);
        }
    }

    /// THE FLEET NEEDS THIS HERALD HAS TOLD, as a seamless update's park stamps them (`SessionHandoff::fleet_told`).
    #[cfg(unix)]
    pub(crate) fn fleet_told_rows(&self) -> Vec<String> {
        let mut rows: Vec<String> = self
            .fleet_heralded
            .iter()
            .filter(|(key, (_, word))| {
                self.presence.herald.told(
                    **key,
                    crate::status_item::EscalationKind::of(*word),
                    need_key(*word),
                )
            })
            .map(|(key, (_, word))| told_row(*key, *word))
            .collect();
        rows.sort();
        rows.truncate(FLEET_TOLD_MAX);
        rows
    }

    /// A fleet row's act on the Mac the book knows as `dial`, shown as `name` (and its session `sid`).
    #[cfg(target_os = "macos")]
    pub(crate) fn fleet_act(
        &mut self,
        action: crate::status_item::StatusAction,
        dial: &str,
        name: &str,
        host: Option<&str>,
        sid: Option<&str>,
    ) {
        use crate::platform::AppRt as _;
        use crate::status_item::StatusAction;
        match action {
            StatusAction::PeerRead(_) | StatusAction::MacRead(_) => {
                let Some(args) = fleet_read_args(dial, sid) else {
                    aterm_log::info!("status item: no read for a fleet row with no name to dial");
                    return;
                };
                let title =
                    sid.map_or_else(|| name.to_string(), |sid| format!("{name} \u{b7} {sid}"));
                self.run_status_read(crate::app_take_over::CliPlan::peer(args, title));
            }
            StatusAction::PeerScreen(_) | StatusAction::MacScreen(_) => {
                if let Some(host) = host.and_then(host_word) {
                    // {host}"));
                    let _ = self.apprt.open_url(&format!("vnc://{host}"));
                }
            }
            StatusAction::PeerSsh(_) | StatusAction::MacSsh(_) => {
                if let Some(host) = host.and_then(host_word) {
                    let _ = self.apprt.pasteboard_set(&format!("ssh {host}"));
                }
            }
            StatusAction::PeerTakeOver(_) | StatusAction::PeerHandBack(_) => {
                let on = matches!(action, StatusAction::PeerTakeOver(_));
                let Some(args) = sid.and_then(|sid| fleet_hold_args(dial, sid, on)) else {
                    aterm_log::info!(
                        "status item: no step-in for a fleet row with no name to dial"
                    );
                    return;
                };
                let title = format!(
                    "{} on {name} \u{b7} {}",
                    if on { "Take Over" } else { "Hand Back" },
                    sid.unwrap_or("")
                );
                self.run_status_read(crate::app_take_over::CliPlan::driven(args, title));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "fleet_glance_tests.rs"]
mod tests;
