// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ESCALATION TABLE: which records the broker flushes IN FULL before it shows or
//! acks them, on a tier whose ordinary ack does not wait for a full flush
//! ([`Durability::Ordered`](crate::Durability::Ordered) and
//! [`Durability::Relaxed`](crate::Durability::Relaxed)).
//!
//! The table is DATA the embedder hands the open ([`OpenOptions::escalation`]), matched
//! by the broker as each record is staged — never a flag a client sets on its request.
//! That is the whole point of putting it here: the broker decides, so a record an OLDER
//! client writes (one that has never heard of escalation) is escalated exactly like a
//! new one's. No request tag, no protocol version and no error code are spent on it.
//!
//! Three kinds of rule, each a reason a record must survive a power loss:
//!
//! - [`Escalate::Subject`]: the record's subject matches the filter (the
//!   `astream_wire::Filter` grammar). Catches a record by WHERE it goes, whoever wrote
//!   it — including a writer with no bound principal (an unbound root grant).
//! - [`Escalate::PrincipalPrefix`]: the producer that wrote it is BOUND
//!   ([`BrokerLog::binding`](crate::BrokerLog::binding), which counts a bind staged
//!   earlier in the same batch) to a principal starting with the prefix. Catches a
//!   record by WHO wrote it. On a broker that binds nobody (an unguarded one) this rule
//!   sees nothing.
//! - [`Escalate::Group`]: the record carries a consumer-group commit whose group
//!   matches the filter. The ONLY rule that sees a pure commit record, which is hidden
//!   (`/a/commit`) and written by producer 0.
//!
//! Hidden subjects (`/a/commit`, `/a/will`, `/a/bind`, `/a/retire`) are never matched by
//! the first two rules: they are the broker's own bookkeeping, and a subject filter as
//! broad as `>` must not drag every will registration into a full flush.
//!
//! Why a table and not a closure: the broker's shared state holds no caller-supplied
//! `dyn Fn` on purpose (an open principal for Trust's thread-spawn census, see
//! `Shared`), and a table built from data can be assembled at run time (from a
//! configuration, say) as easily as written in code. `asb serve` takes no table: on an
//! `asb` broker every record rides the tier's own promise.
//!
//! [`OpenOptions::escalation`]: crate::OpenOptions::escalation

use std::io;

use astream_wire::Filter;

use crate::store::is_hidden_subject;

/// One escalation rule — see the module docs.
#[non_exhaustive]
#[allow(clippy::derived_hash_with_manual_eq)]
#[derive(Debug, Clone, Eq, Hash)]
pub enum Escalate {
    /// Records whose subject matches this filter (hidden subjects excepted).
    Subject(String),
    /// Records written by a producer bound to a principal starting with this prefix
    /// (hidden subjects excepted). An empty prefix is refused at the open: it would
    /// escalate every bound producer, and reads as a forgotten argument.
    PrincipalPrefix(String),
    /// Records carrying a consumer-group commit whose group matches this filter.
    Group(String),
}

impl PartialEq for Escalate {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Escalate::Subject(a), Escalate::Subject(b)) => a.as_str() == b.as_str(),
            (Escalate::PrincipalPrefix(a), Escalate::PrincipalPrefix(b)) => {
                a.as_str() == b.as_str()
            }
            (Escalate::Group(a), Escalate::Group(b)) => a.as_str() == b.as_str(),
            _ => false,
        }
    }
}

/// An escalation table: the rules a log is opened with ([`OpenOptions`]), checked when
/// the log opens (a filter that does not parse refuses the open, naming it) and in force
/// from its first batch on. The default is empty: nothing is escalated.
///
/// [`OpenOptions`]: crate::OpenOptions
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Escalation {
    rules: Vec<Escalate>,
}

impl Escalation {
    /// A table of `rules`.
    pub fn new(rules: impl IntoIterator<Item = Escalate>) -> Escalation {
        let mut out = Vec::new();
        for r in rules {
            out.push(r);
        }
        Escalation { rules: out }
    }

    /// The rules, in the order given.
    #[must_use]
    pub fn rules(&self) -> &[Escalate] {
        &self.rules
    }

    /// Whether the table has no rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Parse every rule, refusing the first that is not one (`InvalidInput`, naming it).
    pub(crate) fn compile(&self) -> io::Result<Rules> {
        let mut rules = Rules::default();
        for rule in &self.rules {
            match rule {
                Escalate::Subject(raw) => match Filter::new(raw) {
                    Ok(f) => rules.subjects.push(f),
                    Err(e) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("escalation rule Subject({raw:?}) is not a filter: {e:?}"),
                        ))
                    }
                },
                Escalate::Group(raw) => match Filter::new(raw) {
                    Ok(f) => rules.groups.push(f),
                    Err(e) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("escalation rule Group({raw:?}) is not a filter: {e:?}"),
                        ))
                    }
                },
                Escalate::PrincipalPrefix(prefix) if prefix.is_empty() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "escalation rule PrincipalPrefix(\"\") would escalate every bound \
                         producer: name the prefix",
                    ))
                }
                Escalate::PrincipalPrefix(prefix) => rules.prefixes.push(prefix.clone()),
            }
        }
        rules.count = u16::try_from(self.rules.len()).unwrap_or(u16::MAX);
        Ok(rules)
    }
}

/// A parsed table, as the log holds it.
#[derive(Debug, Default)]
pub(crate) struct Rules {
    subjects: Vec<Filter>,
    prefixes: Vec<String>,
    groups: Vec<Filter>,
    /// How many rules the table was given (what the `Hello` block reports).
    count: u16,
}

impl Rules {
    /// How many rules the table holds.
    pub(crate) const fn count(&self) -> u16 {
        self.count
    }

    /// Whether a record on `subject`, by a producer bound to `principal` (if any),
    /// carrying a commit of `group` (if any), is escalated.
    pub(crate) fn matches(
        &self,
        subject: &str,
        principal: Option<&str>,
        group: Option<&str>,
    ) -> bool {
        let visible = !is_hidden_subject(subject);
        if visible {
            for f in &self.subjects {
                if f.matches_str(subject) {
                    return true;
                }
            }
            if let Some(p) = principal {
                for x in &self.prefixes {
                    if p.starts_with(x.as_str()) {
                        return true;
                    }
                }
            }
        }
        if let Some(g) = group {
            for f in &self.groups {
                if f.matches_str(g) {
                    return true;
                }
            }
        }
        false
    }

    /// Whether the table is empty (nothing to match, so staging skips the lookups).
    pub(crate) const fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Rules {
        Escalation::new([
            Escalate::Subject("/f/*/fleet/>".into()),
            Escalate::PrincipalPrefix("h-".into()),
            Escalate::Group("/f/*/cur/p/>".into()),
        ])
        .compile()
        .expect("the table parses")
    }

    #[test]
    fn each_rule_catches_its_own_kind_of_record() {
        let t = table();
        assert_eq!(t.count(), 3);
        assert!(
            t.matches("/f/F/fleet/h-a/halt", None, None),
            "a subject rule"
        );
        assert!(
            t.matches("/f/F/in/n/s/h-a/ask", Some("h-alice"), None),
            "a principal rule"
        );
        assert!(
            t.matches("/a/commit", None, Some("/f/F/cur/p/alice/inbox")),
            "a group rule sees a pure commit"
        );
        assert!(
            !t.matches("/f/F/in/n/s/s-1/note", Some("n-1"), None),
            "machine chatter"
        );
        assert!(
            !t.matches("/a/commit", None, Some("/f/F/cur/n/node/inbox")),
            "a node's commit"
        );
    }

    /// The subject and principal rules never see the broker's own hidden records: a
    /// person's producer binding (`/a/bind`), or a will, stays on the ordinary path.
    #[test]
    fn hidden_subjects_escape_the_subject_and_principal_rules() {
        let all = Escalation::new([
            Escalate::Subject("/>".into()),
            Escalate::PrincipalPrefix("h-".into()),
        ])
        .compile()
        .unwrap();
        for hidden in ["/a/bind", "/a/will", "/a/retire", "/a/commit"] {
            assert!(!all.matches(hidden, Some("h-alice"), None), "{hidden}");
        }
        assert!(all.matches("/x", None, None));
    }

    #[test]
    fn a_rule_that_is_not_one_refuses_by_name() {
        for (bad, says) in [
            (Escalate::Subject("not a filter".into()), "Subject"),
            (Escalate::Group("".into()), "Group"),
            (Escalate::PrincipalPrefix(String::new()), "PrincipalPrefix"),
        ] {
            let err = Escalation::new([bad]).compile().unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains(says), "{err}");
        }
        assert!(Escalation::default().compile().unwrap().is_empty());
    }
}
