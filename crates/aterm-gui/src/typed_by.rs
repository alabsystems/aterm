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

/// `SessionCtx.typed`: the session's last [`Typed`], and when a caller other than aterm's own
/// last typed ([`TypedStamp::driven_ms`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct TypedStamp(Arc<Mutex<Stamps>>);

/// What [`TypedStamp`] holds.
#[derive(Debug, Default)]
struct Stamps {
    /// The last typing.
    last: Option<Typed>,
    /// When a caller other than aterm's own ([`Who::is_aterms_own`]) last typed.
    driven_at_unix_ms: Option<u64>,
    /// A caller other than aterm's own was admitted to type here, set before its bytes are
    /// written ([`TypedStamp::admit`]) and never cleared.
    driver_admitted: bool,
}

impl Who {
    /// Whether typing by this caller is aterm's own: its harness's lines ([`Who::Aterm`]) and the
    /// message bus's deliveries ([`Who::Bridge`]), neither of which asks a program to end.
    const fn is_aterms_own(&self) -> bool {
        matches!(self, Self::Aterm | Self::Bridge)
    }
}

impl TypedStamp {
    /// Credit typing at `at_unix_ms` to `by`.
    pub(crate) fn stamp(&self, by: Arc<Caller>, at_unix_ms: u64) -> bool {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if !by.who.is_aterms_own() {
            slot.driven_at_unix_ms = Some(at_unix_ms);
            slot.driver_admitted = true;
        }
        let (by, moved) = match slot.last.take() {
            Some(last) if last.by.covers(&by.who) => {
                (last.by, last.at_unix_ms / 60_000 != at_unix_ms / 60_000)
            }
            _ => (by, true),
        };
        slot.last = Some(Typed { by, at_unix_ms });
        moved
    }

    /// Note that `by` was admitted to type here, before its bytes are written: `reveal` reads
    /// [`TypedStamp::driver_admitted`] from the admission on, so a process the write starts cannot
    /// ask before the driver counts.
    pub(crate) fn admit(&self, by: &Caller) {
        if !by.who.is_aterms_own() {
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .driver_admitted = true;
        }
    }

    /// Whether a caller other than aterm's own has ever been admitted to type into the session
    /// ([`TypedStamp::admit`], or a [`TypedStamp::stamp`] without one).
    pub(crate) fn driver_admitted(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .driver_admitted
    }

    pub(crate) fn last(&self) -> Option<Typed> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last
            .clone()
    }

    /// How long before `now_unix_ms` a control client other than aterm's own last typed into the
    /// session (none: none has): a driver's `/exit` stays the driver's whatever aterm's own lines
    /// or the message bus typed after it, which `typed_by=` (the last typing alone) would lose.
    pub(crate) fn driven_ms(&self, now_unix_ms: u64) -> Option<u64> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .driven_at_unix_ms
            .map(|at| now_unix_ms.saturating_sub(at))
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

/// The tab (session local id) of `who`'s process: the session in `shells` whose shell is that
/// process or its nearest ancestor ([`shell_tab`]). An edge's client is its tab's own; any other
/// caller is in no tab.
pub(crate) fn tab_session_of(who: &Who, shells: &[(u64, i32)]) -> Option<u64> {
    match who {
        Who::Process { lineage, .. } => shell_tab(lineage, shells).map(|(_, tab)| tab),
        Who::Edge { client, .. } => tab_session_of(client, shells),
        Who::Aterm | Who::Bridge | Who::Word(_) => None,
    }
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

/// A tab, `ctx`'s session, as it names who typed from it ([`name`]): its role, else its pinned
/// title, else its short id.
pub(crate) fn tab_named(ctx: &SessionCtx) -> Named {
    let set = |t: &str| {
        Some(crate::session_timeline::sanitize_presentation_line(t, 40))
            .filter(|t| !t.is_empty() && !is_credit_word(t))
    };
    let (role, pin) = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        (
            meta.role.as_deref().and_then(set),
            meta.presentation_value("title").as_deref().and_then(set),
        )
    };
    let sid = aterm_messages::presence::short_sid(ctx.self_id.as_str());
    match (role, pin) {
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
    }
}

/// THE NAME of `who` off the main thread, as [`App::typed_by`](crate::App::typed_by) names a
/// typist ([`name`]), by the sessions `store` registers now: each tab found by its shell's pid and
/// named as [`tab_named`] names it, so a program run from a tab (an `aterm ctl` a Claude Code's
/// tool ran) is that tab's, past the CLI's own process. The `signal` verb names its asker so
/// (the act's `from=`).
pub(crate) fn name_by_store(who: &Who, store: &crate::session_store::Store) -> Named {
    let tabs: Vec<(u64, i32, SessionId, Arc<SessionCtx>)> = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .live_handles()
        .map(|h| {
            (
                h.local_id,
                h.shell_identity.pid,
                h.sid.clone(),
                Arc::clone(&h.ctx),
            )
        })
        .collect();
    let shells: Vec<(u64, i32)> = tabs.iter().map(|(local, pid, ..)| (*local, *pid)).collect();
    name(
        who,
        &shells,
        &|sid| {
            tabs.iter()
                .find(|(_, _, of, _)| of == sid)
                .map(|(local, ..)| *local)
        },
        &|local| {
            tabs.iter()
                .find(|(of, ..)| *of == local)
                .map(|(.., ctx)| tab_named(ctx))
        },
    )
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

    /// A tab as it names who typed from it ([`name`], [`tab_named`]).
    pub(crate) fn typist_tab(&self, local: u64) -> Option<Named> {
        self.pool.get(local).map(|s| tab_named(&s.ctx))
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
#[path = "typed_by_tests.rs"]
mod tests;
