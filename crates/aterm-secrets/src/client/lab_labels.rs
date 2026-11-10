// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine numbers this Mac's vault gives to other Macs, read from its local copy
//! (`docs/DESIGN-machine-names-2026-11-04.md` §11.86): what a person's rename of this Mac
//! (`aterm name set`, Rename This Mac…) must not take on today's lab, where other Macs reach a Mac
//! by its roster label and aliases and its log seat, not by a name record.
//!
//! The rules are admission's (`admit::labels_taken`): every other row's label, aliases, claim and
//! `[machines]` name, the roster's denied labels (a removed Mac's included: the roster read
//! overlays the log's removals as denied labels), and each other seat's bare label in the
//! membership log. Only a Mac holds a number: none is reserved, `m3` included, and no role or
//! policy list holds one (owner ruling, 2026-11-09). This Mac's own row and seats never free
//! another Mac's hold. A fleet-accounts sheet's numbers hold nothing: a rename onto one binds its
//! rows to this Mac, which is how the owner names a Mac its sheet already knows.

use super::*;
use crate::swarm_roster::{SwarmPolicy, SwarmRoster};
use aterm_types::machine_name::Number;

/// Hang detector for the whole-generation read waiting for the vault's state writer.
const LOCK_HANG: Duration = Duration::from_secs(60);

/// What gives a number to another Mac ([`LabLabels::held`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelHold {
    /// Another Mac's roster label.
    Label,
    /// One of another Mac's roster aliases, an earlier label `--host` still reaches it by.
    Alias,
    /// A sheet number the roster holds for another Mac.
    Claim,
    /// The leading number of another Mac's vault `[machines]` name.
    VaultName,
    /// Another Mac's seat in the membership log.
    Seat,
    /// The roster denies it: a Mac named so reads as denied (the survey drops such a label).
    Denied,
}

impl LabelHold {
    /// What holds the number, as the end of "m42 is …".
    #[must_use]
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::Label => "another Mac's label in the vault's roster",
            Self::Alias => "an earlier label of another Mac, which the roster still reaches it by",
            Self::Claim => "held by the vault's roster for another Mac",
            Self::VaultName => "the start of another Mac's name in the vault",
            Self::Seat => "another Mac's seat in the membership log",
            Self::Denied => "denied by the vault's roster, which reads a Mac named so as denied",
        }
    }
}

/// The numbers this Mac's vault gives, from its local copy ([`lab_labels`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabLabels {
    /// Each number the vault gives another Mac, with the first holder found: the roster, then
    /// the log.
    pub held: BTreeMap<Number, LabelHold>,
    /// This Mac's own: its roster row's label, aliases and claim, and its log seats' bare labels.
    pub own: BTreeSet<Number>,
    /// This Mac's roster label.
    pub label: Option<Number>,
    /// The numbers the policy names its vault servers by, this Mac's own included: other Macs
    /// dial each as `m<N>.local`, by a bare label or a URL.
    pub servers: BTreeSet<Number>,
}

/// The numbers this Mac's vault gives to other Macs, read from its local copy under
/// one shared observation of its state lock, so no apply lands between the roster (as the
/// trusted log overlays it), the policy where this Mac reads it, and the log's seats. Nothing
/// where no vault is configured or this Mac holds no identity, and nothing for a Mac its vault
/// took out or a plain vault, which are off the lab.
///
/// # Errors
/// This Mac is still joining its fleet, or its last apply did not finish, so it holds no whole
/// copy of the lab's labels; it holds no applied copy at all; a writer held the state lock past
/// the one-minute hang detector; or the configuration, identity, state, roster, policy or log
/// cannot be read or parsed. A rename never reads any of these as nothing held.
pub fn lab_labels(paths: &Paths) -> crate::Result<LabLabels> {
    lab_labels_with_clock(paths, Instant::now)
}

fn lab_labels_with_clock(
    paths: &Paths,
    mut now: impl FnMut() -> Instant,
) -> crate::Result<LabLabels> {
    let Some(config) = config(paths)? else {
        return Ok(LabLabels::default());
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(LabLabels::default());
    };
    let deadline = now() + LOCK_HANG;
    let held = observe_within(paths, deadline, &mut now)?;
    let state = load_state(paths)?;
    match state.membership {
        Stored::Revoked | Stored::Plain => return Ok(LabLabels::default()),
        Stored::Pending | Stored::None => {
            return Err(Error::new(format!(
                "this {} is still joining its fleet, so it holds no copy of the lab's labels yet",
                aterm_types::machine_name::OS_NOUN
            )));
        }
        Stored::In if state.failed || !state.pending.is_empty() => {
            return Err(Error::new(format!(
                "this {}'s last vault apply did not finish, so its copy of the lab's labels is \
                 not whole",
                aterm_types::machine_name::OS_NOUN
            )));
        }
        Stored::In => {}
    }
    let Some(lock) = held else {
        return Err(Error::new(format!(
            "this {}'s vault holds no applied copy, so the lab's labels are not read",
            aterm_types::machine_name::OS_NOUN
        )));
    };
    let me = identity.recipient().to_bech32();
    let roster = super::roster::swarm_roster_held(paths, &lock)?;
    let policy = entry::read_reserved_entry(paths, &config, crate::SWARM_POLICY_ENTRY)?
        .map(|entry| {
            std::str::from_utf8(entry.value().as_slice())
                .map_err(|_| Error::new("the vault's aterm-swarm-policy is not UTF-8"))
                .and_then(SwarmPolicy::from_toml)
        })
        .transpose()?;
    let seats: Vec<(String, String)> = log_entries::trusted_local_log(paths, &config)?
        .map(|log| {
            log.membership()
                .members()
                .values()
                .map(|member| (member.label.clone(), member.binding.recipient().to_bech32()))
                .collect()
        })
        .unwrap_or_default();
    drop(lock);
    Ok(assemble(&me, roster.as_ref(), policy.as_ref(), &seats))
}

/// The state lock taken shared, waiting out a writer until `deadline`; `None` where no lock file
/// exists, so no copy was ever applied here.
fn observe_within(
    paths: &Paths,
    deadline: Instant,
    now: &mut impl FnMut() -> Instant,
) -> crate::Result<Option<StateLock>> {
    let mut backoff = Duration::from_millis(1);
    loop {
        StateLock::narrow_owned(paths)?;
        match StateLock::observe_classified(paths)? {
            StateObservation::Held(lock) => return Ok(Some(lock)),
            StateObservation::Absent => return Ok(None),
            StateObservation::Busy => {}
        }
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Err(Error::of_kind(
                crate::ErrorKind::VaultBusy,
                format!(
                    "this {}'s vault is busy: a writer still holds its state lock",
                    aterm_types::machine_name::OS_NOUN
                ),
            ));
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

/// The number a vault server's policy name is dialed by: a bare `m<N>` (dialed as
/// `<label>.local`), or a URL whose host is `m<N>.local`. Bare digits are dialed as themselves.
fn dialed(server: &str) -> Option<Number> {
    let host = match server
        .strip_prefix("http://")
        .or_else(|| server.strip_prefix("https://"))
    {
        Some(rest) => {
            let host = rest.split(['/', ':']).next()?;
            let dot = host.len().checked_sub(".local".len())?;
            host.get(dot..)
                .filter(|tail| tail.eq_ignore_ascii_case(".local"))
                .and_then(|_| host.get(..dot))?
        }
        None => server,
    };
    Number::parse(host)
        .ok()
        .filter(|num| num.to_string().eq_ignore_ascii_case(host))
}

/// A label that is exactly a number: `m40`, not a clash's `m40-3fa2`, whose bare number is the
/// first claimant's.
fn bare(label: &str) -> Option<Number> {
    Number::leading(label).filter(|num| num.to_string() == label)
}

/// [`lab_labels`] over what it read: `me` is this Mac's age recipient, and each seat a log label
/// with its Mac's recipient. Another Mac's row or seat holds its numbers whatever this Mac's own
/// say (two rows may claim one sheet number, and this Mac's stale `[machines]` name may lead with
/// another Mac's label). The policy holds no number: it only names the servers it dials.
pub(crate) fn assemble(
    me: &str,
    roster: Option<&SwarmRoster>,
    policy: Option<&SwarmPolicy>,
    seats: &[(String, String)],
) -> LabLabels {
    let hold = |held: &mut BTreeMap<Number, LabelHold>, num: Option<Number>, why| {
        if let Some(num) = num {
            held.entry(num).or_insert(why);
        }
    };
    let mut held = BTreeMap::new();
    let mut own = BTreeSet::new();
    let mut label = None;
    if let Some(roster) = roster {
        for member in &roster.members {
            let claim = member.claimed_label.as_deref().and_then(bare);
            if member.recipient == me {
                label = Number::leading(&member.label);
                own.extend(Number::leading(&member.label));
                own.extend(
                    member
                        .aliases
                        .iter()
                        .filter_map(|alias| Number::leading(alias)),
                );
                own.extend(claim);
                continue;
            }
            if roster.is_member_denied(member) {
                hold(&mut held, Number::leading(&member.label), LabelHold::Denied);
                for alias in &member.aliases {
                    hold(&mut held, Number::leading(alias), LabelHold::Denied);
                }
                continue;
            }
            hold(&mut held, Number::leading(&member.label), LabelHold::Label);
            for alias in &member.aliases {
                hold(&mut held, Number::leading(alias), LabelHold::Alias);
            }
            hold(&mut held, claim, LabelHold::Claim);
            hold(
                &mut held,
                Number::leading(&member.vault_name),
                LabelHold::VaultName,
            );
        }
        for denied in &roster.deny_labels {
            hold(&mut held, Number::leading(denied), LabelHold::Denied);
        }
    }
    for (seat, recipient) in seats {
        if recipient == me {
            own.extend(bare(seat));
        } else {
            hold(&mut held, bare(seat), LabelHold::Seat);
        }
    }
    let servers = policy
        .into_iter()
        .flat_map(|policy| policy.servers.iter().filter_map(|server| dialed(server)))
        .collect();
    LabLabels {
        held,
        own,
        label,
        servers,
    }
}

#[cfg(test)]
#[path = "lab_labels_tests.rs"]
mod tests;
