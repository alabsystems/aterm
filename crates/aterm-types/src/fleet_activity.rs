// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet activity: the agent requests that crossed between this Mac and the others of its fleet,
//! as one row each, and what a window of them adds up to.
//!
//! An agent request is something a session asked for across Macs: a read of another Mac's
//! sessions, typing into one, a new job, a message, a hold or hand back, a close, a claim. What
//! aterm does of its own with no session behind it (the fleet glance's survey, the board keeper,
//! heartbeats, the vault, service-repair and accounts-renew services, the mesh's streams) is not
//! one and has no row: [`Plain::of`] names no type for a service's kind, and [`counted`] takes a
//! read only where a session asked for it.
//!
//! A [`Row`] holds when, which way, the other Mac's node and name, the [`Plain`] type, whether
//! this side's or that side's fleet access admitted it, and the session that asked where the
//! request named one. It holds no message text, prompt, command line or secret. The rows live in
//! one bounded file of the state folder (`aterm_ctl::fleet_activity` writes and reads it);
//! [`summarize`] is what the menu-bar item's menu and `aterm fleet activity` both show.
//!
//! A read shows one minute's admitted reads of one Mac as one counted row ([`folded`]). The file
//! keeps a row for every request until it outgrows its size; then it keeps each minute's alike
//! requests as one counted row, a refusal's with the session that asked, so no count is lost
//! and one Mac's repeats cannot push the others' rows out.

use crate::fleet_access::{DriveKind, FleetAccess};

/// The record's folder in the state folder.
pub const DIR: &str = "fleet-activity";

/// The rows' file in [`DIR`].
pub const ROWS: &str = "rows";

/// The window a summary covers where none is named: the last hour.
pub const WINDOW_MS: u64 = 3_600_000;

/// The span requests of one Mac, one way and one type are folded into one row over: a minute
/// of the clock ([`Row::alike`]).
pub const FOLD_MS: u64 = 60_000;

/// How far past the clock a row's stamp may stand and still be taken as written: rows are
/// stamped before their writer has the record's lock, and two clocks of one Mac agree to well
/// within it. A stamp further ahead was written before the clock was set back
/// ([`settled`]).
pub const AHEAD_MS: u64 = 60_000;

/// The most pairs a menu has rows for ([`Summary::shown`]).
pub const SHOWN_PAIRS: usize = 5;

/// The most admitted reads a pair holds and is still routine ([`Pair::routine`]).
pub const ROUTINE_READS: u64 = 5;

/// Which way a request went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Way {
    /// Another Mac asked this one.
    In,
    /// This Mac asked another.
    Out,
}

impl Way {
    /// Both, incoming first.
    pub const ALL: [Self; 2] = [Self::In, Self::Out];

    /// The way's word in a row and on the command line.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }

    /// The way [`Self::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|way| way.word() == word)
    }

    /// The arrow a person reads it by: `←` into this Mac, `→` out of it.
    #[must_use]
    pub const fn arrow(self) -> &'static str {
        match self {
            Self::In => "\u{2190}",
            Self::Out => "\u{2192}",
        }
    }
}

/// What a request was, in the few plain types a person is shown, lightest first: a tie between
/// two types of one pair goes to the later ([`Pair::dominant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Plain {
    /// A read of the other Mac's sessions: a survey, a status, text, a result, a wait.
    Read,
    /// Mail to a session, or marking a reply read.
    Message,
    /// Typing into a session: a turn, a key, text.
    Typed,
    /// Taking a session over or handing it back, or claiming a folder for a request.
    Hold,
    /// Closing a session.
    Close,
    /// Starting a worker or a shell: a new tab.
    NewJob,
}

impl Plain {
    /// Every type, lightest first.
    pub const ALL: [Self; 6] = [
        Self::Read,
        Self::Message,
        Self::Typed,
        Self::Hold,
        Self::Close,
        Self::NewJob,
    ];

    /// The type's word in a row and in `--json`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Message => "message",
            Self::Typed => "typed",
            Self::Hold => "hold",
            Self::Close => "close",
            Self::NewJob => "new-job",
        }
    }

    /// The type [`Self::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|plain| plain.word() == word)
    }

    /// The type as a person reads `n` of them: `read`, `reads`, `typing`, `new jobs`.
    #[must_use]
    pub const fn named(self, n: u64) -> &'static str {
        match (self, n == 1) {
            (Self::Read, true) => "read",
            (Self::Read, false) => "reads",
            (Self::Message, true) => "message",
            (Self::Message, false) => "messages",
            (Self::Typed, _) => "typing",
            (Self::Hold, true) => "hold",
            (Self::Hold, false) => "holds",
            (Self::Close, true) => "close",
            (Self::Close, false) => "closes",
            (Self::NewJob, true) => "new job",
            (Self::NewJob, false) => "new jobs",
        }
    }

    /// The plain type of an act of `kind`, or `None` for a kind no session asks for: the three
    /// fleet services and the mesh's stream are aterm's own traffic. Every kind is named: a new
    /// one does not compile until it is given a type or left out here.
    #[must_use]
    pub const fn of(kind: DriveKind) -> Option<Self> {
        match kind {
            DriveKind::Read | DriveKind::ShellRead => Some(Self::Read),
            DriveKind::Vault
            | DriveKind::ServiceRepair
            | DriveKind::AccountsRenew
            | DriveKind::BusLog => None,
            DriveKind::Post | DriveKind::MailSeen => Some(Self::Message),
            DriveKind::Turn | DriveKind::Key | DriveKind::Send => Some(Self::Typed),
            // A claim takes a folder for a request as a hold takes a session: neither starts
            // anything, and both need Read and Write.
            DriveKind::Hold | DriveKind::Claim => Some(Self::Hold),
            DriveKind::Close => Some(Self::Close),
            DriveKind::Spawn => Some(Self::NewJob),
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// The plain type a request of `kind` is recorded under, `by` the session that asked where the
/// request named one; `None` where it is not an agent request. A read no session asked for is
/// aterm's own poll (the board keeper's wait, a service's status read): only a session's read
/// counts. Anything else a Mac does to another's sessions counts whoever asked.
#[must_use]
pub fn counted(kind: DriveKind, by: Option<&str>) -> Option<Plain> {
    let plain = Plain::of(kind)?;
    (plain != Plain::Read || by.is_some()).then_some(plain)
}

/// The verbs of the three fleet services (the vault's, service-repair's and accounts-renew's):
/// their lines are their own clients', never a session's, and the record has no row for one on
/// either Mac, whatever kind the lock judges it by.
pub const SERVICE_VERBS: [&str; 3] = ["vault", "service-repair", "accounts-renew"];

/// Whether `verb` is a fleet service's ([`SERVICE_VERBS`]).
#[must_use]
pub fn is_service(verb: &str) -> bool {
    SERVICE_VERBS.contains(&verb)
}

/// THE ONE TABLE OF WHAT A SESSION'S DRIVE LINE IS: the kind of a request by whether an
/// `@<sid>` leads it (`selected`), its `verb`, and its arguments (`rest`). Update reads require
/// the exact `status` or `holds` form; other verbs use their first argument. The driven
/// Mac's gate judges a line by this table and the Mac that sent it records it by the same one,
/// so a request is the same type on both records. `None` for a fleet service's line
/// ([`is_service`]: the gate parses those itself, and the record has no row for one) and for a
/// verb no drive serves. The gate refuses more than this names (a `board` with arguments, an
/// `await` of anything but tasks, a selector on `hold`): a line it refuses does nothing and has
/// no row, whatever kind it would have had.
#[must_use]
pub fn kind_of_request(selected: bool, verb: &str, rest: &str) -> Option<DriveKind> {
    let first = rest.split_whitespace().next();
    Some(if selected {
        match verb {
            "status" | "text" | "screen" | "result" => DriveKind::Read,
            "inbox" if first == Some("seen") => DriveKind::MailSeen,
            "inbox" => DriveKind::Read,
            "blocks" | "blocktext" | "await" => DriveKind::ShellRead,
            "post" => DriveKind::Post,
            "turn" => DriveKind::Turn,
            "key" => DriveKind::Key,
            "send" => DriveKind::Send,
            "close" => DriveKind::Close,
            _ => return None,
        }
    } else {
        match verb {
            "survey" | "version" | "setup" | "board" | "requests" | "await" => DriveKind::Read,
            "update" if matches!(rest.trim(), "status" | "holds") => DriveKind::Read,
            "hold" => DriveKind::Hold,
            "spawn" => DriveKind::Spawn,
            "request" if first == Some("claim") => DriveKind::Claim,
            "request" if first == Some("show") => DriveKind::Read,
            _ => return None,
        }
    })
}

/// [`kind_of_request`] of a drive's whole request `line`: its `@<sid>` where it leads with one,
/// its verb, and the rest.
#[must_use]
pub fn kind_of_line(line: &str) -> Option<DriveKind> {
    let line = line.trim();
    let (selected, line) = match line.strip_prefix('@') {
        Some(after) => (true, after.split_once(char::is_whitespace)?.1.trim_start()),
        None => (false, line),
    };
    let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    kind_of_request(selected, verb, rest)
}

/// The word of a fleet `survey` line that names the session that asked for it: `by=<sid>`.
///
/// A drive names its requester on its first line; a survey connection has no such line, so a
/// survey a session asked for carries this word, and one without it is aterm's own poll (the
/// fleet glance's read, a placement's, a check's). An aterm from before the word answers a line
/// that carries it with its usage; the asker then asks again without it ([`survey_unmarked`]),
/// and that Mac counts no read, as it counts none at all.
pub const SURVEY_BY: &str = "by=";

/// The session a survey line's words (`rest`, after its verb) say asked for it: its
/// [`SURVEY_BY`] word, where that names a session aterm minted.
#[must_use]
pub fn survey_asker(rest: &str) -> Option<&str> {
    rest.split_ascii_whitespace()
        .find_map(|word| word.strip_prefix(SURVEY_BY))
        .filter(|sid| crate::control_verbs::is_minted_sid(sid))
}

/// A survey `line` with `by` named as the session that asked for it ([`SURVEY_BY`]).
#[must_use]
pub fn survey_marked(line: &str, by: &str) -> String {
    format!("{line} {SURVEY_BY}{by}")
}

/// A survey `line` without the word that names its asker ([`survey_asker`]), or `None` where it
/// carries none: what is asked of an aterm from before the word, and what a node job hands a
/// window. A [`SURVEY_BY`] word that names no session stays, for whoever serves the line to
/// refuse.
#[must_use]
pub fn survey_unmarked(line: &str) -> Option<String> {
    let words: Vec<&str> = line.split_ascii_whitespace().collect();
    let kept: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| survey_asker(word).is_none())
        .collect();
    (kept.len() != words.len()).then(|| kept.join(" "))
}

/// Whether the lock of the Mac that was asked let the request through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Its fleet access admitted the request.
    Admitted,
    /// Its fleet access refused it.
    Blocked {
        /// The level in force there.
        level: FleetAccess,
        /// The level the request needed.
        needs: FleetAccess,
    },
}

impl Outcome {
    /// What the first line of a drive's answer (`reply`) says of the lock there: blocked where
    /// it is the lock's refusal ([`FleetAccess::refused_at`]), admitted where it is `OK`, and
    /// `None` for any other answer, which the lock did not decide and nothing was done under.
    #[must_use]
    pub fn of_reply(reply: &str) -> Option<Self> {
        if let Some((level, needs)) = FleetAccess::refused_at(reply) {
            return Some(Self::Blocked { level, needs });
        }
        (reply == "OK" || reply.starts_with("OK ") || reply.starts_with("OK\n"))
            .then_some(Self::Admitted)
    }
}

/// The longest host name a row keeps, in bytes.
const HOST_MAX: usize = 255;

/// One request, or the like requests of one Mac folded over a minute ([`folded`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// When it was asked, in Unix milliseconds; for folded requests, when the last was.
    pub at_ms: u64,
    /// Which way it went.
    pub way: Way,
    /// The other Mac's node id, as its handshake proved it.
    pub node: String,
    /// The other Mac's name as this one knew it then.
    pub host: Option<String>,
    /// What was asked.
    pub what: Plain,
    /// Whether the asked Mac's fleet access admitted it.
    pub outcome: Outcome,
    /// The session that asked, where the request named one.
    pub by: Option<String>,
    /// How many requests the row stands for: 1, or the requests folded into it.
    pub n: u32,
}

impl Row {
    /// The row of one request of `kind` at `at_ms`, or `None` where it is not an agent request
    /// ([`counted`]) or `node` is no node id. A `host` or a `by` out of its shape is left out.
    #[must_use]
    pub fn of(
        at_ms: u64,
        way: Way,
        (node, host): (&str, Option<&str>),
        kind: DriveKind,
        by: Option<&str>,
        outcome: Outcome,
    ) -> Option<Self> {
        let by = by.filter(|by| crate::control_verbs::is_minted_sid(by));
        let what = counted(kind, by)?;
        is_node(node).then(|| Self {
            at_ms,
            way,
            node: node.to_owned(),
            host: host
                .map(str::trim)
                .filter(|host| is_host(host))
                .map(str::to_owned),
            what,
            outcome,
            by: by.map(str::to_owned),
            n: 1,
        })
    }

    /// Whether the asked Mac's fleet access refused it.
    #[must_use]
    pub const fn blocked(&self) -> bool {
        matches!(self.outcome, Outcome::Blocked { .. })
    }

    /// Whether `other` is a request like this one in the same minute of the clock: the same
    /// Mac, way and type, decided alike (admitted, or refused at the same level for the same
    /// need), and for a refusal asked by the same session. What a record that has outgrown its
    /// size keeps as one counted row ([`folded`]).
    #[must_use]
    pub fn alike(&self, other: &Self) -> bool {
        self.what == other.what
            && self.outcome == other.outcome
            && self.way == other.way
            && self.node == other.node
            && self.at_ms / FOLD_MS == other.at_ms / FOLD_MS
            && (!self.blocked() || self.by == other.by)
    }

    /// Whether a reader shows `later` in this row rather than after it: both admitted reads,
    /// [`Self::alike`]. Anything else has its own row until the record outgrows its size.
    #[must_use]
    pub fn folds(&self, later: &Self) -> bool {
        self.what == Plain::Read && self.outcome == Outcome::Admitted && self.alike(later)
    }

    /// This row with `later` folded in: their count, at the later time, with the name and the
    /// asking session of whichever names one, the later first.
    #[must_use]
    pub fn with(&self, later: &Self) -> Self {
        Self {
            at_ms: self.at_ms.max(later.at_ms),
            host: later.host.clone().or_else(|| self.host.clone()),
            by: later.by.clone().or_else(|| self.by.clone()),
            n: self.n.saturating_add(later.n),
            ..self.clone()
        }
    }

    /// The row's line in the record, without a newline: `k=v` words, a name percent-encoded.
    #[must_use]
    pub fn line(&self) -> String {
        let mut line = format!(
            "at={} way={} node={} host={} type={} n={}",
            self.at_ms,
            self.way.word(),
            self.node,
            self.host
                .as_deref()
                .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
            self.what.word(),
            self.n,
        );
        match self.outcome {
            Outcome::Admitted => line.push_str(" out=admitted"),
            Outcome::Blocked { level, needs } => {
                line.push_str(&format!(" out=blocked level={level} needs={needs}"));
            }
        }
        if let Some(by) = &self.by {
            line.push_str(&format!(" by={by}"));
        }
        line
    }

    /// [`Self::line`]'s inverse: `None` for a line that is no row (a word missing or out of its
    /// shape). A word this build does not know is passed over, so a later build's row reads.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let field = |key: &str| {
            line.split(' ')
                .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        };
        let outcome = match field("out")? {
            "admitted" => Outcome::Admitted,
            "blocked" => Outcome::Blocked {
                level: FleetAccess::from_word(field("level")?)?,
                needs: FleetAccess::from_word(field("needs")?)?,
            },
            _ => return None,
        };
        let node = field("node").filter(|node| is_node(node))?;
        let host = match field("host")? {
            "-" => None,
            host => Some(aterm_codec::pct::decode(host)).filter(|host| is_host(host)),
        };
        Some(Self {
            at_ms: field("at")?.parse().ok()?,
            way: Way::from_word(field("way")?)?,
            node: node.to_owned(),
            host,
            what: Plain::from_word(field("type")?)?,
            outcome,
            by: field("by")
                .filter(|by| crate::control_verbs::is_minted_sid(by))
                .map(str::to_owned),
            n: field("n")?.parse().ok().filter(|n| *n > 0)?,
        })
    }
}

/// `rows` (oldest first) with each minute's like requests as one counted row, oldest first by
/// the time of the last request each row stands for: the admitted reads alone ([`Row::folds`]),
/// what a reader shows, or with `every` each request that is [`Row::alike`] an earlier one,
/// what a record that has outgrown its size keeps. The requests add up the same either way.
#[must_use]
pub fn folded(rows: Vec<Row>, every: bool) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::with_capacity(rows.len());
    // Where the row of each minute, Mac, way and type that later rows fold into stands in
    // `out`; rows of one key that are not alike (another outcome, another asker) are few.
    let mut at: std::collections::HashMap<(u64, Way, Plain, String), Vec<usize>> =
        std::collections::HashMap::new();
    for row in rows {
        let key = (row.at_ms / FOLD_MS, row.way, row.what, row.node.clone());
        let places = at.entry(key).or_default();
        let into = places.iter().copied().find(|&place| {
            if every {
                out[place].alike(&row)
            } else {
                out[place].folds(&row)
            }
        });
        match into {
            Some(place) => out[place] = out[place].with(&row),
            None => {
                places.push(out.len());
                out.push(row);
            }
        }
    }
    out.sort_by_key(|row| row.at_ms);
    out
}

/// `rows` as they read at `now_ms`: a row stamped more than [`AHEAD_MS`] after it was written
/// before this Mac's clock was set back, and reads as written now, the latest it can have been.
/// Answers whether any row was. Whoever holds rows over time settles them once: a row so read
/// ages from then like any other.
pub fn settled(rows: &mut [Row], now_ms: u64) -> bool {
    let mut any = false;
    for row in rows
        .iter_mut()
        .filter(|row| row.at_ms > now_ms.saturating_add(AHEAD_MS))
    {
        row.at_ms = now_ms;
        any = true;
    }
    any
}

/// Whether `node` is a fleet node id: 32 lowercase hex digits, or a mesh principal (`n-` and 16
/// of them), as the fleet's handshake proves one (`aterm_net::fleet::is_node_id`).
fn is_node(node: &str) -> bool {
    let hex = |digits: &str, len: usize| {
        digits.len() == len
            && digits
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    hex(node, 32) || node.strip_prefix("n-").is_some_and(|id| hex(id, 16))
}

fn is_host(host: &str) -> bool {
    (1..=HOST_MAX).contains(&host.len()) && !host.chars().any(char::is_control)
}

/// How many requests of each [`Plain`] type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts([u64; Plain::ALL.len()]);

impl Counts {
    /// How many of `what`.
    #[must_use]
    pub const fn of(&self, what: Plain) -> u64 {
        self.0[what.index()]
    }

    /// How many in all.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.0.iter().sum()
    }

    fn add(&mut self, what: Plain, n: u64) {
        self.0[what.index()] += n;
    }

    /// The type with the most, a tie to the later of [`Plain::ALL`]; `None` where there is none.
    #[must_use]
    pub fn most(&self) -> Option<Plain> {
        Plain::ALL
            .into_iter()
            .filter(|what| self.of(*what) > 0)
            .max_by_key(|what| (self.of(*what), *what))
    }
}

/// The requests of one Mac, one way, within a summary's window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    /// The other Mac's node id.
    pub node: String,
    /// Its name, as the latest row that names one has it.
    pub host: Option<String>,
    /// Which way.
    pub way: Way,
    /// How many requests of each type.
    pub types: Counts,
    /// How many of each type the asked Mac's fleet access refused.
    pub blocked: Counts,
    /// When the latest was, in Unix milliseconds.
    pub last_ms: u64,
}

impl Pair {
    /// The Mac as a line names it: its name, else the head of its node id.
    #[must_use]
    pub fn name(&self) -> &str {
        self.host
            .as_deref()
            .unwrap_or_else(|| self.node.get(..8).unwrap_or(&self.node))
    }

    /// How many requests.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.types.total()
    }

    /// The type most of them were, a tie to the heavier.
    #[must_use]
    pub fn dominant(&self) -> Plain {
        self.types.most().unwrap_or(Plain::Read)
    }

    /// The type a person is told first: a new job where there is one, since it is the most
    /// another Mac asks of this one, else [`Self::dominant`].
    #[must_use]
    pub fn lead(&self) -> Plain {
        if self.types.of(Plain::NewJob) > 0 {
            Plain::NewJob
        } else {
            self.dominant()
        }
    }

    /// Whether the pair is only a few reads, each admitted ([`ROUTINE_READS`] at most): counted
    /// in a summary's totals, and given no row of its own in a menu.
    #[must_use]
    pub fn routine(&self) -> bool {
        self.blocked.total() == 0
            && self.types.of(Plain::Read) == self.count()
            && self.count() <= ROUTINE_READS
    }

    /// The pair in a few words, after its count: `reads`, `mostly typing`, `3 new jobs, 1
    /// blocked`, `2 new jobs, both blocked`. The lead type alone where every request was of it,
    /// `mostly` it where others were too, and new jobs by their own number; then what was
    /// blocked, as `both` or `all` where that is every request of the lead type and nothing else.
    #[must_use]
    pub fn phrase(&self) -> String {
        let lead = self.lead();
        let (of_lead, total) = (self.types.of(lead), self.count());
        let mut phrase = if lead == Plain::NewJob {
            format!("{of_lead} {}", lead.named(of_lead))
        } else if of_lead == total {
            lead.named(total).to_owned()
        } else {
            format!("mostly {}", lead.named(of_lead.max(2)))
        };
        let blocked = self.blocked.total();
        if blocked > 0 {
            let every = blocked == self.blocked.of(lead) && blocked == of_lead;
            phrase.push_str(&match (every, blocked) {
                (true, 1) => ", blocked".to_owned(),
                (true, 2) => ", both blocked".to_owned(),
                (true, _) => ", all blocked".to_owned(),
                (false, _) => format!(", {blocked} blocked"),
            });
        }
        phrase
    }
}

/// What a window of the record adds up to ([`summarize`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// The window's length, in milliseconds.
    pub window_ms: u64,
    /// How many distinct Macs asked this one.
    pub peers_in: usize,
    /// How many distinct Macs this one asked.
    pub peers_out: usize,
    /// How many requests of each type, both ways.
    pub types: Counts,
    /// How many incoming requests this Mac's fleet access refused.
    pub blocked_in: u64,
    /// How many outgoing requests another Mac's fleet access refused.
    pub blocked_out: u64,
    /// When this Mac's fleet access last refused an incoming request within the window.
    pub last_blocked_in_ms: Option<u64>,
    /// When the latest request of the whole record was, in or out of the window.
    pub last_ms: Option<u64>,
    /// Each Mac and way with a request in the window, busiest first, a tie to the more recent.
    pub pairs: Vec<Pair>,
    /// Since when the window's rows are whole, where that is later than the window's start: the
    /// record dropped rows up to then to stay within its size ([`Self::since`]).
    pub cut_ms: Option<u64>,
}

impl Summary {
    /// How many requests the window holds.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.types.total()
    }

    /// How many requests a fleet access refused, either way.
    #[must_use]
    pub const fn blocked(&self) -> u64 {
        self.blocked_in + self.blocked_out
    }

    /// The pairs a menu has rows for: the busiest [`SHOWN_PAIRS`] that are not
    /// [`Pair::routine`], in [`Self::pairs`]' order.
    pub fn shown(&self) -> impl Iterator<Item = &Pair> {
        self.pairs
            .iter()
            .filter(|pair| !pair.routine())
            .take(SHOWN_PAIRS)
    }

    /// This summary, made at `now_ms`, of a record that dropped its rows up to `cut_ms` to stay
    /// within its size: where that is inside the window, the summary covers only the time since
    /// and its headline says so.
    #[must_use]
    pub fn since(mut self, cut_ms: Option<u64>, now_ms: u64) -> Self {
        self.cut_ms = cut_ms.filter(|cut| now_ms.saturating_sub(*cut) < self.window_ms);
        self
    }

    /// The summary's one line at `now_ms`: `Last hour · in from 12 Macs · out to 4 · 3 blocked`,
    /// the blocked part only where something was; or, where the window holds no request, `Quiet
    /// for 3h` since the record's latest, or `Quiet` where the record is empty. The span it
    /// names is the one it covers: the window, or the time since the record last dropped rows
    /// where that is shorter (`Last 20m`, [`Self::since`]).
    #[must_use]
    pub fn headline(&self, now_ms: u64) -> String {
        if self.count() == 0 {
            return self.last_ms.map_or_else(
                || "Quiet".to_owned(),
                |last| format!("Quiet for {}", span(now_ms.saturating_sub(last))),
            );
        }
        let covers = self
            .cut_ms
            .map_or(self.window_ms, |cut| now_ms.saturating_sub(cut));
        let window = if covers == WINDOW_MS {
            "Last hour".to_owned()
        } else {
            format!("Last {}", span(covers))
        };
        let macs = |n: usize| {
            if n == 1 {
                "1 Mac".to_owned()
            } else {
                format!("{n} Macs")
            }
        };
        let mut line = format!(
            "{window} \u{b7} in from {} \u{b7} out to {}",
            macs(self.peers_in),
            self.peers_out
        );
        if self.blocked() > 0 {
            line.push_str(&format!(" \u{b7} {} blocked", self.blocked()));
        }
        line
    }
}

/// A span of `ms` in its largest whole unit: `40s`, `4m`, `2h`, `3d`.
#[must_use]
pub fn span(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// How long ago `at_ms` was at `now_ms`, as a row says it: `now` under a minute, else [`span`].
#[must_use]
pub fn ago(now_ms: u64, at_ms: u64) -> String {
    let age = now_ms.saturating_sub(at_ms);
    if age < 60_000 {
        "now".to_owned()
    } else {
        span(age)
    }
}

/// What `rows` add up to over the `window_ms` before `now_ms`. A row is in the window where it
/// is younger than `window_ms`; one stamped after `now_ms` is in it, as new (its reader settles
/// a stamp from before the clock was set back, [`settled`]).
/// The pairs come busiest first, a tie to the more recent, then by node and way, so one record
/// reads one way wherever it is summarized.
#[must_use]
pub fn summarize(rows: &[Row], now_ms: u64, window_ms: u64) -> Summary {
    let mut summary = Summary {
        window_ms,
        last_ms: rows.iter().map(|row| row.at_ms).max(),
        ..Summary::default()
    };
    // When the row that gave each pair its name was, beside the pair.
    let mut named_at: Vec<u64> = Vec::new();
    for row in rows {
        if now_ms.saturating_sub(row.at_ms) >= window_ms {
            continue;
        }
        let n = u64::from(row.n);
        summary.types.add(row.what, n);
        if row.blocked() {
            match row.way {
                Way::In => {
                    summary.blocked_in += n;
                    summary.last_blocked_in_ms = summary.last_blocked_in_ms.max(Some(row.at_ms));
                }
                Way::Out => summary.blocked_out += n,
            }
        }
        let at = summary
            .pairs
            .iter()
            .position(|pair| pair.way == row.way && pair.node == row.node);
        let at = at.unwrap_or_else(|| {
            summary.pairs.push(Pair {
                node: row.node.clone(),
                host: None,
                way: row.way,
                types: Counts::default(),
                blocked: Counts::default(),
                last_ms: 0,
            });
            named_at.push(0);
            summary.pairs.len() - 1
        });
        let pair = &mut summary.pairs[at];
        pair.types.add(row.what, n);
        if row.blocked() {
            pair.blocked.add(row.what, n);
        }
        pair.last_ms = pair.last_ms.max(row.at_ms);
        if row.host.is_some() && (pair.host.is_none() || row.at_ms >= named_at[at]) {
            pair.host.clone_from(&row.host);
            named_at[at] = row.at_ms;
        }
    }
    summary.peers_in = summary.pairs.iter().filter(|p| p.way == Way::In).count();
    summary.peers_out = summary.pairs.len() - summary.peers_in;
    summary.pairs.sort_by(|a, b| {
        (b.count(), b.last_ms)
            .cmp(&(a.count(), a.last_ms))
            .then_with(|| (&a.node, a.way).cmp(&(&b.node, b.way)))
    });
    summary
}

#[cfg(test)]
#[path = "fleet_activity_tests.rs"]
mod tests;
