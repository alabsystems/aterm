// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A person's review of what was asked of this Mac (ATERM-KI-1109-3, step 5 of the requests
//! design): `requests` lists the requests, `request show` reads one, and `request review`,
//! `request keep` and `request reply` act on one. `request claim` is the footprint's
//! ([`crate::requests_footprint::cmd_request_as`]).
//!
//! Who reads: any owner connection, an agent's tab included; and another Mac's drive, which reads
//! only the requests its own node made, and never the content tier (a fallback why, the
//! requesting tab's goal and asked lines, a reply's text).
//!
//! Who acts: this Mac's person, through aterm's own window, an `aterm ctl` in no tab, or a shell
//! tab a person opened where no agent runs and no control client typed. Never an agent's tab (a
//! request's requester included), a tab a request owns or `ctl spawn` started, one an update
//! carried over, one under the remote hold or one a control client typed into, aterm's harness,
//! the bridge or a fleet drive: nothing marks an unreviewed request reviewed on its own, so a
//! person always gets to see it (the design's §7).
//!
//! A read refuses where the journal cannot be read, never answering from what this process last
//! read, and says how much it read: the rows it skipped and whether it holds the whole journal
//! (`skipped=`, `window_complete=`). Its pages are bounded: `requests` by `n=` and a `before=`
//! cursor, `request show`'s tabs by `offset=` and `limit=`.
//!
//! Every read and act takes the store's lock briefly on the control thread, as `request claim`
//! does; [`LISTED`](crate::requests_host::listed) is republished by the requests' writer thread
//! after each act ([`crate::requests_host::relisted`]).

use crate::control::Scope;
use crate::fabric::report::{To, Unsent};
use crate::requests_footprint::Places;
use crate::session_store::{ExitActor, Store};
use aterm_codec::pct;
use aterm_requests::{
    ActRefusal, InspectionStatus, KeptRequest, Request, Reviewed, SaidOf, Stage, State, WhySrc,
    claim,
};
use aterm_session::SessionId;
use std::fmt::Write as _;
use std::path::PathBuf;

const USAGE: &str = "ERR usage: request claim path=<abs|~/…> [req=<id>] | request show <id> \
                     [offset=<n>] [limit=<1..512>] [--json] | request review <id> \
                     [note=<pct>] | request keep <id> | request reply <id> [--] <text>\n";

const LIST_USAGE: &str = "ERR usage: requests [state=open|running|concluded|unreviewed|reviewed|\
                          kept|all] [from=<node|head|sid>] [n=<1..512>] [before=<id>] [--json]\n";

/// How many rows `requests` prints with no `n=`, and `request show` tab rows with no `limit=`.
const LIST_DEFAULT: usize = 50;

/// The most rows `requests` prints (`n=`), and `request show` tab rows (`limit=`), as a board's.
const LIST_MAX: usize = 512;

/// The most bytes a `requests` reply takes, its rows cut short past it, as a board's: a drive's
/// reply carries at most `aterm_net::fleet::REPLY_MAX`.
const REPLY_BYTES: usize = 192 * 1024;

/// Send a one-line report from this session to that one under that key
/// ([`crate::fabric::report::post`]): the post's answer, or why nothing took it.
pub(crate) type Post<'p> =
    dyn Fn(&crate::SessionCtx, To<'_>, &str, &str) -> Result<String, Unsent> + 'p;

/// What a verb here reaches beyond the store and the session registry: the connection's process,
/// the main thread (Keep as Tab) and the fabric (a reply's mail), which a test stands in for.
pub(crate) struct Hands<'a> {
    /// The owner-token connection's process ([`crate::control::caller_actor_as`]).
    pub(crate) client: &'a dyn Fn() -> crate::typed_by::Who,
    /// Keep these sessions (local ids) as the person's tabs, by this actor, on the main thread:
    /// how many it kept.
    pub(crate) keep_tabs: &'a dyn Fn(Vec<u64>, ExitActor) -> Result<usize, String>,
    /// A reply's mail ([`Post`]).
    pub(crate) post: &'a Post<'a>,
    /// The home and state home a claim is judged against.
    pub(crate) places: &'a Places,
}

/// `requests …` or `request …` on the connection being served.
pub(crate) fn cmd(
    verb: &str,
    rest: &str,
    scope: Scope,
    store: &Store,
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
) -> String {
    let client = || crate::typed_by::Who::client(crate::control::serving_peer_pid());
    let keep_tabs = |sessions: Vec<u64>, by: ExitActor| {
        crate::control::control_media::call_main(proxy, |reply| crate::Wake::KeepTabs {
            sessions,
            by,
            reply,
        })
        .map_err(|error| error.to_string())
    };
    let post = |from: &crate::SessionCtx, to: To<'_>, key: &str, text: &str| {
        crate::fabric::report::post(from, to, key, text)
    };
    cmd_as(
        verb,
        rest,
        scope,
        store,
        &Hands {
            client: &client,
            keep_tabs: &keep_tabs,
            post: &post,
            places: &Places::host(),
        },
    )
}

/// [`cmd`] through `hands`.
pub(crate) fn cmd_as(
    verb: &str,
    rest: &str,
    scope: Scope,
    store: &Store,
    hands: &Hands<'_>,
) -> String {
    if verb == "requests" {
        return list(rest, &Reader::of_connection());
    }
    let mut words = rest.split_whitespace();
    let sub = words.next();
    let after = rest
        .trim_start()
        .get(sub.map_or(0, str::len)..)
        .unwrap_or("");
    match sub {
        Some("claim") => crate::requests_footprint::cmd_request_as(
            rest,
            scope,
            store,
            || (hands.client)(),
            hands.places,
        ),
        Some("show") => show(after, &Reader::of_connection(), hands.places),
        Some(act @ ("review" | "keep" | "reply")) => {
            let (actor, by) = match actor_of(act, scope, store, hands) {
                Ok(acting) => acting,
                Err(refusal) => return refusal,
            };
            match act {
                "review" => review(after, &by),
                "keep" => keep(after, actor, &by, store, hands),
                _ => reply(after, &actor, &by, store, hands),
            }
        }
        _ => USAGE.to_owned(),
    }
}

/// Who reads.
enum Reader {
    /// An owner connection of this Mac: every request, with the content tier.
    Owner,
    /// Another Mac's drive: only its own node's requests, without the content tier.
    Drive { node: String },
}

impl Reader {
    fn of_connection() -> Self {
        crate::fabric::serving_driver().map_or(Self::Owner, |driver| Self::Drive {
            node: driver.node.trim().to_owned(),
        })
    }

    fn sees(&self, request: &Request) -> bool {
        match self {
            Self::Owner => true,
            Self::Drive { node } => !node.is_empty() && request.open.node == *node,
        }
    }

    const fn content(&self) -> bool {
        matches!(self, Self::Owner)
    }
}

/// The store's folder, where this Mac keeps one and it has a journal; `Err` the reply.
fn store_dir(what: &str) -> Result<Option<PathBuf>, String> {
    match crate::requests_host::dir() {
        Ok(Some(dir)) => Ok(dir.join(aterm_requests::JOURNAL).exists().then_some(dir)),
        Ok(None) => Err(format!("ERR {what}: this Mac keeps no requests\n")),
        Err(why) => Err(format!("ERR request store: {why}\n")),
    }
}

/// `value` percent-encoded, `-` where empty.
fn enc(value: &str) -> String {
    if value.is_empty() {
        "-".to_owned()
    } else {
        pct::encode(value)
    }
}

fn opt_enc(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_owned(), enc)
}

fn num(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

/// Request `id`'s why as `reader` may read it: the given line, or [`aterm_requests::why::NONE_GIVEN`],
/// to anyone; a fallback's text to an owner, `1` to a drive.
fn why_for(store: &mut aterm_requests::Store, id: &str, reader: &Reader) -> (String, WhySrc) {
    if reader.content() {
        return crate::requests_host::why_of(store, id)
            .map(|(why, src)| (why.to_string(), src))
            .unwrap_or_else(|| ("-".to_owned(), WhySrc::None));
    }
    let Some(request) = store.get(id) else {
        return ("-".to_owned(), WhySrc::None);
    };
    let text = match request.open.why_src {
        WhySrc::Given => request.why.clone().unwrap_or_else(|| "-".to_owned()),
        WhySrc::None => aterm_requests::why::NONE_GIVEN.to_owned(),
        WhySrc::Brief | WhySrc::Goal | WhySrc::Asked => "1".to_owned(),
    };
    (text, request.open.why_src)
}

/// A request's `request` row, no newline, its why `why`.
fn request_row(request: &Request, why: &(String, WhySrc), now_ms: u64) -> String {
    let open = &request.open;
    let bytes = match crate::requests_host::claim_bytes(request, now_ms) {
        Some((bytes, true)) => bytes.to_string(),
        Some((bytes, false)) => format!("{bytes}+"),
        None => "-".to_owned(),
    };
    format!(
        "request id={} opened_ms={} stage={} out={} end_why={} via={} form={} node={} host={} \
         by={} program={} model={} tabs={} open_tabs={} claims={} bytes={bytes} reviewed_by={} \
         kept={} why_src={} why={}",
        request.id,
        request.opened_ms,
        request.stage().word(),
        request.end.map_or("-", |(out, _)| out.word()),
        request.end.map_or("-", |(_, why)| why.word()),
        open.via.word(),
        enc(&open.form.word()),
        enc(&open.node),
        enc(&open.host),
        enc(&open.by),
        opt_enc(open.program.as_deref()),
        opt_enc(open.model.as_deref()),
        request.tabs.len(),
        request.tabs.iter().filter(|tab| tab.is_open()).count(),
        request.claims.len(),
        request
            .review
            .as_ref()
            .map_or_else(|| "-".to_owned(), |review| enc(&review.by)),
        u8::from(request.kept.is_some()),
        why.1.word(),
        enc(&why.0),
    )
}

/// What `requests state=` keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Open,
    Running,
    /// Any concluded request, reviewed or not.
    Concluded,
    /// Concluded, and not reviewed.
    Unreviewed,
    Reviewed,
    Kept,
}

impl Filter {
    fn of(word: &str) -> Option<Self> {
        Some(match word {
            "all" => Self::All,
            "open" => Self::Open,
            "running" => Self::Running,
            "concluded" => Self::Concluded,
            "unreviewed" => Self::Unreviewed,
            "reviewed" => Self::Reviewed,
            "kept" => Self::Kept,
            _ => return None,
        })
    }

    fn admits(self, request: &Request) -> bool {
        match self {
            Self::All => true,
            Self::Open => request.stage() == Stage::Open,
            Self::Running => request.stage() == Stage::Running,
            Self::Concluded => matches!(request.state(), State::Concluded(_)),
            Self::Unreviewed => request.stage() == Stage::Concluded,
            Self::Reviewed => request.stage() == Stage::Reviewed,
            Self::Kept => request.kept.is_some(),
        }
    }
}

/// Whether `from` names request `request`'s requester: its whole node, its node's head, its
/// actor token, or the session that token names.
fn from_matches(request: &Request, from: &str) -> bool {
    let open = &request.open;
    let fleet_sid = open
        .by
        .strip_prefix("fleet:")
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, sid)| sid)
        .filter(|sid| *sid != "-");
    (!open.node.is_empty()
        && (open.node.eq_ignore_ascii_case(from)
            || aterm_requests::id::head_of(&open.node) == from))
        || open.by == from
        || fleet_sid == Some(from)
}

/// `n` as a count of rows between 1 and [`LIST_MAX`].
fn count_of(n: &str) -> Option<usize> {
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse()
        .ok()
        .filter(|count| (1..=LIST_MAX).contains(count))
}

/// The coverage words of a read: rows skipped, and whether the fold holds the whole journal.
fn coverage(status: Option<InspectionStatus>) -> String {
    let status = status.unwrap_or(InspectionStatus {
        skipped_rows: 0,
        complete: true,
    });
    format!(
        "skipped={} window_complete={}",
        status.skipped_rows,
        u8::from(status.complete)
    )
}

/// `requests [state=…] [from=…] [n=…] [before=…] [--json]` for `reader`: newest first by
/// opening time, then id; `before=` the id a page ended at, whose `next=` it was.
fn list(rest: &str, reader: &Reader) -> String {
    let (mut filter, mut from, mut n, mut before, mut json) =
        (Filter::All, None, LIST_DEFAULT, None, false);
    for word in rest.split_whitespace() {
        if aterm_types::control_verbs::json_flag(word) {
            json = true;
        } else if let Some(value) = word.strip_prefix("state=") {
            let Some(of) = Filter::of(value) else {
                return LIST_USAGE.to_owned();
            };
            filter = of;
        } else if let Some(value) = word.strip_prefix("from=") {
            if value.is_empty() {
                return LIST_USAGE.to_owned();
            }
            from = Some(pct::decode(value));
        } else if let Some(value) = word.strip_prefix("n=") {
            match count_of(value) {
                Some(count) => n = count,
                None => return LIST_USAGE.to_owned(),
            }
        } else if let Some(value) = word.strip_prefix("before=")
            && before.is_none()
        {
            if let Err(why) = aterm_types::control_verbs::request_id(value) {
                return format!(
                    "ERR requests before={}: {why}\n",
                    aterm_codec::tty::one_line(value, 40)
                );
            }
            before = Some(value.to_owned());
        } else {
            return LIST_USAGE.to_owned();
        }
    }
    let dir = match store_dir("requests") {
        Ok(dir) => dir,
        Err(refusal) => return refusal,
    };
    let now = crate::requests_host::now_unix_ms();
    let (mut total, mut unreviewed, mut rows, mut next, mut status) =
        (0, 0, Vec::new(), None, None);
    if let Some(dir) = dir {
        let read = crate::requests_host::with_store(&dir, |store| {
            let (requests, read) = store.inspect()?;
            status = Some(read);
            let mut seen: Vec<(u64, String)> = Vec::new();
            let mut cursor = None;
            for request in requests.filter(|request| reader.sees(request)) {
                total += 1;
                if request.stage() == Stage::Concluded {
                    unreviewed += 1;
                }
                if before.as_deref() == Some(request.id.as_str()) {
                    cursor = Some((request.opened_ms, request.id.clone()));
                }
                if filter.admits(request)
                    && from
                        .as_deref()
                        .is_none_or(|from| from_matches(request, from))
                {
                    seen.push((request.opened_ms, request.id.clone()));
                }
            }
            // A cursor names a request this reader sees; another is no page's end.
            if before.is_some() && cursor.is_none() {
                return Ok(Err("ERR requests: no such request cursor\n".to_owned()));
            }
            seen.retain(|key| cursor.as_ref().is_none_or(|cursor| key < cursor));
            seen.sort_by(|a, b| b.cmp(a));
            let mut rows = Vec::new();
            let mut bytes = 0;
            let mut last = None;
            for (_, id) in seen.iter().take(n) {
                let why = why_for(store, id, reader);
                let Some(request) = store.get(id) else {
                    continue;
                };
                let row = request_row(request, &why, now);
                bytes += row.len() + 1;
                if bytes > REPLY_BYTES {
                    break;
                }
                rows.push(row);
                last = Some(id.clone());
            }
            // More remain past this page (by `n=` or the reply's bytes): the next starts after
            // its last row.
            next = last.filter(|_| rows.len() < seen.len());
            Ok(Ok(rows))
        });
        rows = match read {
            Ok(Ok(rows)) => rows,
            Ok(Err(refusal)) => return refusal,
            Err(error) => return format!("ERR request store: {error}\n"),
        };
    } else if before.is_some() {
        return "ERR requests: no such request cursor\n".to_owned();
    }
    let header = format!(
        "requests total={total} unreviewed={unreviewed} shown={} next={} {}",
        rows.len(),
        next.as_deref().unwrap_or("-"),
        coverage(status),
    );
    if json {
        return aterm_control::wire::json_ok(&format!(
            "{{\"requests\":{},\"rows\":[{}]}}",
            json_object(&header),
            rows.iter()
                .map(|row| json_object(row))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    let mut out = format!("OK {}\n{header}\n", rows.len() + 1);
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out
}

/// A row's `k=v` words after its first as one JSON object, each value decoded, as a string.
fn json_object(row: &str) -> String {
    let members: Vec<String> = row
        .split(' ')
        .skip(1)
        .filter_map(|word| word.split_once('='))
        .map(|(key, value)| {
            format!(
                "\"{}\":\"{}\"",
                aterm_codec::json::escape(key),
                aterm_codec::json::escape(&pct::decode(value))
            )
        })
        .collect();
    format!("{{{}}}", members.join(","))
}

/// A request id word, or the reply refusing it.
fn id_of<'a>(word: Option<&'a str>, verb: &str) -> Result<&'a str, String> {
    let Some(word) = word else {
        return Err(USAGE.to_owned());
    };
    aterm_types::control_verbs::request_id(word).map_err(|why| {
        format!(
            "ERR request {verb} {}: {why}\n",
            aterm_codec::tty::one_line(word, 40)
        )
    })
}

/// Which of a request's tabs a `request show` prints: `limit` of them from the `offset`th.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Page {
    offset: usize,
    limit: usize,
}

/// `request show <id> [offset=<n>] [limit=<n>] [--json]` for `reader`; `places` names the home
/// a claim's measure reads.
fn show(after: &str, reader: &Reader, places: &Places) -> String {
    let mut words = after.split_whitespace();
    let id = match id_of(words.next(), "show") {
        Ok(id) => id.to_owned(),
        Err(refusal) => return refusal,
    };
    let (mut json, mut offset, mut limit) = (false, None, None);
    for word in words {
        if aterm_types::control_verbs::json_flag(word) {
            json = true;
        } else if let Some(value) = word.strip_prefix("offset=")
            && offset.is_none()
            && !value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && let Ok(at) = value.parse::<usize>()
        {
            offset = Some(at);
        } else if let Some(value) = word.strip_prefix("limit=")
            && limit.is_none()
            && let Some(count) = count_of(value)
        {
            limit = Some(count);
        } else {
            return USAGE.to_owned();
        }
    }
    let page = Page {
        offset: offset.unwrap_or(0),
        limit: limit.unwrap_or(LIST_DEFAULT),
    };
    let unknown = || format!("ERR request show: {}\n", ActRefusal::Unknown);
    let dir = match store_dir("request show") {
        Ok(Some(dir)) => dir,
        Ok(None) => return unknown(),
        Err(refusal) => return refusal,
    };
    let now = crate::requests_host::now_unix_ms();
    let read = crate::requests_host::with_store(&dir, |store| {
        let (_, status) = store.inspect()?;
        let Some(tabs) = store
            .get(&id)
            .filter(|request| reader.sees(request))
            .map(|request| request.tabs.len())
        else {
            return Ok(Err(unknown()));
        };
        if page.offset > tabs {
            return Ok(Err(format!(
                "ERR request show: offset={} is past its {tabs} tabs\n",
                page.offset
            )));
        }
        let why = why_for(store, &id, reader);
        let said = if reader.content() {
            Some(store.said(&id).unwrap_or_default())
        } else {
            None
        };
        Ok(store.get(&id).map_or_else(
            || Err(unknown()),
            |request| {
                Ok((
                    shown(request, &why, said.as_ref(), now, page, status),
                    stale(request, now),
                ))
            },
        ))
    });
    let (lines, stale) = match read {
        Ok(Ok(read)) => read,
        Ok(Err(refusal)) => return refusal,
        Err(error) => return format!("ERR request store: {error}\n"),
    };
    // A claim not measured lately is measured again on the footprint thread; the read does not
    // wait for it.
    crate::requests_footprint::measure(
        now,
        stale,
        places.home.clone(),
        crate::requests_footprint::measure_probe(),
    );
    if json {
        return aterm_control::wire::json_ok(&show_json(&lines));
    }
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The claimed folders of `request` no measure within its lifetime covers.
fn stale(request: &Request, now_ms: u64) -> Vec<String> {
    request
        .claims
        .iter()
        .filter(|held| crate::requests_footprint::measured(&held.claim.path, now_ms).is_none())
        .map(|held| held.claim.path.clone())
        .collect()
}

/// `request show`'s rows of `request`, its why `why`, and what it said where the reader reads
/// the content tier: its tabs (and their timeline) only those `page` names, an `offset` no
/// greater than its tabs; `status` the read's coverage.
fn shown(
    request: &Request,
    why: &(String, WhySrc),
    said: Option<&SaidOf>,
    now_ms: u64,
    page: Page,
    status: InspectionStatus,
) -> Vec<String> {
    let tabs = &request.tabs[page.offset..(page.offset + page.limit).min(request.tabs.len())];
    let after = page.offset + tabs.len();
    let mut lines = vec![
        request_row(request, why, now_ms),
        format!(
            "page offset={} shown={} tabs={} next_offset={} {}",
            page.offset,
            tabs.len(),
            request.tabs.len(),
            if after < request.tabs.len() {
                after.to_string()
            } else {
                "-".to_owned()
            },
            coverage(Some(status)),
        ),
    ];
    if let Some(said) = said {
        lines.push(format!(
            "said why={} goal={} asked={}",
            opt_enc(said.why.as_ref().map(|(text, _)| text.as_str())),
            opt_enc(said.goal.as_deref()),
            opt_enc(said.asked.as_deref()),
        ));
    }
    for tab in tabs {
        let verbs: Vec<String> = tab
            .verbs
            .iter()
            .map(|(verb, n)| format!("{}:{n}", verb.word()))
            .collect();
        let spent = tab.spent.unwrap_or_default();
        let gone = tab.gone.as_ref();
        lines.push(format!(
            "tab sid={} opened_ms={} last_ms={} verbs={} left={} left_ms={} reason={} code={} \
             task={} kept={} cpu_s={} peak_mb={} written_mb={}",
            tab.sid,
            tab.opened_ms,
            tab.last_ms,
            if verbs.is_empty() {
                "-".to_owned()
            } else {
                verbs.join(",")
            },
            tab.left.map_or("-", aterm_requests::Out::word),
            num(gone.map(|gone| gone.at_ms)),
            gone.map_or("-", |gone| gone.reason.as_str()),
            gone.and_then(|gone| gone.code)
                .map_or_else(|| "-".to_owned(), |code| code.to_string()),
            gone.and_then(|gone| gone.task)
                .map_or("-", aterm_requests::TaskEnd::word),
            u8::from(tab.kept),
            num(spent.cpu_s),
            num(spent.peak_mb),
            num(spent.written_mb),
        ));
    }
    for held in &request.claims {
        let claim = &held.claim;
        let measured = crate::requests_footprint::measured(&claim.path, now_ms);
        let bytes = measured.and_then(|measured| {
            measured.bytes.map(|bytes| {
                if measured.complete {
                    bytes.to_string()
                } else {
                    format!("{bytes}+")
                }
            })
        });
        lines.push(format!(
            "claim path={} src={} by={} tab={} born_ms={} alone={} at_ms={} keep={} bytes={} \
             measured_ms={}",
            pct::encode(&claim.path),
            claim.src.word(),
            enc(&claim.by),
            claim.tab.as_deref().unwrap_or("-"),
            num(claim.born_ms),
            claim
                .alone
                .map_or("-", |alone| if alone { "1" } else { "0" }),
            held.at_ms,
            claim::keep_of(request, claim, measured.as_ref(), now_ms)
                .map_or("-", aterm_requests::Keep::word),
            bytes.unwrap_or_else(|| "-".to_owned()),
            num(measured.map(|measured| measured.at_ms)),
        ));
    }
    if let Some(review) = &request.review {
        lines.push(format!(
            "review by={} at_ms={} note={}",
            enc(&review.by),
            review.at_ms,
            opt_enc(review.note.as_deref()),
        ));
    }
    for reply in &request.replies {
        let text = said.and_then(|said| {
            said.replies
                .iter()
                .find(|(key, _)| *key == reply.key)
                .map(|(_, text)| text.as_str())
        });
        lines.push(format!(
            "reply by={} at_ms={} key={} sent={} text={}",
            enc(&reply.by),
            reply.at_ms,
            reply.key,
            u8::from(reply.sent),
            opt_enc(text),
        ));
    }
    lines.extend(timeline(request, tabs).into_iter().map(|(t, what)| {
        let mut line = format!("timeline t={t} {what}");
        line.truncate(line.trim_end().len());
        line
    }));
    lines
}

/// Request `request`'s folded records as a timeline, oldest first, of its tabs only `tabs`: each
/// one's time and its `what=<word>` and keys.
fn timeline(request: &Request, tabs: &[aterm_requests::Tab]) -> Vec<(u64, String)> {
    let mut events = vec![(
        request.opened_ms,
        format!(
            "what=open via={} by={}",
            request.open.via.word(),
            enc(&request.open.by)
        ),
    )];
    for tab in tabs {
        events.push((tab.opened_ms, format!("what=tab sid={}", tab.sid)));
        if tab.last_ms > tab.opened_ms {
            events.push((tab.last_ms, format!("what=drove sid={}", tab.sid)));
        }
        if let Some(gone) = &tab.gone {
            events.push((
                gone.at_ms,
                format!(
                    "what=left sid={} out={} reason={} code={}",
                    tab.sid,
                    tab.left.map_or("-", aterm_requests::Out::word),
                    gone.reason,
                    gone.code
                        .map_or_else(|| "-".to_owned(), |code| code.to_string()),
                ),
            ));
        }
        if let Some(at) = tab.kept_ms {
            events.push((at, format!("what=keep sid={}", tab.sid)));
        }
    }
    if let (Some((out, why)), Some(at)) = (request.end, request.ended_ms) {
        events.push((
            at,
            format!("what=end out={} why={}", out.word(), why.word()),
        ));
    }
    for held in &request.claims {
        events.push((
            held.at_ms,
            format!(
                "what=claim path={} src={}",
                pct::encode(&held.claim.path),
                held.claim.src.word()
            ),
        ));
    }
    if let Some(review) = &request.review {
        events.push((review.at_ms, format!("what=review by={}", enc(&review.by))));
    }
    if let Some((by, at)) = &request.kept {
        events.push((*at, format!("what=kept by={}", enc(by))));
    }
    for reply in &request.replies {
        events.push((
            reply.at_ms,
            format!(
                "what=reply by={} sent={}",
                enc(&reply.by),
                u8::from(reply.sent)
            ),
        ));
    }
    events.sort_by_key(|(at, _)| *at);
    events
}

/// `request show`'s rows as one JSON object: `request`, `page`, `said` (or null), `tabs`, `claims`,
/// `review` (or null), `replies` and `timeline`.
fn show_json(lines: &[String]) -> String {
    let of = |kind: &str| -> Vec<String> {
        lines
            .iter()
            .filter(|line| line.split(' ').next() == Some(kind))
            .map(|line| json_object(line))
            .collect()
    };
    let one = |kind: &str| {
        of(kind)
            .into_iter()
            .next()
            .unwrap_or_else(|| "null".to_owned())
    };
    let many = |kind: &str| format!("[{}]", of(kind).join(","));
    format!(
        "{{\"request\":{},\"page\":{},\"said\":{},\"tabs\":{},\"claims\":{},\"review\":{},\"replies\":{},\
         \"timeline\":{}}}",
        one("request"),
        one("page"),
        one("said"),
        many("tab"),
        many("claim"),
        one("review"),
        many("reply"),
        many("timeline"),
    )
}

/// Who acts on this connection, where it may act on a request (`verb`): the caller as the
/// records name it and its token; `Err` the refusal.
///
/// A tab is this Mac's person's only where nothing but the person can have typed the command
/// that asks: a tab a request owns (its own or a restore's stand-in for it, read through its
/// lineage as the survey does), one where an agent runs, one `ctl spawn` started, one an update
/// carried over (the handoff keeps no record of who started or drove it), one under the remote
/// hold, or one a control client other than aterm's own was ever admitted to type into is
/// refused, as `reveal` refuses them ([`crate::control::marks_of`]).
fn actor_of(
    verb: &str,
    scope: Scope,
    store: &Store,
    hands: &Hands<'_>,
) -> Result<(ExitActor, String), String> {
    const PERSON: &str = "a request is reviewed by this Mac's person: aterm's window, an aterm \
                          ctl in no tab, or a shell tab a person opened where no agent runs and \
                          nothing else typed";
    let refused = |who: &str| Err(format!("ERR request {verb}: {PERSON}, never {who}\n"));
    match crate::control::caller_actor_as(scope, None, store, || (hands.client)()) {
        ExitActor::Ctl => Ok((ExitActor::Ctl, "ctl".to_owned())),
        ExitActor::Human => Ok((ExitActor::Human, "human".to_owned())),
        ExitActor::Sid(sid) => match tab_of(store, &sid) {
            Some(Tab::Person) => Ok((ExitActor::Sid(sid.clone()), sid)),
            Some(Tab::Agent) => refused("an agent (its requester included)"),
            Some(Tab::Request) => refused("a tab a request owns"),
            Some(Tab::Spawned) => refused("a tab ctl spawn started"),
            Some(Tab::Carried) => refused("a tab an update carried over"),
            Some(Tab::Held) => refused("a tab under the remote hold"),
            Some(Tab::Driven) => refused("a tab a control client typed into"),
            None => refused("a tab this Mac no longer hosts"),
        },
        ExitActor::Fleet { .. } => refused("a fleet drive"),
        ExitActor::Harness => refused("aterm's harness"),
        _ => refused("this caller"),
    }
}

/// Whose tab an `aterm ctl` asked from, as [`actor_of`] judges it: the first that holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Agent,
    Request,
    Spawned,
    Carried,
    Held,
    Driven,
    Person,
}

/// Tab `sid` of `store` as [`Tab`] reads it; `None` where this Mac no longer hosts it.
fn tab_of(store: &Store, sid: &str) -> Option<Tab> {
    let handle = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_sid(&SessionId::new(sid))
        .cloned()?;
    let lineage = {
        let timeline = handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        timeline.slog().lineage().map(|id| id.as_str().to_owned())
    };
    let marks = crate::control::marks_of(&handle.ctx);
    Some(if crate::survey::asking_of(&handle).is_some() {
        Tab::Agent
    } else if crate::requests_host::placed(sid, lineage.as_deref()).is_some() {
        Tab::Request
    } else if marks.spawned {
        Tab::Spawned
    } else if marks.carried {
        Tab::Carried
    } else if marks.held {
        Tab::Held
    } else if marks.driven {
        Tab::Driven
    } else {
        Tab::Person
    })
}

/// `request review <id> [note=<pct>]` by `by`.
fn review(after: &str, by: &str) -> String {
    let mut words = after.split_whitespace();
    let id = match id_of(words.next(), "review") {
        Ok(id) => id.to_owned(),
        Err(refusal) => return refusal,
    };
    let mut note = None;
    for word in words {
        match word.strip_prefix("note=") {
            Some(value) if !value.is_empty() && note.is_none() => {
                note = Some(pct::decode(value));
            }
            _ => return USAGE.to_owned(),
        }
    }
    let dir = match store_dir("request review") {
        Ok(Some(dir)) => dir,
        Ok(None) => return format!("ERR request review: {}\n", ActRefusal::Unknown),
        Err(refusal) => return refusal,
    };
    let at = crate::requests_host::now_unix_ms();
    let done = crate::requests_host::with_store(&dir, |store| {
        Ok(store.review(at, &id, by, note.as_deref()))
    });
    match done {
        Ok(Ok(Reviewed::Already)) => format!("OK already req={id}\n"),
        Ok(Ok(_)) => {
            aterm_log::info!("requests: {id} reviewed by {by}");
            crate::requests_host::relisted(&dir);
            format!("OK reviewed req={id}\n")
        }
        Ok(Err(refusal)) => format!("ERR request review: {refusal}\n"),
        Err(error) => format!("ERR request store: {error}\n"),
    }
}

/// `request keep <id>` by `actor` (`by` its token).
fn keep(after: &str, actor: ExitActor, by: &str, store: &Store, hands: &Hands<'_>) -> String {
    let mut words = after.split_whitespace();
    let id = match id_of(words.next(), "keep") {
        Ok(id) => id.to_owned(),
        Err(refusal) => return refusal,
    };
    if words.next().is_some() {
        return USAGE.to_owned();
    }
    let dir = match store_dir("request keep") {
        Ok(Some(dir)) => dir,
        Ok(None) => return format!("ERR request keep: {}\n", ActRefusal::Unknown),
        Err(refusal) => return refusal,
    };
    let at = crate::requests_host::now_unix_ms();
    let kept = crate::requests_host::with_store(&dir, |store| Ok(store.keep_request(at, &id, by)));
    let KeptRequest { tabs, already } = match kept {
        Ok(Ok(kept)) => kept,
        Ok(Err(refusal)) => return format!("ERR request keep: {refusal}\n"),
        Err(error) => return format!("ERR request store: {error}\n"),
    };
    if !already {
        aterm_log::info!("requests: {id} kept by {by}: its tabs and folders are the person's");
        crate::requests_host::relisted(&dir);
    }
    let sessions: Vec<u64> = {
        let sessions = store.read().unwrap_or_else(|p| p.into_inner());
        tabs.iter()
            .filter_map(|sid| sessions.by_sid(&SessionId::new(sid)))
            .map(|handle| handle.local_id)
            .collect()
    };
    let word = if already { "already" } else { "kept" };
    if sessions.is_empty() {
        return format!("OK {word} req={id} tabs=0\n");
    }
    match (hands.keep_tabs)(sessions, actor) {
        Ok(n) => format!("OK {word} req={id} tabs={n}\n"),
        Err(why) => {
            format!("ERR request keep: req={id} is kept, but its tabs did not move: {why}\n")
        }
    }
}

/// Where a reply to a request asked by `by` (on node `node`) goes: its requester's session.
fn reply_to<'a>(by: &'a str, node: &'a str) -> Result<To<'a>, &'static str> {
    if let Some((_, sid)) = by
        .strip_prefix("fleet:")
        .and_then(|rest| rest.split_once(':'))
    {
        return if sid == "-" {
            Err("its requester named no session, so no mail reaches it")
        } else {
            Ok(To::There { sid, node })
        };
    }
    if aterm_types::control_verbs::is_minted_sid(by) {
        Ok(To::Here(by))
    } else {
        Err("its requester names no session")
    }
}

/// `request reply <id> [--] <text>` by `actor` (`by` its token).
fn reply(after: &str, actor: &ExitActor, by: &str, store: &Store, hands: &Hands<'_>) -> String {
    let after = after.trim_start();
    let (word, text) = after.split_once(char::is_whitespace).unwrap_or((after, ""));
    let id = match id_of(Some(word).filter(|word| !word.is_empty()), "reply") {
        Ok(id) => id.to_owned(),
        Err(refusal) => return refusal,
    };
    let text = text.trim_start();
    let text = text
        .strip_prefix("--")
        .map_or(text, str::trim_start)
        .trim_end();
    if text.is_empty() {
        return USAGE.to_owned();
    }
    if aterm_requests::why::line_of(text).as_deref() != Some(text) {
        return "ERR request reply: a reply is one visible line of at most 200 characters with \
                no double quote\n"
            .to_owned();
    }
    let dir = match store_dir("request reply") {
        Ok(Some(dir)) => dir,
        Ok(None) => return format!("ERR request reply: {}\n", ActRefusal::Unknown),
        Err(refusal) => return refusal,
    };
    let read = crate::requests_host::with_store(&dir, |store| {
        let _ = store.inspect()?;
        Ok(store.get(&id).map(|request| {
            (
                request.open.by.clone(),
                request.open.node.clone(),
                request.stage(),
                request.end.map(|(out, _)| out),
                request
                    .tabs
                    .iter()
                    .filter(|tab| tab.is_open())
                    .map(|tab| tab.sid.clone())
                    .collect::<Vec<_>>(),
            )
        }))
    });
    let (requester, node, stage, out, open_tabs) = match read {
        Ok(Some(read)) => read,
        Ok(None) => return format!("ERR request reply: {}\n", ActRefusal::Unknown),
        Err(error) => return format!("ERR request store: {error}\n"),
    };
    let to = match reply_to(&requester, &node) {
        Ok(to) => to,
        Err(why) => return format!("ERR request reply: {why}\n"),
    };
    // It goes from the caller's own tab, else from a tab of the request this Mac hosts.
    let from = {
        let sessions = store.read().unwrap_or_else(|p| p.into_inner());
        let own = match actor {
            ExitActor::Sid(sid) => Some(sid.clone()),
            _ => None,
        };
        own.into_iter().chain(open_tabs).find_map(|sid| {
            sessions
                .by_sid(&SessionId::new(&sid))
                .map(|h| h.ctx.clone())
        })
    };
    let Some(from) = from else {
        return "ERR request reply: no tab of this Mac can send it (run it in a shell tab)\n"
            .to_owned();
    };
    let at = crate::requests_host::now_unix_ms();
    let key = format!(
        "rq.{id}.{:016x}",
        aterm_hash::fnv1a_64(format!("{at} {text}").as_bytes())
    );
    let mut line = format!(
        "reply to request {id} from {}: {text} | req={id} stage={} out={}",
        crate::requests_host::own_host(),
        stage.word(),
        out.map_or("-", aterm_requests::Out::word),
    );
    if line.len() > crate::fabric::report::REPORT_MAX {
        line = format!("reply to request {id}: {text}");
    }
    let sent = (hands.post)(&from, to, &key, &line);
    let recorded = crate::requests_host::with_store(&dir, |store| {
        Ok(store.reply(at, &id, by, &key, sent.is_ok(), text))
    });
    crate::requests_host::relisted(&dir);
    let address = match to {
        To::Here(sid) => format!("@{sid}"),
        To::There { sid, node } => format!("@{sid}@{node}"),
    };
    let unrecorded = match recorded {
        Ok(Ok(())) => None,
        Ok(Err(refusal)) => Some(refusal.to_string()),
        Err(error) => Some(error.to_string()),
    };
    if let Some(why) = &unrecorded {
        aterm_log::warn!("requests: a reply to {id} was not recorded: {why}");
    }
    match sent {
        Ok(answer) => {
            aterm_log::info!("requests: {by} replied to {id}'s requester {address}: {answer}");
            let mut reply = format!(
                "OK replied req={id} to={address} {}",
                aterm_codec::tty::one_line(answer.trim(), 200)
            );
            if unrecorded.is_some() {
                reply.push_str(" recorded=0");
            }
            let _ = writeln!(reply);
            reply
        }
        Err(why) => format!("ERR request reply: {why}\n"),
    }
}

#[cfg(test)]
#[path = "requests_review_tests.rs"]
mod tests;
