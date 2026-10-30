// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Direct session mail over an authenticated fleet drive. Its `fN` identifiers belong to this
//! mailbox, never to a broker log. A reply is retained for the sending node to read on its next
//! drive; no sender can read another node's conversations. The private file is authoritative,
//! so a lost reply, a process restart and a seamless update all preserve the same identifiers.

use super::{FabricSession, serving_driver};
use serde::{Deserialize, Serialize};

const ROWS: usize = 64;
const PER_NODE: usize = 16;
const TEXT: usize = 4096;
#[cfg(unix)]
const BUSY: &str = "another direct-mail operation holds this mailbox";
#[cfg(unix)]
const FILE_BYTES: usize = ROWS * (2 * TEXT * 6 + 2048);

#[derive(Debug)]
enum MailError {
    #[cfg(unix)]
    Busy,
    Failed(String),
}

impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Busy => f.write_str(BUSY),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl From<String> for MailError {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

impl From<&str> for MailError {
    fn from(why: &str) -> Self {
        Self::Failed(why.to_owned())
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Mailbox {
    next: u64,
    rows: Vec<Row>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Row {
    id: u64,
    node: String,
    key: String,
    kind: String,
    text: String,
    #[serde(default)]
    listed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply: Option<Reply>,
    #[serde(default)]
    reply_seen: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct Reply {
    key: String,
    kind: String,
    text: String,
}

/// The bounded, body-free snapshot read by presence and subscription readers. The private
/// mailbox remains authoritative; only command and restoration workers refresh this cache.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Metadata {
    pub high: u64,
    pub rows: Vec<Notice>,
}

/// A direct arrival's independent `fN` identity, never a broker offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub id: u64,
    pub from: String,
    pub kind: String,
    pub listed: bool,
    pub replied: bool,
}

#[cfg(unix)]
fn cache(ctx: &dyn FabricSession, boxed: &Mailbox) -> bool {
    let metadata = Metadata {
        high: boxed.next,
        rows: boxed
            .rows
            .iter()
            .map(|row| Notice {
                id: row.id,
                from: format!("fleet:{}", row.node),
                kind: row.kind.clone(),
                listed: row.listed,
                replied: row.reply.is_some(),
            })
            .collect(),
    };
    let mut inbox = ctx.fabric().lock();
    if inbox.direct_mail != metadata {
        inbox.direct_mail = metadata;
        true
    } else {
        false
    }
}

/// Refresh discovery after adoption or a write by a handoff peer, only on a worker.
/// False means this observation failed or a writer holds the lock; retry its file stamp.
/// Missing or damaged storage does not affect broker mail or session startup.
pub fn restore(ctx: &dyn FabricSession) -> bool {
    if ctx.fleet_mail_path().is_some() {
        #[cfg(unix)]
        return transaction_within(ctx, std::time::Duration::ZERO, |_| Ok(((), false))).is_ok();
    }
    true
}

fn kind(word: &str) -> bool {
    matches!(word, "ask" | "task" | "note" | "answer" | "report")
}

fn id(word: &str) -> Option<u64> {
    word.strip_prefix('f')?.parse().ok().filter(|n| *n > 0)
}

fn valid_node(node: &str) -> bool {
    node.len() == 32
        && node
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(unix)]
fn validated(boxed: &Mailbox) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    let mut keys = std::collections::BTreeSet::new();
    boxed.rows.len() <= ROWS
        && boxed.rows.iter().all(|row| {
            row.id > 0
                && row.id <= boxed.next
                && ids.insert(row.id)
                && keys.insert((&row.node, &row.key))
                && valid_node(&row.node)
                && super::valid_key(&row.key)
                && kind(&row.kind)
                && !row.text.is_empty()
                && row.text.len() <= TEXT
                && (!row.reply_seen || row.reply.is_some())
                && row.reply.as_ref().is_none_or(|reply| {
                    super::valid_key(&reply.key)
                        && kind(&reply.kind)
                        && !reply.text.is_empty()
                        && reply.text.len() <= TEXT
                })
        })
}

fn transaction<T>(
    ctx: &dyn FabricSession,
    edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
) -> Result<(T, bool), String> {
    transaction_within(ctx, std::time::Duration::from_secs(60), edit).map_err(|why| why.to_string())
}

#[cfg(unix)]
fn transaction_within<T>(
    ctx: &dyn FabricSession,
    within: std::time::Duration,
    edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
) -> Result<(T, bool), MailError> {
    transaction_waiting(ctx, within, edit, || {})
}

#[cfg(unix)]
fn transaction_waiting<T>(
    ctx: &dyn FabricSession,
    within: std::time::Duration,
    edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
    waiting: impl FnMut(),
) -> Result<(T, bool), MailError> {
    use aterm_uds::private_file::{PrivateFileError, PrivateLock};
    let path = ctx
        .fleet_mail_path()
        .ok_or("direct fleet mail has no private storage")?;
    let dir = path.parent().ok_or("direct fleet mail has no directory")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid mailbox name")?;
    aterm_uds::private_file::create_private_dir(dir).map_err(|e| e.to_string())?;
    let _lock = PrivateLock::take_with(dir, &format!("{name}.lock"), within, BUSY, waiting)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                MailError::Busy
            } else {
                MailError::Failed(e.to_string())
            }
        })?;
    let mut boxed = match aterm_uds::private_file::read_private(dir, name, FILE_BYTES) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
            aterm_toml::from_str::<Mailbox>(text)
                .map_err(|_| "direct fleet mailbox is malformed".to_owned())?
        }
        Err(PrivateFileError::Missing) => Mailbox::default(),
        Err(error) => return Err(error.to_string().into()),
    };
    if !validated(&boxed) {
        return Err("direct fleet mailbox is malformed".into());
    }
    let (result, changed) = edit(&mut boxed)?;
    if changed {
        let text = aterm_toml::to_string(&boxed).map_err(|e| e.to_string())?;
        if text.len() > FILE_BYTES {
            return Err("direct fleet mailbox is full".into());
        }
        aterm_uds::private_file::write_private(dir, name, text.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    // Refresh under the private lock so an older read cannot overwrite a newer write's cache.
    let cached = cache(ctx, &boxed);
    if changed || cached {
        self::changed(ctx);
    }
    Ok((result, changed))
}

#[cfg(not(unix))]
fn transaction_within<T>(
    _ctx: &dyn FabricSession,
    _within: std::time::Duration,
    _edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
) -> Result<(T, bool), MailError> {
    Err("direct fleet mail is available on Unix".into())
}

fn reply<T: AsRef<str>>(result: Result<T, String>) -> String {
    match result {
        Ok(text) => text.as_ref().to_owned(),
        Err(why) => format!(
            "ERR fleet-mail: {}\n",
            why.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(1024)
                .collect::<String>()
        ),
    }
}

struct Post<'a> {
    key: &'a str,
    kind: &'a str,
    text: &'a str,
    re: Option<u64>,
}

fn parse_post(rest: &str, answering: bool) -> Result<Post<'_>, String> {
    let (options, text) = rest.split_once(" -- ").ok_or(
        "use key=<token> kind=<kind> -- <text>, or re=f<id> key=<token> kind=<kind> -- <text>",
    )?;
    let (mut key, mut message_kind, mut re) = (None, None, None);
    for word in options.split_whitespace() {
        if let Some(value) = word.strip_prefix("key=").filter(|v| super::valid_key(v)) {
            if key.replace(value).is_some() {
                return Err("duplicate key".to_owned());
            }
        } else if let Some(value) = word.strip_prefix("kind=").filter(|v| kind(v)) {
            if message_kind.replace(value).is_some() {
                return Err("duplicate kind".to_owned());
            }
        } else if let Some(value) = word.strip_prefix("re=").and_then(id).filter(|_| answering) {
            if re.replace(value).is_some() {
                return Err("duplicate reply id".to_owned());
            }
        } else {
            return Err("invalid direct-mail option".to_owned());
        }
    }
    if text.is_empty() || text.len() > TEXT {
        return Err("text must contain 1..4096 bytes".to_owned());
    }
    let (Some(key), Some(message_kind)) = (key, message_kind) else {
        return Err("direct mail requires key and kind".to_owned());
    };
    if answering != re.is_some() {
        return Err("a local reply requires re=f<id>".to_owned());
    }
    Ok(Post {
        key,
        kind: message_kind,
        text,
        re,
    })
}

/// Whether this local post addresses a retained direct message instead of the bus.
pub fn is_reply(rest: &str) -> bool {
    super::post_options(rest.split_whitespace()).any(|word| word.starts_with("re=f"))
}

/// Deliver from the proved remote node, or retain the local recipient's answer for that node.
/// Each retained key is idempotent: the same key with other bytes refuses, including after a restart.
pub fn post(ctx: &dyn FabricSession, rest: &str) -> String {
    let driver = serving_driver();
    if driver.as_ref().is_some_and(|d| !valid_node(&d.node)) {
        return "ERR denied: direct mail requires a proved fleet node\n".to_owned();
    }
    let parsed = match parse_post(rest, driver.is_none()) {
        Ok(post) => post,
        Err(why) => return reply::<String>(Err(why)),
    };
    let mut arrival = None;
    let mut answered = None;
    let result = transaction(ctx, |boxed| {
        if let Some(driver) = &driver {
            if let Some(row) = boxed
                .rows
                .iter()
                .find(|r| r.node == driver.node && r.key == parsed.key)
            {
                return if row.kind == parsed.kind && row.text == parsed.text {
                    Ok((format!("OK f{} dup=1\n", row.id), false))
                } else {
                    Err("that key already names another message".to_owned())
                };
            }
            if boxed.rows.len() >= ROWS
                || boxed.rows.iter().filter(|r| r.node == driver.node).count() >= PER_NODE
            {
                // Both participants have received these completed conversations. Reclaim
                // them only under pressure, after checking retained idempotency keys.
                boxed.rows.retain(|row| !row.reply_seen);
            }
            if boxed.rows.len() >= ROWS
                || boxed.rows.iter().filter(|r| r.node == driver.node).count() >= PER_NODE
            {
                return Err("direct mailbox is full; the recipient can retire listed conversations with inbox seen f<id> forget".to_owned());
            }
            boxed.next = boxed
                .next
                .checked_add(1)
                .ok_or("direct-mail id space is exhausted")?;
            let id = boxed.next;
            arrival = Some((id, format!("fleet:{}", driver.node)));
            boxed.rows.push(Row {
                id,
                node: driver.node.clone(),
                key: parsed.key.to_owned(),
                kind: parsed.kind.to_owned(),
                text: parsed.text.to_owned(),
                listed: false,
                reply: None,
                reply_seen: false,
            });
            Ok((format!("OK f{id}\n"), true))
        } else {
            let id = parsed.re.ok_or("a local reply requires re=f<id>")?;
            let row = boxed
                .rows
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or("no such direct message")?;
            if let Some(was) = &row.reply {
                return if was.key == parsed.key
                    && was.kind == parsed.kind
                    && was.text == parsed.text
                {
                    Ok((format!("OK f{id} reply=1 dup=1\n"), false))
                } else {
                    Err("this message already has another reply".to_owned())
                };
            }
            row.reply = Some(Reply {
                key: parsed.key.to_owned(),
                kind: parsed.kind.to_owned(),
                text: parsed.text.to_owned(),
            });
            row.listed = true;
            answered = Some(id);
            Ok((format!("OK f{id} reply=1\n"), true))
        }
    });
    if result.is_ok()
        && let Some((id, from)) = arrival
    {
        ctx.record_fact(
            "mail",
            super::FabricFact::FleetMailIn {
                id,
                from: &from,
                kind: parsed.kind,
            },
        );
    }
    if result.is_ok()
        && let Some(id) = answered
    {
        ctx.record_fact("mail-seen", super::FabricFact::FleetMailSeen { id });
    }
    finish(ctx, result)
}

fn finish(_ctx: &dyn FabricSession, result: Result<(String, bool), String>) -> String {
    reply(result.map(|(text, _)| text))
}

#[cfg(unix)]
fn changed(ctx: &dyn FabricSession) {
    let guard = ctx.fabric().lock();
    ctx.fabric()
        .direct_epoch
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    ctx.fabric().changed.notify_all();
    drop(guard);
    ctx.fabric_changed();
}

fn render(row: &Row, meta: bool) -> String {
    let mut line = format!(
        "msg f{} from=fleet:{} kind={} trust=agent key={} len={} reply={} reply_seen={}",
        row.id,
        row.node,
        row.kind,
        row.key,
        row.text.len(),
        u8::from(row.reply.is_some()),
        u8::from(row.reply_seen)
    );
    if !meta {
        line.push_str(&format!(
            " text={}",
            aterm_control::wire::pct_encode(&row.text)
        ));
    }
    if let Some(answer) = &row.reply {
        line.push_str(&format!(
            " reply_kind={} reply_len={}",
            answer.kind,
            answer.text.len()
        ));
        if !meta {
            line.push_str(&format!(
                " reply_text={}",
                aterm_control::wire::pct_encode(&answer.text)
            ));
        }
    }
    line.push('\n');
    line
}

/// One direct-mail page, including rows omitted by a normal inbox's shared row limit.
#[derive(Default)]
pub struct View {
    /// The rendered rows, with a header for a standalone direct read.
    pub text: String,
    /// Rows included in `text`.
    pub count: usize,
    /// Direct rows omitted by the page limit.
    pub pending: usize,
}

/// Format a direct-mail refusal without including untrusted parser excerpts.
pub fn refusal(why: String) -> String {
    reply::<String>(Err(why))
}

/// Remote reads are restricted to the authenticated node; local reads see every sender.
pub fn inbox(ctx: &dyn FabricSession, rest: &str, combined: bool) -> String {
    reply(view(ctx, rest, combined).map(|page| page.text))
}

/// Read direct rows; `combined` appends unlisted arrivals to the normal inbox without
/// consuming a broker cursor. A standalone fleet inbox can reread retained conversations.
/// # Errors
/// Invalid options or unavailable or malformed private storage.
pub fn view(ctx: &dyn FabricSession, rest: &str, combined: bool) -> Result<View, String> {
    if combined && ctx.fleet_mail_path().is_none() {
        return Ok(View::default());
    }
    let driver = serving_driver();
    let (mut since, mut peek, mut meta) = (0, false, false);
    let mut limit = ROWS;
    for word in rest.split_whitespace() {
        if word == "--peek" {
            peek = true;
        } else if word == "--meta" {
            meta = true;
        } else if let Some(value) = word
            .strip_prefix("since=f")
            .and_then(|v| v.parse::<u64>().ok())
        {
            since = value;
        } else if let Ok(value) = word.parse::<usize>() {
            limit = value.min(ROWS);
        } else if combined && word.starts_with("since=") {
        } else {
            return Err("use inbox fleet [since=f<id>] [<n>] [--peek] [--meta]".to_owned());
        }
    }
    let result = transaction(ctx, |boxed| {
        let allowed = |r: &&Row| {
            driver.as_ref().is_none_or(|d| d.node == r.node)
                && r.id > since
                && (!combined || !r.listed)
        };
        let ids: std::collections::BTreeSet<u64> = boxed
            .rows
            .iter()
            .filter(allowed)
            .rev()
            .take(limit)
            .map(|r| r.id)
            .collect();
        let pending = boxed
            .rows
            .iter()
            .filter(allowed)
            .filter(|row| !row.listed && !ids.contains(&row.id))
            .count();
        let mut text = String::new();
        let mut moved = false;
        for row in &mut boxed.rows {
            if ids.contains(&row.id) {
                text.push_str(&render(row, meta));
                if driver.is_none() && !peek {
                    moved |= !row.listed;
                    row.listed = true;
                }
            }
        }
        if combined {
            return Ok((
                View {
                    text,
                    count: ids.len(),
                    pending,
                },
                moved,
            ));
        }
        let head = boxed
            .rows
            .iter()
            .filter(|r| driver.as_ref().is_none_or(|d| d.node == r.node))
            .map(|r| r.id)
            .max()
            .unwrap_or(0);
        Ok((
            View {
                text: format!("OK {} fleet=1 head=f{head}\n{text}", ids.len()),
                count: ids.len(),
                pending,
            },
            moved,
        ))
    });
    result.map(|(page, _)| page)
}

/// A sender explicitly acknowledges a reply it has read; no other node can acknowledge it.
pub fn acknowledge(ctx: &dyn FabricSession, word: &str) -> String {
    let Some(driver) = serving_driver() else {
        return "ERR denied: the sending node acknowledges its reply\n".to_owned();
    };
    let Some(id) = id(word.trim()) else {
        return "ERR usage: inbox seen f<id>\n".to_owned();
    };
    let result = transaction(ctx, |boxed| {
        let row = boxed
            .rows
            .iter_mut()
            .find(|r| r.id == id && r.node == driver.node)
            .ok_or("no such direct message")?;
        if row.reply.is_none() {
            return Err("this direct message has no reply to acknowledge".to_owned());
        }
        let moved = !row.reply_seen;
        row.reply_seen = true;
        Ok((format!("OK seen=f{id}\n"), moved))
    });
    finish(ctx, result)
}

/// The recipient may retire a conversation it has listed, without depending on the sender
/// remaining online to answer or acknowledge it. Unread messages cannot be discarded.
pub fn forget(ctx: &dyn FabricSession, word: &str) -> String {
    if serving_driver().is_some() {
        return "ERR denied: only the recipient retires direct mail\n".to_owned();
    }
    let Some(id) = id(word.trim()) else {
        return "ERR usage: inbox seen f<id> forget\n".to_owned();
    };
    let mut unread = false;
    let result = transaction(ctx, |boxed| {
        let row = boxed
            .rows
            .iter()
            .find(|r| r.id == id)
            .ok_or("no such direct message")?;
        if !row.listed {
            return Err("list this direct message before retiring it".to_owned());
        }
        unread = row.reply.is_none();
        boxed.rows.retain(|r| r.id != id);
        Ok((format!("OK forgot=f{id}\n"), true))
    });
    if result.is_ok() && unread {
        ctx.record_fact("mail-seen", super::FabricFact::FleetMailSeen { id });
    }
    finish(ctx, result)
}

/// The newest unlisted direct message past `after` a normal inbox wait may wake for.
/// # Errors
/// The private mailbox is unreadable or malformed. Contention is no observation this round;
/// the wait retries after notification or its periodic cross-process recheck.
pub fn pending(
    ctx: &dyn FabricSession,
    after: u64,
    accepts: impl Fn(&str) -> bool,
) -> Result<Option<u64>, String> {
    if serving_driver().is_some() || ctx.fleet_mail_path().is_none() {
        return Ok(None);
    }
    match transaction_within(ctx, std::time::Duration::ZERO, |boxed| {
        Ok((
            boxed
                .rows
                .iter()
                .filter(|r| r.id > after && !r.listed && accepts(&r.kind))
                .map(|r| r.id)
                .max(),
            false,
        ))
    }) {
        Ok((id, _)) => Ok(id),
        #[cfg(unix)]
        Err(MailError::Busy) => Ok(None),
        Err(why) => Err(why.to_string()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{Driver, DrivingAs, FabricFact, SessionFabric};
    use aterm_session::SessionId;

    struct Session {
        sid: SessionId,
        fabric: SessionFabric,
        path: std::path::PathBuf,
        facts: std::sync::Mutex<Vec<String>>,
        wakes: std::sync::atomic::AtomicU64,
    }

    impl FabricSession for Session {
        fn self_id(&self) -> &SessionId {
            &self.sid
        }
        fn fabric(&self) -> &SessionFabric {
            &self.fabric
        }
        fn record_fact(&self, _: &'static str, fact: FabricFact<'_>) -> bool {
            self.facts
                .lock()
                .unwrap()
                .push(crate::test_support::said(fact));
            false
        }
        fn fabric_changed(&self) {
            self.wakes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn holder_token(&self) -> Option<String> {
            None
        }
        fn fleet_mail_path(&self) -> Option<std::path::PathBuf> {
            Some(self.path.clone())
        }
    }

    fn session(path: &std::path::Path) -> Session {
        Session {
            sid: SessionId::new("s-12345678"),
            fabric: SessionFabric::default(),
            path: path.to_owned(),
            facts: Default::default(),
            wakes: Default::default(),
        }
    }

    fn as_node<T>(byte: &str, f: impl FnOnce() -> T) -> T {
        let _driver = DrivingAs::enter(Driver {
            node: byte.repeat(32),
            host: "peer.local".to_owned(),
        });
        f()
    }

    #[test]
    fn direct_arrival_discovers_once_without_reading_storage_on_presence_or_subscription() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(
                &receiver,
                "key=notice kind=ask -- PRIVATE_BODY"
            )),
            "OK f1\n"
        );
        assert_eq!(receiver.fabric.mail_facts().unread, 1);
        assert_eq!(receiver.fabric.mail_facts().head, 1);
        assert_eq!(
            receiver.fabric.mail_facts().last.unwrap().from,
            format!("fleet:{}", "a".repeat(32))
        );
        let (high, rows) = receiver
            .fabric
            .direct_mail_since(0, &crate::MailFilter::default());
        assert_eq!(high, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 1);
        assert_eq!(receiver.facts.lock().unwrap().len(), 1);
        assert!(!receiver.facts.lock().unwrap()[0].contains("PRIVATE_BODY"));
        assert_eq!(receiver.wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            as_node("a", || post(
                &receiver,
                "key=notice kind=ask -- PRIVATE_BODY"
            )),
            "OK f1 dup=1\n"
        );
        assert_eq!(receiver.facts.lock().unwrap().len(), 1);
        assert_eq!(receiver.wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            receiver
                .fabric
                .direct_mail_since(1, &crate::MailFilter::default())
                .1
                .is_empty()
        );
        for filter in [
            crate::MailFilter {
                topic: Some("build".to_owned()),
                ..Default::default()
            },
            crate::MailFilter {
                from: Some("human".to_owned()),
                ..Default::default()
            },
            crate::MailFilter {
                kinds: vec!["note".to_owned()],
                ..Default::default()
            },
        ] {
            assert!(receiver.fabric.direct_mail_since(0, &filter).1.is_empty());
        }
        let successor = session(&path);
        restore(&successor);
        assert_eq!(successor.fabric.mail_facts().unread, 1);
        assert_eq!(
            successor.wakes.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(
            successor.facts.lock().unwrap().is_empty(),
            "restoring discovery must not repeat carried timeline marks"
        );
        assert_eq!(
            as_node("b", || post(
                &receiver,
                "key=later kind=note -- handoff arrival"
            )),
            "OK f2\n"
        );
        assert_eq!(
            successor.fabric.mail_facts().unread,
            1,
            "the other process has its own cache"
        );
        assert!(restore(&successor));
        assert_eq!(
            successor.fabric.mail_facts().unread,
            2,
            "a write after adoption is discovered on refresh"
        );
        assert_eq!(
            successor
                .fabric
                .direct_mail_since(1, &crate::MailFilter::default())
                .1[0]
                .id,
            2
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            successor.fabric.mail_facts().unread,
            2,
            "GUI reads only cached metadata"
        );
        assert_eq!(
            successor
                .fabric
                .direct_mail_since(0, &crate::MailFilter::default())
                .1
                .len(),
            2
        );
    }

    #[test]
    fn answering_or_retiring_direct_mail_clears_cached_pending_without_reusing_bus_cursors() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert_eq!(
            as_node("a", || post(&receiver, "key=one kind=ask -- question")),
            "OK f1\n"
        );
        let _ = crate::cmd_inbox(&receiver, "");
        assert_eq!(receiver.fabric.mail_facts().pending, 1);
        assert_eq!(
            post(&receiver, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1\n"
        );
        assert_eq!(receiver.fabric.mail_facts().unread, 0);
        assert_eq!(receiver.fabric.mail_facts().pending, 0);
        assert_eq!(receiver.fabric.last_msg_id(), 0);
        assert_eq!(
            as_node("a", || post(&receiver, "key=two kind=note -- note")),
            "OK f2\n"
        );
        let _ = crate::cmd_inbox(&receiver, "");
        assert_eq!(forget(&receiver, "f2"), "OK forgot=f2\n");
        assert_eq!(receiver.fabric.mail_facts().unread, 0);
        assert_eq!(receiver.fabric.last_direct_mail_id(), 2);
        assert_eq!(
            receiver
                .facts
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.contains("moved=1"))
                .count(),
            2
        );
    }

    #[test]
    fn a_failed_refresh_is_retryable_and_keeps_the_last_successful_snapshot() {
        use aterm_uds::private_file::PrivateLock;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=one kind=ask -- question")),
            "OK f1\n"
        );
        let successor = session(&path);
        let held = PrivateLock::take(
            path.parent().unwrap(),
            "inbox.toml.lock",
            std::time::Duration::ZERO,
            "test lock",
        )
        .unwrap();
        assert!(
            !restore(&successor),
            "a busy observation must not commit its stamp"
        );
        assert_eq!(successor.fabric.last_direct_mail_id(), 0);
        drop(held);
        assert!(restore(&successor));
        assert_eq!(successor.fabric.last_direct_mail_id(), 1);
        std::fs::write(&path, "not valid toml").unwrap();
        assert!(
            !restore(&successor),
            "malformed observations must not commit their stamp"
        );
        assert_eq!(successor.fabric.mail_facts().unread, 1);
    }

    #[test]
    fn a_peer_posts_and_reads_its_reply_without_reading_another_peers_mail() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert_eq!(
            as_node("a", || crate::cmd_post(
                &receiver,
                "key=one kind=ask -- build status?",
                None
            )),
            "OK f1\n"
        );
        assert_eq!(
            as_node("b", || crate::cmd_post(
                &receiver,
                "key=two kind=note -- unrelated",
                None
            )),
            "OK f2\n"
        );
        let read = as_node("a", || crate::cmd_inbox_verb(&receiver, ""));
        assert!(read.contains("build%20status?"), "{read}");
        assert!(!read.contains("unrelated"), "{read}");
        let local = crate::cmd_inbox(&receiver, "");
        assert!(
            local.contains("msg f1 ") && local.contains("msg f2 "),
            "{local}"
        );
        assert_eq!(
            crate::cmd_post(&receiver, "re=f1 key=answer kind=answer -- complete", None),
            "OK f1 reply=1\n"
        );
        let reply = as_node("a", || crate::cmd_inbox_verb(&receiver, ""));
        assert!(reply.contains("reply_text=complete"), "{reply}");
        assert!(!as_node("b", || crate::cmd_inbox_verb(&receiver, "")).contains("complete"));
    }

    #[test]
    fn durable_keys_and_replies_survive_a_successor_using_the_same_session_path() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let before = session(&path);
        assert_eq!(
            as_node("a", || post(&before, "key=k kind=ask -- question")),
            "OK f1\n"
        );
        let successor = session(&path);
        assert_eq!(
            as_node("a", || post(&successor, "key=k kind=ask -- question")),
            "OK f1 dup=1\n"
        );
        assert!(as_node("a", || post(&successor, "key=k kind=ask -- changed")).starts_with("ERR "));
        assert_eq!(
            post(&successor, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1\n"
        );
        let again = session(&path);
        assert_eq!(
            post(&again, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1 dup=1\n"
        );
        assert!(as_node("a", || inbox(&again, "", false)).contains("reply_text=answer"));
    }

    #[test]
    fn no_local_sender_forges_a_peer_and_no_peer_replies_or_retires_another_conversation() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert!(post(&receiver, "key=k kind=ask -- forged").starts_with("ERR "));
        assert!(
            as_node("a", || post(
                &receiver,
                "from=other key=k kind=ask -- forged"
            ))
            .starts_with("ERR ")
        );
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- question")),
            "OK f1\n"
        );
        assert!(
            as_node("b", || post(&receiver, "re=f1 key=r kind=answer -- forged"))
                .starts_with("ERR ")
        );
        assert!(as_node("b", || forget(&receiver, "f1")).starts_with("ERR denied"));
        assert!(forget(&receiver, "f1").starts_with("ERR "));
    }

    #[test]
    fn bounded_mailbox_refuses_without_losing_waiting_messages_and_wakes_normal_inbox_waits() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        for n in 0..PER_NODE {
            assert!(
                as_node("a", || post(
                    &receiver,
                    &format!("key=k{n} kind=ask -- question")
                ))
                .starts_with("OK ")
            );
        }
        assert!(
            as_node("a", || post(&receiver, "key=overflow kind=ask -- question"))
                .starts_with("ERR ")
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 0 fleet=f16\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0", "fleet_since=f16"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=f16"], 0),
            "OK timeout\n"
        );
        assert!(crate::cmd_inbox(&receiver, "").contains("msg f16 "));
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            post(&receiver, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1\n"
        );
        assert!(as_node("b", || acknowledge(&receiver, "f1")).starts_with("ERR "));
        assert_eq!(
            as_node("a", || acknowledge(&receiver, "f1")),
            "OK seen=f1\n"
        );
        assert_eq!(forget(&receiver, "f1"), "OK forgot=f1\n");
        assert_eq!(
            as_node("a", || post(&receiver, "key=after kind=note -- after")),
            "OK f17\n"
        );
    }

    #[test]
    fn damaged_durable_state_refuses_and_is_not_replaced() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        as_node("a", || post(&receiver, "key=k kind=ask -- question"));
        assert_eq!(
            crate::deliver_row(
                &receiver,
                &[
                    "off=8",
                    "from=s-a@n-a",
                    "kind=ask",
                    "trust=agent",
                    "text=bus"
                ],
                &|| {}
            ),
            "OK 1\n"
        );
        std::fs::write(&receiver.path, b"other-peer-private-message = [").unwrap();
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 1\n"
        );
        assert!(crate::cmd_await_inbox(&receiver, &["since=f0"], 0).starts_with("ERR "));
        let page = crate::cmd_inbox(&receiver, "");
        assert!(
            page.starts_with("OK 1 ") && page.contains("fleet_error=unavailable"),
            "{page}"
        );
        assert!(page.contains("text=bus"), "{page}");
        assert!(
            receiver.fabric.lock().rows[0].listed,
            "unrelated damaged direct storage does not block bus delivery"
        );
        let refused = as_node("a", || post(&receiver, "key=k2 kind=ask -- question"));
        assert!(
            refused.starts_with("ERR ") && !refused.contains("other-peer-private-message"),
            "{refused}"
        );
        assert_eq!(
            std::fs::read(&receiver.path).unwrap(),
            b"other-peer-private-message = ["
        );
    }

    #[test]
    fn bus_and_direct_cursors_can_be_rearmed_independently() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert_eq!(
            crate::deliver_row(
                &receiver,
                &[
                    "off=8",
                    "from=s-a@n-a",
                    "kind=ask",
                    "trust=agent",
                    "text=bus"
                ],
                &|| {}
            ),
            "OK 1\n"
        );
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- direct")),
            "OK f1\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 1\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=1"], 0),
            "OK inbox 1 fleet=f1\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=1", "fleet_since=f1"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=f1"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            crate::deliver_row(
                &receiver,
                &[
                    "off=9",
                    "from=s-a@n-a",
                    "kind=ask",
                    "trust=agent",
                    "text=next"
                ],
                &|| {}
            ),
            "OK 2\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=1", "fleet_since=f1"], 0),
            "OK inbox 2\n"
        );
    }

    #[test]
    fn a_small_inbox_page_keeps_unreturned_bus_and_direct_rows_pending() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        crate::deliver_row(
            &receiver,
            &[
                "off=8",
                "from=s-a@n-a",
                "kind=ask",
                "trust=agent",
                "text=bus",
            ],
            &|| {},
        );
        as_node("a", || post(&receiver, "key=one kind=ask -- direct-one"));
        as_node("a", || post(&receiver, "key=two kind=ask -- direct-two"));
        let empty = crate::cmd_inbox(&receiver, "0");
        assert!(
            empty.starts_with("OK 0 ") && empty.contains("pending=3"),
            "{empty}"
        );
        let page = crate::cmd_inbox(&receiver, "1");
        assert!(
            page.starts_with("OK 1 ") && page.contains("pending=2"),
            "{page}"
        );
        assert!(
            page.contains("msg f2 ") && !page.contains("msg 1 "),
            "{page}"
        );
        assert!(!receiver.fabric.lock().rows[0].listed);
        assert!(crate::cmd_inbox(&receiver, "1").contains("msg f1 "));
        let bus = crate::cmd_inbox(&receiver, "1");
        assert!(
            bus.contains("msg 1 "),
            "retained listed direct rows cannot starve bus mail: {bus}"
        );
    }

    #[test]
    fn a_busy_durable_mailbox_waits_for_the_writer_without_losing_its_transaction() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        let dir = receiver.path.parent().unwrap();
        aterm_uds::private_file::create_private_dir(dir).unwrap();
        let held = aterm_uds::private_file::PrivateLock::take(
            dir,
            "inbox.toml.lock",
            std::time::Duration::ZERO,
            "busy",
        )
        .unwrap();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
            let receiver = &receiver;
            scope.spawn(move || {
                let mut waiting_tx = Some(waiting_tx);
                let result = transaction_waiting(
                    receiver,
                    aterm_spec::wait::MUST_HAPPEN,
                    |boxed| {
                        boxed.next += 1;
                        Ok((boxed.next, true))
                    },
                    || {
                        if let Some(tx) = waiting_tx.take() {
                            tx.send(()).unwrap();
                        }
                    },
                );
                tx.send(result).unwrap()
            });
            waiting_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
            assert!(rx.try_recv().is_err(), "a held mailbox cannot be edited");
            assert_eq!(
                crate::cmd_await_inbox(receiver, &["since=0"], 0),
                "OK timeout\n"
            );
            assert_eq!(
                crate::cmd_await_inbox(receiver, &["since=f0"], 0),
                "OK timeout\n"
            );
            drop(held);
            assert_eq!(
                rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap()
                    .unwrap(),
                (1, true)
            );
        });
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- question")),
            "OK f2\n"
        );
    }

    #[test]
    fn a_recipient_can_retire_read_mail_even_if_the_sender_never_returns() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        as_node("a", || post(&receiver, "key=k kind=note -- received"));
        assert!(forget(&receiver, "f1").starts_with("ERR "));
        inbox(&receiver, "--peek", false);
        assert!(forget(&receiver, "f1").starts_with("ERR "));
        inbox(&receiver, "", false);
        assert_eq!(forget(&receiver, "f1"), "OK forgot=f1\n");
        assert_eq!(
            as_node("a", || post(&receiver, "key=next kind=note -- next")),
            "OK f2\n"
        );
    }

    #[test]
    fn a_lost_inbox_response_can_be_recovered_from_the_retained_fleet_inbox() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        as_node("a", || post(&receiver, "key=k kind=ask -- retained"));
        assert!(crate::cmd_inbox(&receiver, "").contains("msg f1 "));
        assert!(crate::cmd_inbox_verb(&receiver, "fleet").contains("msg f1 "));
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK timeout\n"
        );
    }

    #[test]
    fn a_full_mailbox_reclaims_acknowledged_replies_without_a_manual_cleanup() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        for n in 0..PER_NODE {
            as_node("a", || {
                post(&receiver, &format!("key=k{n} kind=ask -- question"))
            });
        }
        post(&receiver, "re=f1 key=r kind=answer -- complete");
        as_node("a", || acknowledge(&receiver, "f1"));
        assert_eq!(
            as_node("a", || post(&receiver, "key=next kind=note -- next")),
            "OK f17\n"
        );
        let page = inbox(&receiver, "", false);
        assert!(!page.contains("msg f1 "), "{page}");
        assert!(
            page.contains("msg f2 ") && page.contains("msg f17 "),
            "{page}"
        );
    }
}
