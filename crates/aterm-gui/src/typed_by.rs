// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO TYPED: the last control client to type into a session, and when.

use std::sync::{Arc, Mutex, OnceLock};

use aterm_session::SessionId;

use crate::SessionCtx;

/// A process lineage is read at most this deep (`consent_observer::
/// session_of_pid`'s bound).
const LINEAGE_MAX: usize = 32;

/// The words aterm credits typing by on its own ([`Who::own_words`]) beside
/// `aterm`, which the session log's actor words already hold.
const CREDIT_WORDS: &[&str] = &["the message bus", "a script"];

/// The words a drive from another Mac's credit ends in (the step-in):
/// `<that Mac> over the fleet` ([`fleet_credit`]).
const FLEET_CREDIT: &str = "over the fleet";

/// The credit of what another Mac did here over the fleet's drive.
pub(crate) fn fleet_credit(who: &str) -> String {
    format!("{who} {FLEET_CREDIT}")
}

/// Whether `name` is a word aterm credits typing by, one of the session
/// log's actors (`aterm`, `human`, `harness`, …), or a name that ends as a
/// drive's credit does ([`fleet_credit`]): a tab named one of those would
/// read as aterm's own credit, or as another Mac's drive, so no tab is
/// named by it.
fn is_credit_word(name: &str) -> bool {
    let name = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    aterm_slog::is_reserved_actor_word(&name)
        || CREDIT_WORDS.contains(&name.as_str())
        || name.ends_with(FLEET_CREDIT)
}

/// Who a control connection is, as the typing it sends is credited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Who {
    /// aterm itself: the in-process harness's token, or a peer pid that is this process.
    Aterm,
    /// The fabric bridge's connection.
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "the Unix fabric connection constructs it; shared caller naming stays portable"
        )
    )]
    Bridge,
    /// An edge-scoped connection.
    Edge { src: SessionId, client: Box<Who> },
    /// An owner-token client.
    Process {
        pid: u32,
        comm: Option<String>,
        lineage: Vec<(i32, Option<i32>)>,
    },
    /// A caller its own lane names in words, shown as given.
    Word(String),
}

impl Who {
    /// An owner-token client whose connection's peer pid is `pid`.
    pub(crate) fn client(pid: Option<u32>) -> Self {
        Self::client_with(
            pid,
            std::process::id(),
            &crate::session_status::program::parent_of,
            &crate::consent_observer::live_pgid_of,
            &crate::quit_safety::process_name,
        )
    }

    /// [`Self::client`] with the process facts injected.
    pub(crate) fn client_with(
        pid: Option<u32>,
        own_pid: u32,
        parent_of: &dyn Fn(i32) -> Option<i32>,
        pgid_of: &dyn Fn(i32) -> Option<i32>,
        comm_of: &dyn Fn(i32) -> Option<String>,
    ) -> Self {
        let Some(pid) = pid else {
            return Self::Word("a script".to_string());
        };
        if pid == own_pid {
            return Self::Aterm;
        }
        let mut lineage = Vec::new();
        let mut at = i32::try_from(pid).ok();
        while let Some(p) = at.filter(|&p| p > 1 && lineage.len() < LINEAGE_MAX) {
            lineage.push((p, pgid_of(p)));
            at = parent_of(p).filter(|&parent| parent != p);
        }
        Self::Process {
            pid,
            comm: i32::try_from(pid).ok().and_then(comm_of),
            lineage,
        }
    }

    /// Whether `other` is another process of the same name under the same
    /// line of parents (over the same edge): what `aterm ctl` run again from
    /// one shell is, a new process and connection per verb.
    fn sibling_of(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Process { comm, lineage, .. },
                Self::Process {
                    comm: other_comm,
                    lineage: other_lineage,
                    ..
                },
            ) => {
                comm == other_comm
                    && lineage.len() > 1
                    && lineage.get(1..) == other_lineage.get(1..)
            }
            (
                Self::Edge { src, client },
                Self::Edge {
                    src: other_src,
                    client: other_client,
                },
            ) => src == other_src && client.sibling_of(other_client),
            _ => false,
        }
    }

    /// What it says of itself, before (or without) the main thread naming it.
    fn own_words(&self) -> String {
        match self {
            Self::Aterm => "aterm".to_string(),
            Self::Bridge => "the message bus".to_string(),
            Self::Edge { src, client } => format!(
                "{} on {}'s edge",
                client.own_words(),
                aterm_messages::presence::short_sid(src.as_str())
            ),
            Self::Process {
                pid,
                comm: Some(comm),
                ..
            } => format!("{comm} (pid {pid})"),
            Self::Process { pid, .. } => format!("pid {pid}"),
            Self::Word(word) => word.clone(),
        }
    }

    /// Its name unresolved: its own words, which the survey's facts carry
    /// only where aterm gave them (a process's name is its program's).
    fn own_name(&self) -> Named {
        Named {
            words: self.own_words(),
            facts: match self {
                Self::Edge { .. } | Self::Process { .. } => None,
                Self::Aterm | Self::Bridge | Self::Word(_) => Some(self.own_words()),
            },
            by_parent: false,
        }
    }
}

/// What a caller is called ([`name`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Named {
    /// Every reader's: the hover, the Overview, `status`, the survey's content.
    pub(crate) words: String,
    /// For the survey's facts, which carry no text a person or a program wrote.
    pub(crate) facts: Option<String>,
    /// A parent's tab named it, so another process of its name under that parent is named the same.
    pub(crate) by_parent: bool,
}

/// One connection's caller and, once the main thread named it, its name.
#[derive(Debug)]
pub(crate) struct Caller {
    pub(crate) who: Who,
    named: OnceLock<Named>,
}

impl Caller {
    pub(crate) fn new(who: Who) -> Arc<Self> {
        Arc::new(Self {
            who,
            named: OnceLock::new(),
        })
    }

    /// A caller already named `named`, as the main thread would name it.
    #[cfg(test)]
    pub(crate) fn named(who: Who, named: Named) -> Arc<Self> {
        Arc::new(Self {
            who,
            named: OnceLock::from(named),
        })
    }

    /// Its name: the main thread's ([`App::typed_by`](crate::App::typed_by)), else its own words.
    pub(crate) fn words(&self) -> String {
        self.named
            .get()
            .map_or_else(|| self.who.own_words(), |named| named.words.clone())
    }

    /// Its name for the survey's facts ([`Named::facts`]).
    pub(crate) fn facts(&self) -> Option<String> {
        match self.named.get() {
            Some(named) => named.facts.clone(),
            None => self.who.own_name().facts,
        }
    }

    /// Whether typing by `other` says what this caller's says, so its stamp
    /// keeps this caller and its name: the same caller, or a process of its
    /// name under the parent whose tab named it.
    fn covers(&self, other: &Who) -> bool {
        self.who == *other
            || (self.named.get().is_some_and(|named| named.by_parent) && self.who.sibling_of(other))
    }
}

/// The last typing into a session.
#[derive(Clone, Debug)]
pub(crate) struct Typed {
    pub(crate) by: Arc<Caller>,
    pub(crate) at_unix_ms: u64,
}

/// `SessionCtx.typed`: the session's last [`Typed`].
#[derive(Clone, Debug, Default)]
pub(crate) struct TypedStamp(Arc<Mutex<Option<Typed>>>);

impl TypedStamp {
    /// Credit typing at `at_unix_ms` to `by`.
    pub(crate) fn stamp(&self, by: Arc<Caller>, at_unix_ms: u64) -> bool {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let (by, moved) = match slot.take() {
            Some(last) if last.by.covers(&by.who) => {
                (last.by, last.at_unix_ms / 60_000 != at_unix_ms / 60_000)
            }
            _ => (by, true),
        };
        *slot = Some(Typed { by, at_unix_ms });
        moved
    }

    pub(crate) fn last(&self) -> Option<Typed> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Credit the accepted event to the captured session after all timeline locks are released.
    pub(crate) fn record(&self, session: &SessionId, by: Arc<Caller>) {
        let now = aterm_time::unix_millis().unwrap_or(0);
        if self.stamp(by, now) {
            crate::presence::post_typed(session);
        }
    }
}

/// THE STAMP an in-process paste makes on its target.
pub(crate) fn stamp(ctx: &SessionCtx, by: Arc<Caller>) {
    ctx.typed.record(&ctx.self_id, by);
}

/// `typed_by=<pct|-> typed_ms=<ms|->`, the `status` pair.
pub(crate) fn wire(typed: Option<&Typed>, now_unix_ms: u64) -> String {
    match typed {
        Some(t) => format!(
            "typed_by={} typed_ms={}",
            aterm_control::wire::pct_encode(&t.by.words()),
            now_unix_ms.saturating_sub(t.at_unix_ms)
        ),
        None => "typed_by=- typed_ms=-".to_string(),
    }
}

/// The tab whose shell is the process of `lineage`, its process group or an
/// ancestor (`shells`, `(local id, shell pid)`), and how far up it was found
/// (0 the process itself).
fn shell_tab(lineage: &[(i32, Option<i32>)], shells: &[(u64, i32)]) -> Option<(usize, u64)> {
    lineage.iter().enumerate().find_map(|(up, &(pid, pgid))| {
        crate::consent_observer::owning_session(pid, pgid, shells).map(|tab| (up, tab))
    })
}

/// THE NAME of `who` (main thread).
pub(crate) fn name(
    who: &Who,
    shells: &[(u64, i32)],
    local_of: &dyn Fn(&SessionId) -> Option<u64>,
    tab_of: &dyn Fn(u64) -> Option<Named>,
) -> Named {
    match who {
        Who::Process { lineage, .. } => shell_tab(lineage, shells)
            .and_then(|(up, tab)| {
                tab_of(tab).map(|named| Named {
                    by_parent: up > 0,
                    ..named
                })
            })
            .unwrap_or_else(|| who.own_name()),
        Who::Edge { src, client } => {
            let by = name(client, shells, local_of, tab_of);
            let src_tab = local_of(src);
            let client_tab = match client.as_ref() {
                Who::Process { lineage, .. } => shell_tab(lineage, shells).map(|(_, tab)| tab),
                _ => None,
            };
            if src_tab.is_some() && src_tab == client_tab {
                // The edge's own tab typing through it.
                return by;
            }
            let on = src_tab.and_then(tab_of).map_or_else(
                || aterm_messages::presence::short_sid(src.as_str()),
                |named| named.words,
            );
            Named {
                words: format!("{} on {on}'s edge", by.words),
                facts: None,
                by_parent: by.by_parent,
            }
        }
        _ => who.own_name(),
    }
}

impl crate::App {
    /// Who last typed into `ctx`'s session and when, its caller named here once ([`name`]).
    pub(crate) fn typed_by(&self, ctx: &SessionCtx) -> Option<Typed> {
        let typed = ctx.typed.last()?;
        typed.by.named.get_or_init(|| {
            let shells: Vec<(u64, i32)> = self.pool.iter().map(|s| (s.id, s.pid)).collect();
            name(
                &typed.by.who,
                &shells,
                &|sid| self.local_session_of(sid),
                &|local| self.typist_tab(local),
            )
        });
        Some(typed)
    }

    /// A tab as it names who typed from it ([`name`]).
    pub(crate) fn typist_tab(&self, local: u64) -> Option<Named> {
        let s = self.pool.get(local)?;
        let set = |t: &str| {
            Some(crate::session_timeline::sanitize_presentation_line(t, 40))
                .filter(|t| !t.is_empty() && !is_credit_word(t))
        };
        let (role, pin) = {
            let meta = s.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            (
                meta.role.as_deref().and_then(set),
                meta.presentation_value("title").as_deref().and_then(set),
            )
        };
        let sid = aterm_messages::presence::short_sid(s.ctx.self_id.as_str());
        Some(match (role, pin) {
            (Some(role), _) => Named {
                facts: Some(role.clone()),
                words: role,
                by_parent: false,
            },
            (None, Some(pin)) => Named {
                words: pin,
                facts: Some(sid),
                by_parent: false,
            },
            (None, None) => Named {
                words: sid.clone(),
                facts: Some(sid),
                by_parent: false,
            },
        })
    }

    /// The hover's line: `last typed into by lab-sweep at 14:02`.
    pub(crate) fn typed_hover_line(&self, ctx: &SessionCtx) -> Option<String> {
        let typed = self.typed_by(ctx)?;
        let at = i64::try_from(typed.at_unix_ms / 1000)
            .ok()
            .and_then(crate::status_item::clock_words)
            .map_or_else(String::new, |clock| format!(" at {clock}"));
        Some(format!("last typed into by {}{at}", typed.by.words()))
    }

    /// The `Wake::Typed` arm: name the caller and push the tab's hover again.
    pub(crate) fn on_typed(&mut self, sid: &SessionId) {
        let Some(local) = self.local_session_of(sid) else {
            return;
        };
        if let Some(ctx) = self.pool.get(local).map(|s| Arc::clone(&s.ctx)) {
            let _ = self.typed_by(&ctx);
        }
        self.refresh_meta_dependent_chrome(local);
    }

    /// The `Wake::Driven` arm.
    pub(crate) fn on_driven(&mut self, sid: &SessionId, host: &str, act: &str) {
        let tab = self.local_session_of(sid).map_or_else(
            || sid.as_str().to_owned(),
            |local| self.tab_label_of_session(local),
        );
        let _ = self.post_message(crate::message_reporters::driven_over_fleet(host, act, &tab));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 900 and 901 ← 800 ← 700 (a shell, pid 700) ← 1, each its own group
    /// but 800.
    fn parent(pid: i32) -> Option<i32> {
        match pid {
            900 | 901 => Some(800),
            800 => Some(700),
            700 => Some(1),
            _ => None,
        }
    }

    fn group(pid: i32) -> Option<i32> {
        Some(if pid == 800 { 650 } else { pid })
    }

    fn comm(pid: i32) -> Option<String> {
        matches!(pid, 900 | 901).then(|| "python3".to_string())
    }

    fn client(pid: u32) -> Who {
        Who::client_with(Some(pid), 42, &parent, &group, &comm)
    }

    /// Tab `local` names a typist `tab-<local>`, `s-<local>` at facts; the
    /// edge's session is tab 3.
    fn named(who: &Who, shells: &[(u64, i32)]) -> Named {
        let sid = SessionId::new("s-0123456789abcdef");
        name(who, shells, &|s| (*s == sid).then_some(3), &|local| {
            Some(Named {
                words: format!("tab-{local}"),
                facts: Some(format!("s-{local}")),
                by_parent: false,
            })
        })
    }

    #[test]
    fn a_client_is_read_while_connected_and_named_by_its_tab() {
        let who = client(900);
        assert_eq!(
            who,
            Who::Process {
                pid: 900,
                comm: Some("python3".to_string()),
                lineage: vec![(900, Some(900)), (800, Some(650)), (700, Some(700))],
            },
            "the lineage stops before pid 1"
        );
        let by_shell = named(&who, &[(5, 700)]);
        assert_eq!(
            (by_shell.words.as_str(), by_shell.by_parent),
            ("tab-5", true)
        );
        assert_eq!(by_shell.facts.as_deref(), Some("s-5"));
        assert_eq!(named(&who, &[(6, 650)]).words, "tab-6", "a process group");
        assert!(
            !named(&who, &[(8, 900)]).by_parent,
            "the shell is the process itself"
        );
        let alone = named(&who, &[(7, 4242)]);
        assert_eq!(alone.words, "python3 (pid 900)", "no tab");
        assert_eq!(alone.facts, None, "a program's name is no fact");
        assert_eq!(
            named(&client(42), &[(5, 700)]).words,
            "aterm",
            "this process"
        );
        let gone = Who::client_with(Some(77), 42, &|_| None, &|_| None, &|_| None);
        assert_eq!(named(&gone, &[]).words, "pid 77");
        let unread = Who::client_with(None, 42, &parent, &group, &comm);
        assert_eq!(named(&unread, &[]).words, "a script");
    }

    /// An edge names who presented its token, on the edge's tab, and that
    /// tab alone only when its own shell is the client.
    #[test]
    fn an_edge_is_who_presented_it_on_its_tab() {
        let edge = |sid: &str, client: Who| Who::Edge {
            src: SessionId::new(sid),
            client: Box::new(client),
        };
        let here = "s-0123456789abcdef";
        let own = named(&edge(here, client(900)), &[(3, 700)]);
        assert_eq!(own.words, "tab-3", "the edge's own tab typed");
        assert_eq!(own.facts.as_deref(), Some("s-3"));
        let other = named(&edge(here, client(900)), &[(5, 700)]);
        assert_eq!(other.words, "tab-5 on tab-3's edge");
        assert_eq!(other.facts, None);
        assert_eq!(
            named(&edge(here, client(900)), &[]).words,
            "python3 (pid 900) on tab-3's edge"
        );
        assert_eq!(
            named(&edge("s-fedcba9876543210", client(900)), &[]).words,
            "python3 (pid 900) on s-fedc's edge"
        );
        let unread = Who::client_with(None, 42, &parent, &group, &comm);
        assert_eq!(
            Caller::new(edge(here, unread)).words(),
            "a script on s-0123's edge",
            "unnamed"
        );
        assert_eq!(named(&Who::Bridge, &[(5, 700)]).words, "the message bus");
        let fleet = Who::Word("mini over the fleet".to_string());
        assert_eq!(named(&fleet, &[]).words, "mini over the fleet");
    }

    #[test]
    fn no_tab_takes_a_word_aterm_credits_by() {
        for word in [
            "aterm",
            "Aterm",
            "the message bus",
            "a script",
            "human",
            "harness",
            "mac-03 over the fleet",
            "MAC-03  Over the\tFLEET ",
            fleet_credit("mac-03.local (node 1a2b3c4d)").as_str(),
        ] {
            assert!(is_credit_word(word), "{word}");
        }
        for word in [
            "lab-sweep",
            "aterm-dev",
            "bus",
            "fleet",
            "over-the-fleet-bot",
        ] {
            assert!(!is_credit_word(word), "{word}");
        }
    }

    /// AN `invoke Paste` TYPES FOR ITS CALLER where its paste lands while the
    /// invoke runs (`App::invoke_typist`).
    #[test]
    #[cfg(unix)]
    fn an_invoked_paste_is_its_callers_typing() {
        let mut pipe = [0; 2];
        // SAFETY: `pipe` is two writable ints; the ends are this test's.
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "a pipe");
        let sink = Arc::new(aterm_session::sink::SinkWriter::new(pipe[1]));
        let mut app = crate::App::headless_for_test_with_sink(sink);
        let wid = crate::WindowId(0);
        let owner = app.focused_session_id(wid);
        let ctx = Arc::clone(
            &app.pool
                .get(owner.expect("a front session"))
                .expect("pooled")
                .ctx,
        );
        app.deliver_paste(wid, owner, "ls".to_string(), crate::input::Source::Human);
        assert!(ctx.typed.last().is_none(), "the person's paste");

        app.invoke_typist = Some(Caller::new(Who::Word("lab-sweep".to_string())));
        app.deliver_paste(wid, owner, "ls".to_string(), crate::input::Source::Human);
        let typed = ctx.typed.last().expect("the caller typed");
        assert_eq!(typed.by.words(), "lab-sweep");
        assert!(app.invoke_typist.is_none(), "taken by the paste it credits");
        // SAFETY: the read end is this test's, closed once.
        unsafe { libc::close(pipe[0]) };
    }

    #[test]
    fn a_stamp_owes_the_hover_only_a_new_caller_or_a_new_minute() {
        let stamp = TypedStamp::default();
        assert!(stamp.last().is_none());
        assert_eq!(wire(None, 0), "typed_by=- typed_ms=-");
        let first = Caller::named(client(900), named(&client(900), &[(5, 700)]));
        assert!(stamp.stamp(Arc::clone(&first), 120_000), "a first stamp");
        assert!(
            !stamp.stamp(Caller::new(client(900)), 150_000),
            "the same caller in the same minute"
        );
        let last = stamp.last().expect("stamped");
        assert!(Arc::ptr_eq(&last.by, &first), "it keeps its name");
        assert_eq!(last.at_unix_ms, 150_000);
        assert_eq!(wire(Some(&last), 154_000), "typed_by=tab-5 typed_ms=4000");
        assert!(
            !stamp.stamp(Caller::new(client(901)), 155_000),
            "`aterm ctl` again from the shell whose tab named it"
        );
        assert!(Arc::ptr_eq(&stamp.last().expect("stamped").by, &first));
        assert!(
            stamp.stamp(Caller::new(client(900)), 180_000),
            "a new minute"
        );
        assert!(
            stamp.stamp(Caller::new(Who::Bridge), 181_000),
            "a new caller"
        );
        let by_bus = stamp.last().expect("stamped");
        assert_eq!(
            wire(Some(&by_bus), 181_000),
            "typed_by=the%20message%20bus typed_ms=0"
        );

        let alone = TypedStamp::default();
        let by_pid = Caller::named(client(900), named(&client(900), &[]));
        assert!(alone.stamp(by_pid, 120_000));
        assert!(alone.stamp(Caller::new(client(901)), 121_000));
        assert!(
            alone.stamp(Caller::new(client(900)), 122_000),
            "an unnamed caller covers no sibling"
        );
    }
}
