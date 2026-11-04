// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit native execution assignments carried by a separate signed vault
//! entry. Parsing this document alone grants nothing; its current authenticated
//! source and member binding must be checked by the composing executable.

use super::Provider;
use crate::harness::ai_pool;
use std::collections::BTreeSet;

pub const ENTRY: &str = "ai-native-pool.toml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Intent,
    Preparation,
}

impl Scope {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "intent" => Some(Self::Intent),
            "preparation" => Some(Self::Preparation),
            _ => None,
        }
    }

    pub const fn word(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Preparation => "preparation",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subject {
    pub scope: Scope,
    pub account: [u8; 32],
    pub revision: [u8; 32],
    pub provider: Provider,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub subject: Subject,
    pub warm: Vec<String>,
    pub automatic: Option<Allocator>,
}

/// A root-signed delegation. The machine may succeed its writer only after
/// the current unique-machine membership predicate is checked independently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocator {
    pub actor: [u8; 48],
    pub machine: [u8; 16],
    pub grant: [u8; 16],
}

impl Assignment {
    pub fn validate(&self) -> Result<(), String> {
        if self.subject.account == [0; 32]
            || self.subject.revision == [0; 32]
            || self.warm.len() > ai_pool::MAX_WARM_SLOTS
            || self
                .automatic
                .as_ref()
                .is_some_and(|a| a.actor == [0; 48] || a.machine == [0; 16] || a.grant == [0; 16])
        {
            return Err("invalid native assignment or too many warm slots".into());
        }
        let mut seen = BTreeSet::new();
        for label in &self.warm {
            if ai_pool::leading_m_label(label).is_none_or(|(_, canonical)| canonical != *label)
                || !seen.insert(label)
            {
                return Err("native warm slots need distinct canonical member labels".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub assignments: Vec<Assignment>,
}

impl Policy {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.len() > 64 * 1024 {
            return Err("native policy exceeds its size bound".into());
        }
        let value: aterm_toml::Table =
            aterm_toml::from_str(raw).map_err(|_| "native policy is not valid TOML")?;
        if value.get("version").and_then(aterm_toml::Value::as_integer) != Some(1)
            || value
                .keys()
                .any(|key| !matches!(key.as_str(), "version" | "account"))
        {
            return Err("unsupported native policy schema".into());
        }
        let mut policy = Self::default();
        if let Some(rows) = value.get("account") {
            let rows = rows.as_array().ok_or("native policy needs account rows")?;
            for row in rows {
                let row = row
                    .as_table()
                    .ok_or("native policy account is not a table")?;
                if !(row.len() == 5 || row.len() == 9)
                    || row.keys().any(|key| {
                        !matches!(
                            key.as_str(),
                            "scope"
                                | "account"
                                | "revision"
                                | "provider"
                                | "warm"
                                | "allocation"
                                | "allocator"
                                | "allocator_machine"
                                | "grant"
                        )
                    })
                {
                    return Err("unknown or missing native policy field".into());
                }
                let text = |key| {
                    row.get(key)
                        .and_then(aterm_toml::Value::as_str)
                        .ok_or("native policy field is not text")
                };
                let digest = |key| {
                    let mut out = [0; 32];
                    if !aterm_codec::hex::decode_into(text(key)?.as_bytes(), &mut out)
                        || out == [0; 32]
                    {
                        return Err("native policy identity is invalid");
                    }
                    Ok(out)
                };
                let assignment = Assignment {
                    subject: Subject {
                        scope: Scope::parse(text("scope")?)
                            .ok_or("unknown native assignment scope")?,
                        account: digest("account")?,
                        revision: digest("revision")?,
                        provider: Provider::of(text("provider")?)
                            .ok_or("unknown native provider")?,
                    },
                    warm: row
                        .get("warm")
                        .and_then(aterm_toml::Value::as_array)
                        .ok_or("native policy warm slots are not an array")?
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .map(str::to_owned)
                                .ok_or("native policy warm slot is not text")
                        })
                        .collect::<Result<_, _>>()?,
                    automatic: if row.len() == 9 {
                        if text("allocation")? != "automatic" {
                            return Err("unknown native allocation mode".into());
                        }
                        let mut actor = [0; 48];
                        let mut machine = [0; 16];
                        let mut grant = [0; 16];
                        if !aterm_codec::hex::decode_into(text("allocator")?.as_bytes(), &mut actor)
                            || !aterm_codec::hex::decode_into(
                                text("allocator_machine")?.as_bytes(),
                                &mut machine,
                            )
                            || !aterm_codec::hex::decode_into(text("grant")?.as_bytes(), &mut grant)
                        {
                            return Err("invalid native allocator binding".into());
                        }
                        Some(Allocator {
                            actor,
                            machine,
                            grant,
                        })
                    } else {
                        None
                    },
                };
                assignment.validate()?;
                if policy
                    .assignments
                    .iter()
                    .any(|old| same_slot(old.subject, assignment.subject))
                {
                    return Err("duplicate native policy assignment".into());
                }
                if assignment.automatic.is_some()
                    && policy.assignments.iter().any(|old| {
                        old.subject.provider == assignment.subject.provider
                            && old.automatic.is_some()
                    })
                {
                    return Err("more than one automatic native subject for a provider".into());
                }
                policy.assignments.push(assignment);
            }
        }
        Ok(policy)
    }

    /// An empty warm set or a different revision is held. `member` must come
    /// from the current authenticated recipient binding, never a hostname.
    pub fn permits(&self, subject: Subject, member: &str) -> bool {
        self.assignments
            .iter()
            .any(|row| row.subject == subject && row.warm.iter().any(|warm| warm == member))
    }

    pub fn set(&mut self, assignment: Assignment) -> Result<(), String> {
        assignment.validate()?;
        if assignment.automatic.is_some()
            && self.assignments.iter().any(|row| {
                !same_slot(row.subject, assignment.subject)
                    && row.subject.provider == assignment.subject.provider
                    && row.automatic.is_some()
            })
        {
            return Err("more than one automatic native subject for a provider".into());
        }
        if self.assignments.iter().any(|row| {
            !same_slot(row.subject, assignment.subject)
                && row.subject.provider == assignment.subject.provider
                && row
                    .warm
                    .iter()
                    .any(|member| assignment.warm.contains(member))
        }) {
            return Err(
                "native member has more than one assigned account for this provider".into(),
            );
        }
        if let Some(row) = self
            .assignments
            .iter_mut()
            .find(|row| same_slot(row.subject, assignment.subject))
        {
            *row = assignment;
        } else {
            self.assignments.push(assignment);
        }
        Ok(())
    }

    pub fn assigned(&self, provider: Provider, member: &str) -> Result<Option<Subject>, String> {
        let mut found = self.assignments.iter().filter(|row| {
            row.subject.provider == provider && row.warm.iter().any(|warm| warm == member)
        });
        let first = found.next().map(|row| row.subject);
        if found.next().is_some() {
            return Err("native execution assignment is ambiguous".into());
        }
        Ok(first)
    }

    pub fn automatic(&self, provider: Provider) -> Result<Option<&Assignment>, String> {
        let mut rows = self
            .assignments
            .iter()
            .filter(|row| row.subject.provider == provider && row.automatic.is_some());
        let first = rows.next();
        if rows.next().is_some() {
            return Err("automatic native assignment is ambiguous".into());
        }
        Ok(first)
    }

    pub fn text(&self) -> String {
        let mut out = "version = 1\n".to_owned();
        for row in &self.assignments {
            out.push_str(&format!("\n[[account]]\nscope = {:?}\naccount = {:?}\nrevision = {:?}\nprovider = {:?}\nwarm = [{}]\n",
                row.subject.scope.word(),aterm_codec::hex::encode_lower(&row.subject.account),
                aterm_codec::hex::encode_lower(&row.subject.revision),row.subject.provider.name(),
                row.warm.iter().map(|label|format!("{label:?}")).collect::<Vec<_>>().join(", ")));
            if let Some(allocator) = &row.automatic {
                out.push_str(&format!(
                    "allocation = \"automatic\"\nallocator = {:?}\nallocator_machine = {:?}\ngrant = {:?}\n",
                    aterm_codec::hex::encode_lower(&allocator.actor),
                    aterm_codec::hex::encode_lower(&allocator.machine),
                    aterm_codec::hex::encode_lower(&allocator.grant)
                ));
            }
        }
        out
    }
}

fn same_slot(a: Subject, b: Subject) -> bool {
    a.account == b.account && a.scope == b.scope && a.provider == b.provider
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment() -> Assignment {
        Assignment {
            subject: Subject {
                scope: Scope::Intent,
                account: [1; 32],
                revision: [2; 32],
                provider: Provider::Google,
            },
            warm: vec!["m117".into()],
            automatic: None,
        }
    }

    #[test]
    fn native_policy_binds_revision_provider_and_exact_member() {
        let mut policy = Policy::default();
        let row = assignment();
        policy.set(row.clone()).unwrap();
        assert!(policy.permits(row.subject, "m117"));
        assert!(!policy.permits(row.subject, "M117.local"));
        assert!(!policy.permits(
            Subject {
                revision: [3; 32],
                ..row.subject
            },
            "m117"
        ));
        assert!(!policy.permits(
            Subject {
                provider: Provider::Meta,
                ..row.subject
            },
            "m117"
        ));
        assert_eq!(Policy::parse(&policy.text()).unwrap(), policy);
        let mut held = row;
        held.warm.clear();
        policy.set(held.clone()).unwrap();
        assert!(!policy.permits(held.subject, "m117"));
    }

    #[test]
    fn native_policy_refuses_aliases_duplicates_and_extra_slots() {
        for warm in [vec!["M1"], vec!["m1", "m1"], vec!["m1", "m2", "m3", "m4"]] {
            let mut row = assignment();
            row.warm = warm.into_iter().map(str::to_owned).collect();
            assert!(row.validate().is_err());
        }
        let mut p = Policy::default();
        p.set(assignment()).unwrap();
        let raw = p.text();
        assert!(Policy::parse(&(raw.clone() + raw.trim_start_matches("version = 1\n"))).is_err());
        assert!(Policy::parse(&(raw + "arbitrary = true\n")).is_err());
    }

    #[test]
    fn automatic_policy_is_explicit_delegation_and_keeps_fixed_semantics() {
        let mut fixed = Policy::default();
        fixed.set(assignment()).unwrap();
        assert!(fixed.automatic(Provider::Google).unwrap().is_none());
        assert!(!fixed.text().contains("allocation"));
        let mut row = assignment();
        row.warm.clear();
        row.automatic = Some(Allocator {
            actor: [7; 48],
            machine: [8; 16],
            grant: [9; 16],
        });
        let mut policy = Policy::default();
        policy.set(row.clone()).unwrap();
        assert!(!policy.permits(row.subject, "m117"));
        assert_eq!(Policy::parse(&policy.text()).unwrap(), policy);
        let mut other = row.clone();
        other.subject.account = [9; 32];
        assert!(policy.set(other).is_err());
        let mut other = row.clone();
        other.subject.scope = Scope::Preparation;
        assert!(policy.set(other).is_err());
        let mut duplicate = row.clone();
        duplicate.subject.revision = [10; 32];
        policy.set(duplicate.clone()).unwrap();
        assert_eq!(policy.assignments, vec![duplicate]);
        row.automatic.as_mut().unwrap().actor = [0; 48];
        assert!(row.validate().is_err());
        assert!(Policy::parse(&policy.text().replace("automatic", "fixed")).is_err());
    }
}
