// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A SESSION'S BIRTH CLAIMS (the comprehension design §2.3, CX-04): what its spawn said of it,
//! written as `desc` claims at its open, so its card has a topic and a reason before any writer
//! runs. An agent worker's task's first line is its `title`, the spawn's `why=` its `matters`,
//! and `package=` its `package`. A spawn with no `why=` that joins a request (`req=`) whose
//! requester gave a why takes that why, as aterm's derived claim whoever the caller is; else a
//! worker whose `from=` names a session of this Mac takes that session's standing reason as it
//! stood at the open.
//!
//! The claims are the caller's: the session `from=` names, which ranks as the new session's
//! creator, or an `aterm ctl` in no tab, at the control rank ([`BirthAuthor::Stated`]), each at
//! `src=mark conf=exact`, the record's mark of "made at birth", which the `mark` verb never
//! writes (it writes `conf=claim`). So the author's next claim of the slot on the session
//! replaces it and its withdrawal removes it: `meta set|unset title` for the title, and
//! `mark - desc` only while no agent runs in the session, since while one runs `mark - desc`
//! claims the agent's run, where the birth claim still stands over it (ATERM-KI-1115-270 (a)).
//! A caller that is neither (a fleet drive, a session that is not the one `from=` names, a plain
//! spawn from a tab) gets aterm's derived claim instead ([`BirthAuthor::Derived`]: `by=aterm
//! src=aterm conf=exact`, its source in `as=`), so the reason stays visible where the caller's
//! own claim would only be listed. A derived birth writes no title: aterm's default writer
//! titles the session from the same task line, as the same author.
//!
//! An agent started by hand in a shell has no task: aterm's default writer titles its run from
//! its first prompt the harness did not type, which the run keeps here
//! ([`SessionLog::keep_first_prompt`]).

use aterm_slog::{
    Actor, BoundedToken, ClaimRefused, Conf, Key, Kind, MATTERS_AS_CREATOR, MATTERS_AS_WHY, Mark,
    Node, PACKAGE_AS_SPAWN, ShortId, Src, Stamp, TITLE_AS_TASK,
};

use super::SessionLog;
use super::mark::{DescRefused, ObservedClaim, desc_fields, desc_stored};
use crate::session_store::ExitActor;

const TITLE: &str = "title";
const MATTERS: &str = "matters";
const PACKAGE: &str = "package";

/// WHAT A SPAWN SAID OF THE SESSION IT OPENS, carried from the control thread to the new tab's
/// open (`Wake::SpawnSession`, then the act it opens under: [`crate::tab_marks::ActScope::asking`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Birth {
    /// An agent worker's task's first line ([`crate::task_record::task_line`]): the bytes
    /// `start_worker`'s `set_task_line` reads, so the birth title and aterm's default writer's
    /// `task` rung agree. `None` for a shell and a program worker.
    pub(crate) title: Option<String>,
    /// The spawn's `why=`, as [`aterm_types::control_verbs::request_why`] admitted it.
    pub(crate) matters: Option<String>,
    /// The spawn's `package=`, as [`aterm_slog::desc_text`] admitted it.
    pub(crate) package: Option<String>,
    /// A worker's `from=`: the session that starts it.
    pub(crate) from: Option<aterm_session::SessionId>,
    /// The given why of the request a `req=` joined, where the spawn gave no `why=`
    /// ([`crate::requests_host::Begun::joined_why`]): aterm's derived `matters`, kept apart from
    /// [`Self::matters`], which the tab's open records as the spawn's own `why_text=` and which
    /// is written as the caller's claim.
    pub(crate) request_why: Option<String>,
}

impl Birth {
    /// The birth of a spawn of `task` (`None`: a plain shell), its `why=` and `package=`.
    pub(crate) fn of(
        task: Option<&crate::task_launch::TaskOrder>,
        why: Option<String>,
        package: Option<String>,
    ) -> Birth {
        Birth {
            title: task
                .filter(|order| order.program.is_none())
                .and_then(|order| crate::task_record::task_line(&order.task)),
            matters: why,
            package,
            from: task.and_then(|order| order.caller.clone()),
            request_why: None,
        }
    }

    /// This birth of a spawn that joined a request whose requester gave the why `joined`
    /// ([`crate::requests_host::Begun::joined_why`]): kept as [`Self::request_why`] only where
    /// the spawn gave no `why=` of its own, which is the reason it states.
    #[must_use]
    pub(crate) fn joining(self, joined: Option<&str>) -> Birth {
        Birth {
            request_why: joined.filter(|_| self.matters.is_none()).map(str::to_owned),
            ..self
        }
    }
}

/// WHO A BIRTH CLAIM IS BY: always the caller, never `from=` written as the author.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BirthAuthor {
    /// The caller's own claim: the creator's session, or a control client in no tab.
    Stated(Actor),
    /// aterm's derived claim, naming its source in `as=`.
    Derived,
}

impl BirthAuthor {
    /// Who a birth by `by` is, where `creator` is the session `from=` names as the open resolved
    /// it ([`crate::session_log::sid_ident`]) and `fleet` whether a fleet drive asked. First
    /// match: a fleet drive's is derived (until the creator rule R2 is agreed, CX-81); the
    /// session `from=` names states its own; a control client in no tab states its own (a
    /// `from=` it names still names the creator); anyone else's (a session `from=` does not name,
    /// a plain spawn from a tab, aterm's harness, the bridge, nobody) is derived.
    pub(crate) fn of(by: &ExitActor, creator: Option<ShortId>, fleet: bool) -> BirthAuthor {
        if fleet {
            return BirthAuthor::Derived;
        }
        match by {
            ExitActor::Sid(sid) => match super::sid_ident(sid) {
                Some(id) if Some(id) == creator => BirthAuthor::Stated(Actor::Sid(id)),
                _ => BirthAuthor::Derived,
            },
            ExitActor::Ctl => BirthAuthor::Stated(Actor::Ctl),
            _ => BirthAuthor::Derived,
        }
    }
}

/// WHAT THE OPEN MADE OF A BIRTH (`App::birth_of`): who its claims are by, the creator its
/// `from=` names as resolved, and the creator's standing reason it copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Born {
    pub(crate) author: BirthAuthor,
    /// The sid `from=` resolved to: the creator the new session's claims are ranked by.
    pub(crate) creator: Option<String>,
    /// The creator's standing `matters` at the open, copied where the spawn gave no `why=` and
    /// joined no request whose why was given.
    pub(crate) copied: Option<String>,
}

impl Born {
    /// The claims to write of `birth`.
    pub(crate) fn claims<'a>(&'a self, birth: &'a Birth) -> BirthClaims<'a> {
        BirthClaims {
            birth,
            author: self.author,
            creator: self.creator.as_deref(),
            copied: self.copied.as_deref(),
        }
    }
}

/// The birth claims [`SessionLog::claim_birth`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BirthClaims<'a> {
    pub(crate) birth: &'a Birth,
    pub(crate) author: BirthAuthor,
    pub(crate) creator: Option<&'a str>,
    pub(crate) copied: Option<&'a str>,
}

impl SessionLog {
    /// WRITE A SESSION'S BIRTH CLAIMS at `at`, right after its open: first the creator `from=`
    /// names, so a creator's claim ranks [`aterm_slog::Rank::Creator`] from its first record;
    /// then, in this order, the `title` (a stated birth's task line, `as=task`), the `matters`
    /// (the `why=`, the caller's; else the given why of the request a `req=` joined, aterm's
    /// derived claim `as=why` whoever the caller is; else the creator's copied reason,
    /// `as=creator`) and the `package`. A claim
    /// the description rule refuses (only a credential, a protocol line, nothing left once
    /// redacted) is skipped and the rest are written. No mark token is spent: the spawn verb is
    /// the rate. Returns how many records it wrote.
    pub(crate) fn claim_birth(&mut self, c: &BirthClaims<'_>, at: Stamp) -> usize {
        if let Some(creator) = c.creator {
            self.set_desc_creator(Some(creator));
        }
        let birth = c.birth;
        let mut wrote = 0;
        if let (BirthAuthor::Stated(by), Some(title)) = (c.author, birth.title.as_deref()) {
            wrote += usize::from(
                self.claim_stated_at_birth(TITLE, title, Some(TITLE_AS_TASK), by, at)
                    .is_ok(),
            );
        }
        let matters = match (
            birth.matters.as_deref(),
            birth.request_why.as_deref(),
            c.copied,
        ) {
            (Some(why), ..) => {
                Some(self.claim_at_birth(c.author, MATTERS, why, MATTERS_AS_WHY, at))
            }
            (None, Some(why), _) => {
                Some(self.claim_derived_at_birth(MATTERS, why, MATTERS_AS_WHY, at))
            }
            (None, None, Some(copied)) => {
                Some(self.claim_derived_at_birth(MATTERS, copied, MATTERS_AS_CREATOR, at))
            }
            (None, None, None) => None,
        };
        wrote += usize::from(matters.is_some_and(|r| r.is_ok()));
        if let Some(package) = birth.package.as_deref() {
            wrote += usize::from(
                self.claim_at_birth(c.author, PACKAGE, package, PACKAGE_AS_SPAWN, at)
                    .is_ok(),
            );
        }
        wrote
    }

    /// One birth claim of `slot` by `author`: its own, or aterm's derived one naming `derived_as`.
    fn claim_at_birth(
        &mut self,
        author: BirthAuthor,
        slot: &str,
        raw: &str,
        derived_as: &str,
        at: Stamp,
    ) -> Result<u64, DescRefused> {
        match author {
            BirthAuthor::Stated(by) => self.claim_stated_at_birth(slot, raw, None, by, at),
            BirthAuthor::Derived => self.claim_derived_at_birth(slot, raw, derived_as, at),
        }
    }

    /// The caller's own birth claim: the text judged and, the author being no person, redacted
    /// as a mark's is ([`desc_stored`]), on the session, at `src=mark conf=exact`.
    fn claim_stated_at_birth(
        &mut self,
        slot: &str,
        raw: &str,
        as_name: Option<&str>,
        by: Actor,
        at: Stamp,
    ) -> Result<u64, DescRefused> {
        let token = BoundedToken::new(Key::Field, slot)
            .filter(|t| t.as_str() == slot)
            .ok_or(DescRefused::Claim(ClaimRefused::Slot))?;
        let text = desc_stored(slot, raw, by, Src::Mark)?;
        let mark = Mark::new(Kind::Desc, Src::Mark, Conf::Exact)
            .at(Node::SESSION)
            .by(by)
            .fields(desc_fields(token, Key::Value, Some(&text), as_name));
        self.log.mark(mark, at).map_err(DescRefused::of_log)
    }

    /// aterm's derived birth claim, through the one entry for what aterm derives
    /// ([`Self::claim_observed`]: redacted always, a person's text copied from the creator
    /// included), at `conf=exact` with its source in `as=`.
    fn claim_derived_at_birth(
        &mut self,
        slot: &str,
        raw: &str,
        as_name: &str,
        at: Stamp,
    ) -> Result<u64, DescRefused> {
        self.claim_observed(
            &ObservedClaim {
                on: Node::SESSION,
                slot,
                text: Some(raw),
                content: false,
                by: Actor::Aterm,
                src: Src::Aterm,
                conf: Conf::Exact,
                as_name: Some(as_name),
            },
            at,
        )
    }

    /// KEEP `text` AS THE FIRST PROMPT OF AGENT RUN `run`, the one aterm's default writer read
    /// first that the harness did not type: only while `run` is the open agent run and it has
    /// kept none (in this process or, by the carry's `kept=`, before an update), cut at a
    /// character boundary to [`aterm_agent::harness::excerpt::EXCERPT_BYTES`]. The text is never
    /// carried or journaled; the fact that the run kept one is carried
    /// ([`Self::kept_first_prompt`]).
    pub(crate) fn keep_first_prompt(&mut self, run: Node, text: &str) {
        let Some(agent) = self
            .agent
            .as_mut()
            .filter(|a| a.node == run && a.first_prompt.is_none() && !a.kept_prompt)
        else {
            return;
        };
        let mut end = text.len().min(aterm_agent::harness::excerpt::EXCERPT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if !text[..end].trim().is_empty() {
            agent.first_prompt = Some(Box::from(&text[..end]));
            agent.kept_prompt = true;
        }
    }

    /// The first prompt agent run `run` keeps, while it is the open agent run.
    pub(crate) fn first_prompt(&self, run: Node) -> Option<&str> {
        self.agent
            .as_ref()
            .filter(|a| a.node == run)
            .and_then(|a| a.first_prompt.as_deref())
    }

    /// Whether agent run `run`, while it is the open agent run, kept a first prompt: in this
    /// process ([`Self::first_prompt`]), or before the seamless update that carried it, whose
    /// text this process never had.
    pub(crate) fn kept_first_prompt(&self, run: Node) -> bool {
        self.agent
            .as_ref()
            .is_some_and(|a| a.node == run && a.kept_prompt)
    }
}

#[cfg(test)]
#[path = "session_log_birth_tests.rs"]
mod tests;
