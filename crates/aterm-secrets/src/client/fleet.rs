// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated ALab fleet-code distribution. The code is never materialized
//! as a secret file, logged, or included in an error.

use super::*;

/// Reserved vault entry holding the default ALab fleet's join code.
pub const FLEET_CODE_ENTRY: &str = "aterm-fleet-code";

/// Reserved `[everyone]` vault entry holding the swarm roster and its grow-only `deny_nodes`.
pub const SWARM_ROSTER_ENTRY: &str = "aterm-swarm-roster";

/// The signed ALab default fleet read from the locally applied vault copy: its
/// join code (never printed) and whether this machine holds that copy's admin
/// locker. Fleet sync grants an admin the default drive grant as it grants every
/// member (`docs/DESIGN-fluid-swarm-2026-10-28.md` F2).
pub struct AlabFleet {
    /// Join code from [`FLEET_CODE_ENTRY`].
    pub code: SecretBytes,
    /// `true` when this machine's vault identity opens the envelope's `admin`
    /// locker.
    pub admin: bool,
}

impl std::fmt::Debug for AlabFleet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlabFleet")
            .field("admin", &self.admin)
            .finish_non_exhaustive()
    }
}

/// Read the current signed ALab fleet code and whether this machine holds the
/// vault's admin locker after the local apply completed. Open-admitted ALab
/// members qualify under the owner's 2026-10-26 default full-membership policy.
/// Other vaults never supply the default fleet. `None`: the applied copy
/// carries no code for this machine (none is published, or the machine is not
/// yet a member it reaches).
///
/// # Errors
/// Local state, the signed cache or its recipient or admin lockers cannot be
/// read safely, or the apply has not finished (a pass that held the state lock
/// through the whole wait for it, or the last one leaving an entry failed or
/// pending), so the code is read once it has.
pub fn alab_fleet(paths: &Paths) -> crate::Result<Option<AlabFleet>> {
    let expected =
        config(paths)?.ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
    if !alab_vault(&expected) {
        return Ok(None);
    }
    reserved_fleet(paths, &expected)
}

/// Read an applied fixture's default fleet without requiring the compiled ALab key.
/// Production always uses [`alab_fleet`]; this seam is absent from release builds.
#[cfg(any(test, debug_assertions))]
pub fn test_alab_fleet(paths: &Paths) -> crate::Result<Option<AlabFleet>> {
    let expected = config(paths)?.ok_or_else(|| Error::new("no fixture vault configured"))?;
    reserved_fleet(paths, &expected)
}

/// Read the current signed ALab fleet code after the local apply completed.
/// Open-admitted ALab members qualify under the owner's 2026-10-26 default
/// full-membership policy. Other vaults never supply the default fleet.
/// `None`: the applied copy carries no code for this machine (none is
/// published, or the machine is not yet a member it reaches).
///
/// # Errors
/// Local state, the signed cache or its recipient or admin lockers cannot be
/// read safely, or the apply has not finished (a pass that held the state lock
/// through the whole wait for it, or the last one leaving an entry failed or
/// pending), so the code is read once it has.
pub fn alab_fleet_code(paths: &Paths) -> crate::Result<Option<SecretBytes>> {
    Ok(alab_fleet(paths)?.map(|fleet| fleet.code))
}

/// [`alab_fleet`]'s read of `expected`'s applied copy, any vault's: an apply
/// that has not finished is an error naming it, never read as no code. A vault pass
/// holding the state lock is waited for ([`StateLock::observe_judging`]).
pub(super) fn reserved_fleet(paths: &Paths, expected: &Config) -> crate::Result<Option<AlabFleet>> {
    let observation = match StateLock::observe_narrowed(paths)? {
        StateObservation::Busy => StateLock::observe_judging(paths)?,
        observation => observation,
    };
    let lock = match observation {
        StateObservation::Held(lock) => lock,
        StateObservation::Absent => return Ok(None),
        StateObservation::Busy => {
            return Err(writer_outlasted_wait(
                "the default fleet code is read once it ends",
            ));
        }
    };
    if steward::local_recipient_denial_held(paths, &lock)? {
        return Ok(None);
    }
    if let Some((entry, admin)) =
        entry::read_reserved_entry_and_admin(paths, expected, FLEET_CODE_ENTRY)?
    {
        return Ok(Some(AlabFleet {
            code: entry.into_parts().0,
            admin,
        }));
    }
    let state = load_state(paths)?;
    if state.membership == Stored::In && (state.failed || !state.pending.is_empty()) {
        return Err(Error::new(
            "this machine's last vault apply did not finish (`aterm secrets sync` names what \
             failed): the default fleet code is read once it does",
        ));
    }
    Ok(None)
}

/// [`alab_fleet_code`]'s read of `expected`'s applied copy, any vault's: an
/// apply that has not finished is an error naming it, never read as no code.
#[cfg(test)]
pub(super) fn reserved_code(
    paths: &Paths,
    expected: &Config,
) -> crate::Result<Option<SecretBytes>> {
    Ok(reserved_fleet(paths, expected)?.map(|fleet| fleet.code))
}

/// Publish the code of the fleet this Mac minted to every current and future ALab
/// vault member, including open-admitted machines. This changes no fleet or
/// drive setting on the publisher.
///
/// # Errors
/// The caller is not an ALab vault admin, or publication fails.
pub fn admin_publish_alab_fleet(paths: &Paths, code: &SecretBytes) -> crate::Result<Published> {
    let admin = alab_admin(paths)?;
    if code.is_empty() {
        return Err(Error::new("the fleet join code is empty"));
    }
    let doc = with_code(&admin.doc, code)?;
    admin_publish(paths, &admin, &doc)
}

/// The admin of the ALab vault, for a publish of the default fleet's code.
fn alab_admin(paths: &Paths) -> crate::Result<Admin> {
    let admin = admin_open(paths)?;
    if !alab_vault(&admin.config) {
        return Err(Error::new(
            "only an ALab vault admin can publish the default fleet",
        ));
    }
    Ok(admin)
}

/// `doc` with `code` as the default fleet's, for every member.
fn with_code(doc: &Document, code: &SecretBytes) -> crate::Result<Document> {
    let mut doc = doc.clone();
    doc.set_entry(&Audience::Everyone, FLEET_CODE_ENTRY, code.as_slice())
        .map_err(|error| Error::new(error.to_string()))?;
    Ok(doc)
}

/// Parse the top-level `deny_nodes` array from an `aterm-swarm-roster` TOML document,
/// returning sorted, deduplicated, lowercase node identifiers.
///
/// # Errors
/// `text` is not valid TOML, or `deny_nodes` is present and not an array of strings.
pub fn parse_denied_nodes_toml(text: &str) -> crate::Result<Vec<String>> {
    let table: aterm_toml::Table = aterm_toml::from_str(text).map_err(|error| {
        Error::new(format!(
            "invalid {SWARM_ROSTER_ENTRY}: {}",
            error.log_words()
        ))
    })?;
    let Some(raw) = table.get("deny_nodes") else {
        return Ok(Vec::new());
    };
    let items = raw.as_array().ok_or_else(|| {
        Error::new(format!(
            "{SWARM_ROSTER_ENTRY}: `deny_nodes` must be an array"
        ))
    })?;
    let mut set = BTreeSet::new();
    for item in items {
        let s = item.as_str().ok_or_else(|| {
            Error::new(format!(
                "{SWARM_ROSTER_ENTRY}: `deny_nodes` must contain only strings"
            ))
        })?;
        let trimmed = s.trim_ascii().to_ascii_lowercase();
        if !trimmed.is_empty() {
            set.insert(trimmed);
        }
    }
    Ok(set.into_iter().collect())
}

/// Read the verified swarm roster's `deny_nodes` from the locally applied vault copy.
///
/// Returns:
/// - `Ok(Some(nodes))` when an authoritative answer is available (`nodes` is empty when no
///   signed vault is configured, this machine has not joined yet, or the applied vault carries
///   no `aterm-swarm-roster` entry).
/// - `Ok(None)` when the local vault state is mid-apply, locked, partial, or revoked, so a
///   listener retains its last authoritative deny-list rather than clearing it transiently.
///   A signed, joined copy without its observation lock is partial too.
///
/// A state lock an older build left wider than `0600` is narrowed first, where it is this user's
/// to narrow (`StateLock::narrow_owned`), so an upgrade does not leave the list unread until the
/// next vault sync.
///
/// # Errors
/// Local vault files cannot be read, or `aterm-swarm-roster` is malformed UTF-8 / TOML.
pub fn swarm_denied_nodes(paths: &Paths) -> crate::Result<Option<Vec<String>>> {
    swarm_denied_nodes_then(paths, || {})
}

/// One authoritative deny-list observation. Unavailability cannot authorize an empty list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwarmDenialSnapshot {
    /// Owned deny-list contents, including an authoritative empty list.
    Available(Vec<String>),
    /// The actual observation could not supply an authoritative list.
    Unavailable(SwarmDenialUnavailable),
}

impl SwarmDenialSnapshot {
    /// A listener retains its previous verdict when the current observation is unavailable.
    pub(super) fn into_nodes(self) -> Option<Vec<String>> {
        match self {
            Self::Available(nodes) => Some(nodes),
            Self::Unavailable(_) => None,
        }
    }
}

/// Why a deny-list observation was unavailable. These reasons carry no vault contents or
/// identifiers and grant no repair, enrollment or retry authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwarmDenialUnavailable {
    /// A writer held the state lock at the observation or bounded read's deadline.
    WriterBusy,
    /// The configured vault changed during this read or its bounded wait.
    ConfigurationChanged,
    /// The applied state marks this machine revoked.
    Revoked,
    /// A signed joined copy has no state observation lock.
    MissingStateLock,
    /// No usable reserved entry or authenticated absence was observed.
    EntryUnavailable,
    /// The absent-entry path could not read a local identity.
    MissingIdentity,
    /// The verified membership log holds this recipient out.
    RecipientHeldOut,
}

impl std::fmt::Display for SwarmDenialUnavailable {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(match self {
            Self::WriterBusy => "a vault writer holds the local state lock",
            Self::ConfigurationChanged => "the configured vault changed during the read",
            Self::Revoked => "the applied state marks this machine revoked",
            Self::MissingStateLock => "the joined vault copy has no state observation lock",
            Self::EntryUnavailable => {
                "the reserved roster entry is unavailable in this applied copy"
            }
            Self::MissingIdentity => "the local vault identity is unavailable",
            Self::RecipientHeldOut => "the verified membership log holds this recipient out",
        })
    }
}

/// Read the same authoritative deny list as [`swarm_denied_nodes`], retaining the reason for
/// an unavailable observation instead of projecting it to a listener's `None` signal.
///
/// # Errors
/// As [`swarm_denied_nodes`].
pub fn swarm_denial_snapshot(paths: &Paths) -> crate::Result<SwarmDenialSnapshot> {
    swarm_denial_snapshot_then(paths, || {})
}

/// Read the same authoritative deny list as [`swarm_denial_snapshot`], waiting only for a
/// concurrent vault writer until `deadline`. Each attempt reads the current configuration and
/// applied state afresh under the configuration that first encountered that writer. A changed
/// or removed configuration is unavailable; an unavailable, revoked or malformed copy is never
/// a reason to wait. The shared observation is released before returning the owned list.
///
/// # Errors
/// As [`swarm_denied_nodes`]. A writer still holding state at the deadline returns
/// [`SwarmDenialUnavailable::WriterBusy`].
pub fn swarm_denial_snapshot_until(
    paths: &Paths,
    deadline: Instant,
) -> crate::Result<SwarmDenialSnapshot> {
    swarm_denial_snapshot_until_with(paths, deadline, std::thread::sleep)
}

fn swarm_denial_snapshot_until_with(
    paths: &Paths,
    deadline: Instant,
    mut wait: impl FnMut(Duration),
) -> crate::Result<SwarmDenialSnapshot> {
    let mut backoff = Duration::from_millis(1);
    let mut pinned = None;
    loop {
        match observe_denied_nodes(paths, pinned.as_ref(), || {})? {
            DeniedObservation::Answer(answer) => return Ok(answer),
            DeniedObservation::Busy(config) => {
                if matches!(pinned, None | Some(None)) {
                    pinned = Some(config);
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(SwarmDenialSnapshot::Unavailable(
                SwarmDenialUnavailable::WriterBusy,
            ));
        }
        wait(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

/// Read the current verified swarm roster, waiting only for
/// a concurrent vault writer until `deadline`, so a writer mid-apply is never read as "no
/// roster". The shared observation is held across the read and released before returning the
/// owned roster. `Ok(None)` is what the applied copy says (no vault configured here, no applied
/// copy, a copy that names no roster), never a writer's moment.
///
/// # Errors
/// Local state or the verified cache cannot be read safely, the roster is malformed, or a
/// writer still holds the state lock at `deadline`
/// ([`crate::ErrorKind::VaultBusy`]).
pub fn swarm_roster_until(
    paths: &Paths,
    deadline: Instant,
) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
    #[cfg(any(test, debug_assertions))]
    if UNWAITED_ROSTER.with(std::cell::Cell::get) {
        return super::test_unwaited_roster(paths);
    }
    swarm_roster_until_with(paths, deadline, std::thread::sleep)
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static UNWAITED_ROSTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run a negative control with the old roster read that answers absence under a writer.
#[cfg(any(test, debug_assertions))]
pub fn test_without_roster_wait<T>(read: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNWAITED_ROSTER.with(|old| old.set(self.0));
        }
    }
    let _restore = Restore(UNWAITED_ROSTER.with(|old| old.replace(true)));
    read()
}

fn swarm_roster_until_with(
    paths: &Paths,
    deadline: Instant,
    mut wait: impl FnMut(Duration),
) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
    let mut backoff = Duration::from_millis(1);
    loop {
        match StateLock::observe_narrowed(paths)? {
            StateObservation::Held(lock) => return super::roster::swarm_roster_held(paths, &lock),
            // No lock file: no copy was ever applied here, so no roster either way
            // ([`super::roster::with_roster_observation`]'s absent arm).
            StateObservation::Absent => return Ok(None),
            StateObservation::Busy => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::of_kind(
                crate::ErrorKind::VaultBusy,
                "this Mac's swarm roster is busy: a vault writer still holds its state lock",
            ));
        }
        busy_observed();
        wait(backoff.min(remaining));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

enum DeniedObservation {
    Answer(SwarmDenialSnapshot),
    Busy(Option<Config>),
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static DENIED_NODES_BUSY: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// A fixture's action ([`test_with_denied_nodes_busy`]) where a reader met a busy state lock.
fn busy_observed() {
    #[cfg(any(test, debug_assertions))]
    DENIED_NODES_BUSY.with(|slot| {
        if let Some(busy) = slot.borrow_mut().as_mut() {
            busy();
        }
    });
}

/// Run a fixture's callback after the deny reader observes a busy state lock, and before the
/// roster's bounded reader ([`swarm_roster_until`]) waits out one.
/// This observes contention; it does not change whether the reader waits.
#[cfg(any(test, debug_assertions))]
pub fn test_with_denied_nodes_busy<R>(busy: impl FnMut() + 'static, read: impl FnOnce() -> R) -> R {
    struct Restore(Option<Box<dyn FnMut()>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DENIED_NODES_BUSY.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(DENIED_NODES_BUSY.with(|slot| slot.replace(Some(Box::new(busy)))));
    read()
}

/// A fixture's exclusive lock on the real vault state, released when dropped.
#[cfg(any(test, debug_assertions))]
pub struct TestStateLock {
    _held: StateLock,
}

/// Hold the same exclusive state lock as a vault apply in an isolated fixture.
///
/// # Errors
/// The state lock cannot be opened or acquired.
#[cfg(any(test, debug_assertions))]
pub fn test_hold_state_lock(paths: &Paths) -> crate::Result<TestStateLock> {
    Ok(TestStateLock {
        _held: StateLock::take(paths)?,
    })
}

/// Read the deny list, with a witness after the reserved-entry read for a test that releases a
/// concurrent writer at that point.
fn swarm_denied_nodes_then(
    paths: &Paths,
    after_entry: impl FnOnce(),
) -> crate::Result<Option<Vec<String>>> {
    Ok(swarm_denial_snapshot_then(paths, after_entry)?.into_nodes())
}

fn swarm_denial_snapshot_then(
    paths: &Paths,
    after_entry: impl FnOnce(),
) -> crate::Result<SwarmDenialSnapshot> {
    match observe_denied_nodes(paths, None, after_entry)? {
        DeniedObservation::Answer(answer) => Ok(answer),
        DeniedObservation::Busy(_) => Ok(SwarmDenialSnapshot::Unavailable(
            SwarmDenialUnavailable::WriterBusy,
        )),
    }
}

fn observe_denied_nodes(
    paths: &Paths,
    pinned: Option<&Option<Config>>,
    after_entry: impl FnOnce(),
) -> crate::Result<DeniedObservation> {
    // Before the early answers below, so a deny read narrows the lock on every path, and opens it
    // twice where it goes on to observe; a roster read's `observe_narrowed` opens it once.
    StateLock::narrow_owned(paths)?;
    let configured = config(paths)?;
    if let Some(Some(expected)) = pinned
        && configured.as_ref() != Some(expected)
    {
        return Ok(DeniedObservation::Answer(SwarmDenialSnapshot::Unavailable(
            SwarmDenialUnavailable::ConfigurationChanged,
        )));
    }
    let Some(expected) = configured else {
        if matches!(
            StateLock::observe_classified(paths)?,
            StateObservation::Busy
        ) {
            busy_observed();
            return Ok(DeniedObservation::Busy(None));
        }
        return Ok(DeniedObservation::Answer(SwarmDenialSnapshot::Available(
            Vec::new(),
        )));
    };
    if expected.signer.is_none() {
        return Ok(DeniedObservation::Answer(SwarmDenialSnapshot::Available(
            Vec::new(),
        )));
    }
    let observation = StateLock::observe_classified(paths)?;
    if matches!(observation, StateObservation::Busy) {
        busy_observed();
        return Ok(DeniedObservation::Busy(Some(expected)));
    }
    held_denied_nodes(paths, &expected, observation, after_entry).map(DeniedObservation::Answer)
}

fn held_denied_nodes(
    paths: &Paths,
    expected: &Config,
    observation: StateObservation,
    after_entry: impl FnOnce(),
) -> crate::Result<SwarmDenialSnapshot> {
    if config(paths)?.as_ref() != Some(expected) {
        return Ok(SwarmDenialSnapshot::Unavailable(
            SwarmDenialUnavailable::ConfigurationChanged,
        ));
    }
    let state = load_state(paths)?;
    match state.membership {
        Stored::None | Stored::Pending | Stored::Plain => {
            return Ok(SwarmDenialSnapshot::Available(Vec::new()));
        }
        Stored::Revoked => {
            return Ok(SwarmDenialSnapshot::Unavailable(
                SwarmDenialUnavailable::Revoked,
            ));
        }
        Stored::In => {}
    }
    // Keep the same snapshot across the entry, its absence, and the membership log. A second
    // free-lock observation cannot make an earlier unavailable entry into an absent one.
    let StateObservation::Held(_observation) = observation else {
        return Ok(SwarmDenialSnapshot::Unavailable(
            SwarmDenialUnavailable::MissingStateLock,
        ));
    };
    let entry = entry::observe_reserved_entry(paths, expected, SWARM_ROSTER_ENTRY)?;
    after_entry();
    let Some(entry) = entry else {
        return Ok(SwarmDenialSnapshot::Unavailable(
            SwarmDenialUnavailable::EntryUnavailable,
        ));
    };
    if let Some(entry) = entry.entry {
        let (bytes, _) = entry.into_parts();
        let text = std::str::from_utf8(bytes.as_slice())
            .map_err(|_| Error::new(format!("{SWARM_ROSTER_ENTRY} is not UTF-8")))?;
        let mut denied = parse_denied_nodes_toml(text)?;
        let mut parsed_roster = crate::swarm_roster::SwarmRoster::from_toml(text).ok();
        let local_log = log_entries::trusted_local_log(paths, expected)?;
        // A Mac the log let back in (Allow again, a quorum Undo): the deny rows its removal wrote
        // no longer hold for it (`roster::restored_by_log`).
        let mut active_roster_nodes = BTreeSet::new();
        if let Some(log) = &local_log {
            let (_, nodes) = super::roster::restored_by_log(log);
            active_roster_nodes.extend(nodes);
            if let Some(roster) = parsed_roster.as_mut() {
                let _ = super::roster::lift_restored_from_roster(log, roster);
                let remaining_deny_nodes: BTreeSet<String> = roster
                    .deny_nodes
                    .iter()
                    .map(|n| n.trim().to_ascii_lowercase())
                    .collect();
                denied.retain(|node| {
                    remaining_deny_nodes.contains(&node.trim().to_ascii_lowercase())
                });
            }
        }
        if let Some(roster) = &parsed_roster {
            let is_removed_in_log = |m: &crate::swarm_roster::SwarmMember| {
                local_log.as_ref().is_some_and(|log| {
                    let nk_removed = m.node_key.as_deref().is_some_and(|nk_hex| {
                        log.membership().nodes().iter().any(|(nk, standing)| {
                            *nk != [0; 32]
                                && matches!(
                                    standing,
                                    crate::membership_log::NodeStanding::Removed(_)
                                )
                                && aterm_codec::hex::encode_lower(nk)
                                    .eq_ignore_ascii_case(nk_hex.trim())
                        })
                    });
                    nk_removed
                        || Recipient::parse(&m.recipient).is_ok_and(|rec| {
                            log.recipient_is_removed(&rec)
                                || log.audience_left_out_recipient(
                                    &crate::membership_log::EpochAudience::Everyone,
                                    &rec,
                                )
                                || log_entries::recipient_denied_by_roster_or_log(
                                    Some(roster),
                                    log,
                                    &rec,
                                )
                        })
                })
            };
            for m in &roster.members {
                if !is_removed_in_log(m) && !steward::member_is_denied_in_roster(roster, m) {
                    if let Some(node_id) = &m.node_id {
                        active_roster_nodes.insert(node_id.trim().to_ascii_lowercase());
                    }
                    if let Some(node_key) = &m.node_key {
                        active_roster_nodes.insert(node_key.trim().to_ascii_lowercase());
                    }
                }
            }
            for member in &roster.members {
                if is_removed_in_log(member) || steward::member_is_denied_in_roster(roster, member)
                {
                    for raw in [&member.node_id, &member.node_key].into_iter().flatten() {
                        let trimmed = raw.trim_ascii().to_ascii_lowercase();
                        if !trimmed.is_empty() && !active_roster_nodes.contains(&trimmed) {
                            denied.push(trimmed);
                        }
                    }
                }
            }
        }
        if let Some(log) = &local_log {
            extend_denied_nodes_from_log(log, parsed_roster.as_ref(), &mut denied);
            denied.retain(|node| !active_roster_nodes.contains(&node.trim().to_ascii_lowercase()));
        }
        denied.sort();
        denied.dedup();
        return Ok(SwarmDenialSnapshot::Available(denied));
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(SwarmDenialSnapshot::Unavailable(
            SwarmDenialUnavailable::MissingIdentity,
        ));
    };
    let local_log = log_entries::trusted_local_log(paths, expected)?;
    if local_log.as_ref().is_some_and(|log| {
        log.recipient_is_removed(&identity.recipient())
            || log_entries::recipient_denied_by_roster_or_log(None, log, &identity.recipient())
            || log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
    }) {
        return Ok(SwarmDenialSnapshot::Unavailable(
            SwarmDenialUnavailable::RecipientHeldOut,
        ));
    }
    let mut denied = Vec::new();
    if let Some(log) = &local_log {
        extend_denied_nodes_from_log(log, None, &mut denied);
        denied.sort();
        denied.dedup();
    }
    Ok(SwarmDenialSnapshot::Available(denied))
}

fn extend_denied_nodes_from_log(
    log: &crate::membership_log::Log,
    roster: Option<&crate::swarm_roster::SwarmRoster>,
    denied: &mut Vec<String>,
) {
    let membership = log.membership();
    let active_nodes: BTreeSet<[u8; 32]> = membership
        .members()
        .iter()
        .filter(|(actor, member)| {
            !log.recipient_is_removed(&member.binding.recipient())
                && !log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
                && !roster.is_some_and(|r| log_entries::member_denied_by_roster(r, member))
        })
        .filter_map(|(_, member)| member.node_key.filter(|k| *k != [0; 32]))
        .filter(|nk| {
            !matches!(
                membership.node(nk),
                crate::membership_log::NodeStanding::Removed(_)
            )
        })
        .chain(
            membership
                .unproved()
                .iter()
                .filter(|founder| {
                    !log.recipient_is_removed(&founder.recipient)
                        && !log.audience_left_out_recipient(
                            &crate::membership_log::EpochAudience::Everyone,
                            &founder.recipient,
                        )
                        && !roster.is_some_and(|r| {
                            r.is_label_denied(&founder.label)
                                || r.is_label_denied(&founder.name)
                                || founder.node_key.filter(|k| *k != [0; 32]).is_some_and(|k| {
                                    r.is_node_denied(&aterm_codec::hex::encode_lower(&k))
                                })
                        })
                })
                .filter_map(|founder| founder.node_key.filter(|k| *k != [0; 32]))
                .filter(|nk| {
                    !matches!(
                        membership.node(nk),
                        crate::membership_log::NodeStanding::Removed(_)
                    )
                }),
        )
        .collect();
    for (nk, standing) in membership.nodes() {
        if matches!(standing, crate::membership_log::NodeStanding::Removed(_)) && *nk != [0; 32] {
            denied.push(aterm_codec::hex::encode_lower(nk));
        }
    }
    for (actor, member) in membership.members() {
        let roster_denied = roster.is_some_and(|r| log_entries::member_denied_by_roster(r, member));
        if (log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
            || roster_denied)
            && let Some(nk) = member
                .node_key
                .filter(|k| *k != [0; 32] && !active_nodes.contains(k))
        {
            denied.push(aterm_codec::hex::encode_lower(&nk));
        }
    }
    for founder in membership.unproved() {
        let roster_denied = roster
            .is_some_and(|r| r.is_label_denied(&founder.label) || r.is_label_denied(&founder.name));
        if (log.audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Everyone,
            &founder.recipient,
        ) || roster_denied)
            && let Some(nk) = founder
                .node_key
                .filter(|k| *k != [0; 32] && !active_nodes.contains(k))
        {
            denied.push(aterm_codec::hex::encode_lower(&nk));
        }
    }
}

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;
