// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client flows and admin workflows for `aterm` secrets.

use crate::age::{self, Identity, Recipient};
use crate::envelope::{self, Envelope, Floor, Locker, Pin, Retired, Unsigned, Verdict};
use crate::materialize::{self, Manifest, Report};
use crate::text::{self, Audience, Document, Entry};
use crate::transport::{self, Fetched, Source};
use crate::{Error, SecretBytes, fsx, join};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod account;
pub use account::AccountIssuerSuccession;
mod account_input;
mod alab_join;
pub use account_input::{admin_set_entry_until, open_account_input};
pub use alab_join::{
    NativeAdmission, NativeBootstrap, check_bootstrap_allowed, native_bootstrap,
    on_bootstrap_allowed, on_native_bootstrap, prepare_alab_join_retry,
};
mod admin_keys;
pub mod admin_service;
mod admission;
mod admit;
mod bounded;
mod chain_cache;
#[cfg(any(test, debug_assertions))]
mod commons_excluded;
mod continuity;
mod control_recovery;
mod creation;
pub mod custodian;
mod enrollment;
mod entry;
mod fan_out;
mod fleet;
mod foundation;
mod genesis;
mod handover;
mod http204_observation;
mod inspection;
mod join_queues;
mod join_routing;
mod lab_labels;
mod ledger_publish;
mod locker_skew;
mod log_entries;
mod materialization_selection;
mod member_authority;
mod member_routes;
mod mirrors;
mod network_observation;
mod network_refresh;
mod open_migration;
mod own_head;
mod peer_candidates;
mod projection;
pub use projection::{
    ConvergenceInputs, ConvergenceRole, ConvergenceSnapshot, ConvergenceWake, IdentityDebt,
    convergence_inputs, on_convergence_inputs,
};
mod readmit;
mod reads;
mod records_cache;
pub mod release;
mod release_migration;
mod removal_plan;
mod remove;
mod retirement;
mod roster;
mod seat_ask;
mod steward;
mod subscriber;
mod successor;
mod toml_cache;
mod vault_memo;
pub mod version;
use admin_keys::parse_admin;
pub use admission::{Evidence as AdmissionEvidence, Method as AdmissionMethod};
pub use admit::{
    ByLog, LogAdmission, accepts_bundle, admit_by_phrase, admits_by_log, hand_over, hand_over_with,
    log_admitted_actors, member_by_log, post_records_to_servers, readmit_from_log_bundle,
    readmit_from_log_bundle_until, write_from_log_bundle,
};
pub(crate) use admit::{
    log_admitted_actors_read, log_seated, readmit_from_log_bundle_admitted, seated_member,
    with_proof_admission,
};
pub use bounded::{join_if_unchanged_until, sync_reader_until};
#[cfg(any(test, debug_assertions))]
pub use commons_excluded::{CommonsExcluded, test_commons_excluded};
pub use continuity::{Split, SplitBy, seat_splits, splits};
pub use control_recovery::{
    CommonsRecovery, CommonsRecoveryCapacity, CommonsRecoveryPlan, CommonsRecoveryRecipient,
    plan_commons_control_recovery, restore_commons_control,
};
pub use enrollment::{admin_cached, admin_cached_until};
pub use entry::{
    EntryAdmission, EntryClaim, EntryClaimRelation, EntryObservation, EntryOrigin, EntryRead,
    EntryReader, EntrySnapshot, EntryVersion, MAX_ENTRY_CLAIM_BYTES, PublicRosterPair,
    PublicRosterRead, VerifiedEntry, current_public_roster, observe_verified_entry,
    read_account_bound_entry, read_verified_entry,
};
#[path = "client/mailbox_index.rs"]
mod mailbox_index;
pub(crate) use entry::read_claude_token_until;
#[cfg(any(test, debug_assertions))]
pub use fleet::test_without_roster_wait;
pub use fleet::{
    AlabFleet, FLEET_CODE_ENTRY, SWARM_ROSTER_ENTRY, SwarmDenialSnapshot, SwarmDenialUnavailable,
    admin_publish_alab_fleet, alab_fleet, alab_fleet_code, parse_denied_nodes_toml,
    swarm_denial_snapshot, swarm_denial_snapshot_until, swarm_denied_nodes, swarm_roster_until,
};
#[cfg(any(test, debug_assertions))]
pub use fleet::{test_alab_fleet, test_hold_state_lock, test_with_denied_nodes_busy};
pub use foundation::{
    CommonsInit, Foundable, NotFounded, admin_commons_found_by_default, admin_commons_init,
    commons_foundable,
};
pub(crate) use genesis::configured_genesis;
pub use genesis::{SignedGenesis, admin_sign_genesis, seed_vault_genesis};
#[cfg(any(test, debug_assertions))]
pub use genesis::{test_found, test_sign_genesis};
pub use handover::{
    HANDOVER_LOCK_WAIT, HandoverHold, ask_allowed, handover_bundle, holds_allowed, post_allowed,
    take_handover_hold, write_from_bundle,
};
pub use http204_observation::{VaultHttp204Response, observe_http204_response};
#[cfg(test)]
pub(crate) use inspection::newest_everyone;
pub(crate) use inspection::newest_everyone_until;
pub use inspection::{
    CommonsControlInspection, MembershipHistoryInspection, inspect_membership_history_until,
};
pub use lab_labels::{LabLabels, LabelHold, lab_labels};
pub use ledger_publish::{
    MergedHeads, ObservedHead, extend_parent_spine, ledger_publish_environment,
    merge_diverged_heads, publish_to_servers, read_server_heads, verify_approved_in_served_head,
};
pub(crate) use log_entries::local_swarm_roster_for_log_held;
pub use log_entries::{
    Anchor, FIRST_ANCHOR, MemberEntry, MemberUnset, MemberWritten, OnBusy, RESEAL_REFUSAL, WaitFor,
    WriteRoute, member_set, member_unset, per_host_shadows, write_route,
};
pub use log_entries::{
    CommonsLog, GAP_STEP_MS, Overtaken, Overtaker, REMOVER_GRACE_MS, SETTLE_GRACE_MS,
    acknowledged_commons_log, admin_mirror_log, admit_member_in_log, applied_version_names_log,
    check_log_commons_enrollment, configure_log_member, enroll_commons_member_in_log,
    keyed_epochs_environment, local_swarm_roster_for_log, member_denied_by_roster,
    publish_local_chosen_name, publish_local_node_key, remove_member_in_log, trusted_commons_log,
};
#[cfg(any(test, debug_assertions))]
pub use log_entries::{TestPassClaim, test_hold_pass_claim};
pub(crate) use log_entries::{member_unset_names, member_waits_here, member_write};
pub use mailbox_index::{
    mailbox_component_held, read_mailbox_component, refresh_mailbox_component_index,
};
pub use member_authority::member_authority;
pub use mirrors::{
    MAX_MIRRORS, MirrorProbe, mirror_notes, mirror_typed, probe_mirror, set_mirrors,
};
pub use network_observation::{VaultNetworkMaterialization, VaultNetworkSource};
pub use network_refresh::{
    VaultNetworkRefresh, VaultNetworkRefreshOutcome, observe_network_refresh,
};
use peer_candidates::{PeerDiscovery, PeerUse};
pub use readmit::{
    AUTOMATIC_PER_HOUR, Book, How as ReadmitHow, KeptSeat, MAX_READMISSIONS,
    ProofAdmissionCapacity, Proven, READMIT_EVERY_MS, Readmit, admit_by_proof, kept_seats,
    prove_at, prove_at_ms, refuses_before_possession, refuses_before_possession_at_ms,
};
use reads::{HeldLog, Reads};
pub use remove::{
    CutPass, ListedMac, MemberRemoval, NodePass, NodePassHost, NodePassPace, NodePassReceipt,
    NodePassSaid, RemovedNote, RemovedNotes, TableCuts, UndoOffer, UndoShare, cut_own_removals,
    epoch_convergence_environment, gap_heal_environment, member_list, node_pass_receipt,
    on_table_cuts, read_after_cutoff_lines, remove_member, remove_seat, removed_notes,
    removes_by_log, reseal_waits, revoke_by_log, run_node_pass, seats_ever, tend_epochs,
    tend_epochs_at, undo_offers, undo_removals, vault_admin_named, vault_pass_at,
};
pub(crate) use remove::{recipient_label, seat_label};
#[cfg(any(test, debug_assertions))]
pub use remove::{test_put_everyone, test_put_everyone_draft, test_remove_unposted};
#[cfg(any(test, debug_assertions))]
pub use retained_history::{
    RetainedHistoryReadCounts, RetainedHistoryReadOutcomes, test_retained_history_read_counts,
};
pub use retirement::{
    Retirement, Revocation, StolenRevocation, admin_retire, admin_revoke, admin_revoke_stolen,
    machine_named_in_vault, repair_half_done_revokes, rotate_fleet_code_text,
};
#[cfg(any(test, debug_assertions))]
pub use roster::admin_seed_swarm_roster;
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub use roster::test_roster_observations;
#[cfg(any(test, debug_assertions))]
pub use roster::test_unwaited_roster;
pub use roster::{historical_swarm_roster, resolve_local_swarm_label, swarm_lease_holder};
pub(crate) use roster::{vault_busy, with_roster_observation, with_roster_observation_or_busy};
#[cfg(any(test, debug_assertions))]
pub use seat_ask::test_read_dropped;
pub use seat_ask::{
    Seat, accepts_seat, dropped, dropped_words, out_since, out_words, seat, seat_asks, take_seat,
};
pub(crate) use steward::local_recipient_denial_held;
#[cfg(any(test, debug_assertions))]
pub use steward::test_alab_standing;
pub(crate) use steward::{AlabRoleContext, AlabRoleObservation, observe_alab_role};
pub use steward::{
    StewardLease, StewardRevoke, StewardStandbyServer, Stewardship,
    local_recipient_denial_observed, local_recipient_denied_in_roster, no_admin_key,
    retire_stale_member_server, stale_member_server, steward_alab_policy_role,
    steward_ensure_lease, steward_ensure_standby_server, steward_finish_revokes,
    steward_lease_check, steward_revoke_check, steward_standby_server_check, stewardship,
};
pub use subscriber::mend_subscriber;
#[cfg(any(test, debug_assertions))]
pub use successor::SecondSuccessor;
pub use successor::{Succeeding, Succession, admit_successor};
pub(crate) use vault_memo::{
    Currency, Moment, Under, local_recipient_denied_under, observe_commons_now,
    observe_commons_quiet, state_standing_in, trusted_commons_log_under,
};
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub use vault_memo::{
    Derivations, LookDerivations, test_logs_read, test_look_derivations, test_looks_observed,
};
pub use vault_memo::{
    DeskInputs, VaultLook, acknowledged_commons_log_now, config_now, desk_inputs,
    desk_inputs_quiet, local_swarm_roster_for_log_now, records_now, swarm_denial_snapshot_now,
    swarm_denied_nodes_now, swarm_member_heard_now, swarm_roster_now_until,
    trusted_commons_log_now,
};
#[cfg(test)]
mod approver_queue_tests;
#[cfg(test)]
pub(crate) use approver_queue_tests::{
    mirror_vault as mirrored_vault_for_test, request as queued_identity_for_test,
};
#[cfg(test)]
mod birth_tests;
#[cfg(test)]
mod desk_node_tests;
#[cfg(test)]
mod join_elsewhere_tests;
#[cfg(test)]
mod join_tests;
#[cfg(test)]
mod log_entries_tests;
#[cfg(test)]
pub(crate) use log_entries_tests::two_macs as two_log_macs_for_test;
#[cfg(test)]
pub(crate) use log_entries_tests::two_macs_with as two_log_macs_with_for_test;
#[cfg(test)]
mod own_head_tests;
#[cfg(test)]
mod parent_history_tests;
#[cfg(test)]
mod peer_candidates_tests;
mod peer_selection;
#[cfg(test)]
mod peer_selection_tests;

#[cfg(test)]
mod cold_read_tests;
#[cfg(test)]
mod entry_class_tests;
#[cfg(test)]
mod envelope_denial_tests;
#[cfg(test)]
mod idle_reads_tests;
#[cfg(test)]
mod lab_start_over_tests;
#[cfg(test)]
mod log_only_tests;
#[cfg(test)]
mod observe_reads_tests;
#[cfg(test)]
mod probe_reads_tests;
#[cfg(test)]
mod received_records_tests;
#[cfg(test)]
mod records_push_tests;
#[cfg(test)]
mod records_sync_tests;

/// Where a machine keeps its vault state, under one home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The home directory (`~`).
    pub home: PathBuf,
    /// `~/.config/aterm/secrets`.
    pub dir: PathBuf,
}

impl Paths {
    /// Locate the paths under `home` without enrolling, retiring or moving any
    /// identity. A read-only observer uses this rather than [`Self::for_home`].
    pub fn locate_for_home(home: &Path) -> Paths {
        Paths {
            home: home.to_path_buf(),
            dir: home.join(".config").join("aterm").join("secrets"),
        }
    }

    /// Locate this user's paths without running identity initialization.
    /// # Errors
    /// Returns an error when the user's home directory is unavailable.
    pub fn locate() -> crate::Result<Paths> {
        aterm_types::dirs::home_dir()
            .map(|home| Self::locate_for_home(&home))
            .ok_or_else(|| Error::new("no home directory"))
    }

    /// The paths under `home`. Where `home` is this user's home, the first such call in a process
    /// sets a lab identity's vault folder aside where this process would make an unmarked identity
    /// ([`start_over_live`]), so each use of this Mac's real vault folder after it sees the Mac it
    /// starts as. Another home (a test's, a scratch one) is left as it is.
    pub fn for_home(home: &Path) -> Paths {
        let paths = Self::locate_for_home(home);
        static STARTED_OVER: std::sync::Once = std::sync::Once::new();
        if !STARTED_OVER.is_completed()
            && aterm_types::dirs::home_dir().is_some_and(|live| live == home)
        {
            STARTED_OVER.call_once(|| start_over_live(&paths));
        }
        paths
    }

    /// The paths under this user's home ([`Paths::for_home`]).
    pub fn live() -> crate::Result<Paths> {
        aterm_types::dirs::home_dir()
            .map(|home| Paths::for_home(&home))
            .ok_or_else(|| Error::new("no home directory"))
    }

    /// The files whose change can move this Mac's standing in its vault (in, asking, let in,
    /// let go): its source, its saved state, its identity, the Allowed a member handed it
    /// (which moves it from asking to let in), and the trusted log, which decides a seat: the
    /// membership records' entry, its pin and intent, and a layout's header and data file
    /// ([`crate::records_storage::standing_files`]). A reader that shows the standing can
    /// watch them rather than poll. The log's own files are among them because a record that
    /// arrives is put on the end of the data file in place ([`persist_records`]), which
    /// changes no entry of the vault folder, and because the saved state is left as it is by a
    /// round that changed nothing ([`save_state`]): neither stands in for a change to the log.
    /// The header and data file are named whatever the records entry is, since a watcher takes
    /// this list once: while the entry is a plain file they are paths under a file (a lookup
    /// answers `ENOTDIR`), which a watcher takes as files not there yet, and the vault folder
    /// hears the entry replaced by a layout. The source comes first.
    #[must_use]
    pub fn standing_files(&self) -> Vec<PathBuf> {
        let mut files = vec![
            self.source_file(),
            self.state_file(),
            self.identity_file(),
            handover::allowed_file(self),
        ];
        if let Ok(records) = crate::records_storage::standing_files(&self.records_file()) {
            files.extend(records);
        }
        files
    }

    fn source_file(&self) -> PathBuf {
        self.dir.join("source.toml")
    }

    fn identity_file(&self) -> PathBuf {
        self.dir.join("identity")
    }

    pub(crate) fn state_file(&self) -> PathBuf {
        self.dir.join("state.toml")
    }

    pub(crate) fn cache_file(&self) -> PathBuf {
        self.dir.join("vault")
    }

    /// The raw bytes of the last version this Mac sealed that a server took, kept while it carries
    /// an approval the servers lost at a heal ([`own_head`]). Nothing clears it: one of another
    /// vault, another pin or another identity fails verification and is never read.
    fn published_file(&self) -> PathBuf {
        self.dir.join("vault.published")
    }

    /// The digests a vault pass last looked at for [`Self::published_file`]'s carry
    /// ([`own_head::due`]).
    fn carry_tried_file(&self) -> PathBuf {
        self.dir.join("vault.published.tried")
    }

    /// The Macs [`Self::published_file`]'s carry let back in, carried again while a newest
    /// `[everyone]` epoch leaves one unwrapped ([`own_head::lost_to`]).
    fn carried_file(&self) -> PathBuf {
        self.dir.join("vault.published.carried")
    }

    fn lock_file(&self) -> PathBuf {
        self.dir.join(".lock")
    }

    fn publish_lock_file(&self) -> PathBuf {
        self.dir.join(".publish.lock")
    }

    fn asking_file(&self) -> PathBuf {
        self.dir.join("asking")
    }

    fn answered_file(&self) -> PathBuf {
        self.dir.join("answered")
    }

    /// The queue file of the approver's record ([`read_answered`]), beside its requests so a
    /// build before it, which reads `answered` under 64 KiB, always reads them whole.
    fn answered_queue_file(&self) -> PathBuf {
        self.dir.join("answered-queue")
    }

    /// The requests this Mac let in by code ([`record_approved`]): written only by an approval,
    /// read by the approver, so no write of one undoes the other's.
    fn answered_approved_file(&self) -> PathBuf {
        self.dir.join("answered-approved")
    }

    fn stuck_file(&self, pid: u32) -> PathBuf {
        self.dir.join(format!("{STUCK_PREFIX}{pid}"))
    }

    /// Where `aterm secrets serve` keeps its store by default.
    pub fn served(&self) -> PathBuf {
        self.dir.join("served")
    }

    /// The logical entry for this machine's verified membership-record log. Resolve its data
    /// with [`Self::records_data_file`]; an entry that fenced older writers is a directory.
    pub fn records_file(&self) -> PathBuf {
        self.dir.join("records")
    }

    /// The current data file selected by the records entry, without changing storage
    /// (`records_storage::reader_path`: a pinned layout standing as a reader last judged it is
    /// not read again).
    ///
    /// # Errors
    /// The layout is pending, unsafe, missing its current file, inconsistent with its pin, or
    /// requires a larger reader capacity than this build supports.
    pub fn records_data_file(&self) -> crate::Result<PathBuf> {
        crate::records_storage::reader_path(&self.records_file())
    }

    /// The crossed mark beside [`Self::records_file`] ([`crate::crossing::Mark`]): stored once
    /// this Mac's log crosses, never removed.
    pub(crate) fn crossed_file(&self) -> PathBuf {
        self.dir.join("records.crossed")
    }

    /// The files that hold the membership history this Mac acknowledged, which
    /// [`load_acknowledged_records`] reads besides the records: the enrollment claim and the
    /// journal's anchor, receipts and checkpoint. Every acknowledgement replaces the checkpoint,
    /// and one can leave [`Self::records_file`] as it was (a write's records made durable before
    /// it was cut short are acknowledged by the next writer), so a reader that keeps what it read
    /// until its files change watches these too.
    #[must_use]
    pub fn acknowledged_history_files(&self) -> [PathBuf; 4] {
        retained_history::files(self)
    }
}

/// A machine's configured source.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// Where the vault is: its one `url` (or path), the source a pass reads first and puts a lost
    /// copy back to.
    pub source: Source,
    /// Other addresses of the same vault (`mirrors` in `source.toml`), which a pass reads beside
    /// its other servers and never writes to, and no join reaches: only the entries [`config`] keeps (a URL of this
    /// vault's name, with no `#` pin, not the endpoint of `source` or of one before it, at most
    /// [`MAX_MIRRORS`]); none for a path. What a mirror serves is verified under the one pin and
    /// floor `source`'s is.
    pub mirrors: Vec<String>,
    /// The vault's name.
    pub name: String,
    /// The pinned signing key (None for a plain text file at a path).
    pub signer: Option<[u8; 32]>,
    /// Verified handoff keys (`oldest` first) through which `signer` was reached.
    pub followed: Vec<[u8; 32]>,
    /// The name this machine asked to be called.
    pub host: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mirrors: Vec<_> = self
            .mirrors
            .iter()
            .map(|url| transport::describe(url))
            .collect();
        f.debug_struct("Config")
            .field("source", &self.source)
            .field("mirrors", &mirrors)
            .field("name", &self.name)
            .field("signer", &self.signer)
            .field("followed", &self.followed)
            .field("host", &self.host)
            .finish()
    }
}

/// Whether `config` is the ALab vault: pinned to a key this build names for it
/// ([`crate::ALAB_SIGNER`], [`crate::ALAB_RETIRED`]), or reached from one by verified
/// handoffs, and not a pin that gives way to the current key ([`Retired::gives_way`]).
/// Its address does not decide: anyone can serve at any address, and only the signer says
/// whose vault it is.
#[must_use]
pub fn alab_vault(config: &Config) -> bool {
    config
        .signer
        .is_some_and(|signer| Retired::compiled().names(&signer, &config.followed))
}

/// Whether a requested join pin admits this configured authority without fetching.
pub fn pin_admitted(config: &Config, requested: Option<[u8; 32]>) -> bool {
    let Some(signer) = config.signer else {
        return requested.is_none() && matches!(config.source, Source::Path(_));
    };
    let retired = Retired::compiled();
    if retired.gives_way(&signer, &config.followed) {
        return false;
    }
    let Some(requested) = requested else {
        return true;
    };
    let requested = if retired.gives_way(&requested, &[]) {
        let Some(current) = retired.current else {
            return false;
        };
        current
    } else {
        requested
    };
    signer == requested || config.followed.contains(&requested)
}

/// Whether this machine is in the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Membership {
    /// No source configured.
    Unconfigured,
    /// Configured; asked to join; no locker has opened yet.
    Pending,
    /// A locker opened; this machine holds this sequence.
    In {
        /// The seq held.
        seq: u64,
        /// Stored source-processing marker in Unix seconds. Local replay and an unanswered
        /// long-poll timeout can advance it; this is not successful network-sync evidence.
        heard: u64,
    },
    /// It was in, and its latest version opens no locker for it: a `Remove` revoked it, or a
    /// rotation dropped it with no act against it that it can read ([`dropped`]), as an admin's
    /// revoke of a Mac that never took a seat reads here.
    Revoked {
        /// The newest version this Mac read while out; 0 for one that read only the log.
        seq: u64,
    },
    /// Reading a plain text file at a path.
    Plain,
}

/// What `aterm secrets status` shows. Names only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The source, if configured.
    pub config: Option<Config>,
    /// This machine's recipient and join code, if it has a key.
    pub machine: Option<(Recipient, String)>,
    /// Membership.
    pub membership: Membership,
    /// The entry names materialized here.
    pub names: Vec<String>,
}

impl Status {
    /// The command an admin Mac runs to let this Mac in while it waits to join: its code for a
    /// vault at a URL, whose server queues the request, or its key and name for a vault at a
    /// path, which has no queue (`aterm secrets approve <age1…> --name <host>`). `None` with no
    /// key yet.
    pub fn approve_command(&self) -> Option<String> {
        let (recipient, code) = self.machine.as_ref()?;
        Some(match &self.config {
            Some(Config {
                source: Source::Path(_),
                host,
                ..
            }) => format!(
                "aterm secrets approve {} --name {host}",
                recipient.to_bech32()
            ),
            _ => format!("aterm secrets approve {code}"),
        })
    }
}

/// How a join should behave.
pub struct JoinOptions<'a> {
    /// URL (optionally `#<signer>`), path, or empty for the ALab default.
    pub source: String,
    /// The machine name to ask for (default: [`default_host_name`]).
    pub host: Option<String>,
    /// Whether a person is at a terminal for fingerprint confirmation and password prompts.
    pub interactive: bool,
    /// How long to wait for the approval after asking (zero: return at once, pending).
    pub wait: Duration,
    /// Shown a fingerprint, answers whether to trust it (interactive only).
    pub confirm: &'a dyn Fn(&str) -> bool,
    /// The vault's join password, for when this machine is not in yet and the source is a URL
    /// (an admin Mac then approves within seconds); none waits for an admin to approve the
    /// code.
    pub password: JoinPassword<'a>,
    /// What the caller already fetched from the source (a [`Probed::envelope`]): the join
    /// verifies and applies it in place of a fetch of its own, unless this machine holds a
    /// newer version of that vault.
    pub fetched: Option<&'a [u8]>,
    /// Called once the request is queued and before any wait.
    pub requested: &'a dyn Fn(),
    /// Each step, as it happens (never a value): its `Display` is the line a person reads.
    pub say: &'a dyn Fn(&JoinEvent<'_>),
}

/// Where a join's password comes from ([`JoinOptions::password`]).
#[derive(Clone)]
pub enum JoinPassword<'a> {
    /// What the caller holds already (`None`: an approval by code). The join stretches it on
    /// a thread while its fetch is out.
    Given(Option<SecretBytes>),
    /// Asked once, when this machine is not in yet and the source is a URL (a terminal's
    /// prompt): `None` waits for an admin to approve the code.
    Ask(&'a dyn Fn() -> Option<SecretBytes>),
}

/// A step of a join ([`JoinOptions::say`]): names, codes and keys only, never a value. Its
/// `Display` is the line a person reads, so a caller prints each one, or renders one line of
/// its own from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinEvent<'a> {
    /// The vault verified, and this machine holds its key: the name it asks for, its join code
    /// (what an admin compares) and its `age1…` key.
    Machine {
        /// The machine name.
        host: &'a str,
        /// Its join code.
        code: &'a str,
        /// Its `age1…` key.
        recipient: &'a str,
    },
    /// A request to join `vault` is queued at its URL: with a proof of the join password,
    /// which an admin Mac's subscriber approves, or for an approval by `code`.
    Asked {
        /// The vault's name.
        vault: &'a str,
        /// This machine's join code.
        code: &'a str,
        /// Whether the request proves the join password.
        by_password: bool,
    },
    /// How a mistyped join password is fixed, said as the request with it is queued.
    Retype(&'a str),
    /// Not in the vault at a path yet: an admin approves this key under this name.
    NotInPath {
        /// The vault's name.
        vault: &'a str,
        /// This machine's `age1…` key.
        recipient: &'a str,
        /// The machine name.
        host: &'a str,
    },
    /// The join waits for the approval ([`JoinOptions::wait`]).
    Waiting,
    /// A pass of that wait failed; the wait goes on.
    StillWaiting(&'a Error),
    /// An admin Mac refused the request: its join password is not the vault's.
    Refused {
        /// The vault's name.
        vault: &'a str,
    },
}

impl std::fmt::Display for JoinEvent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinEvent::Machine {
                host,
                code,
                recipient,
            } => write!(f, "this machine: {host}, code {code}, key {recipient}"),
            JoinEvent::Asked {
                vault,
                by_password: true,
                ..
            } => write!(
                f,
                "asked to join {vault} with the join password; an admin Mac's subscriber approves it"
            ),
            JoinEvent::Asked { vault, code, .. } => write!(
                f,
                "asked to join {vault}. On an admin Mac run: aterm secrets approve {code}"
            ),
            JoinEvent::Retype(hint) => f.write_str(hint),
            JoinEvent::NotInPath {
                vault,
                recipient,
                host,
            } => write!(
                f,
                "not in {vault} yet. On an admin Mac run: aterm secrets approve {recipient} --name {host}"
            ),
            JoinEvent::Waiting => f.write_str(
                "waiting for the approval (Ctrl-C stops waiting; this Mac joins by itself once approved)",
            ),
            JoinEvent::StillWaiting(error) => write!(f, "still waiting ({error})"),
            JoinEvent::Refused { vault } => write!(
                f,
                "an admin Mac refused the join password: it is not {vault}'s"
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum ExpectedConfig<'a> {
    Any,
    Read(Option<&'a Config>),
    AdmittedAny(&'a (dyn Fn() -> crate::Result<()> + Sync)),
    AdmittedRead(
        Option<&'a Config>,
        &'a (dyn Fn() -> crate::Result<()> + Sync),
    ),
    /// A background join over the source read, held whenever sharing is off.
    WhileSharing(Option<&'a Config>),
}

impl ExpectedConfig<'_> {
    fn check(self, paths: &Paths) -> crate::Result<()> {
        self.check_sharing(paths)?;
        let expected = match self {
            Self::Any | Self::AdmittedAny(_) => return Ok(()),
            Self::Read(expected)
            | Self::AdmittedRead(expected, _)
            | Self::WhileSharing(expected) => expected,
        };
        if config(paths)?.as_ref() != expected {
            return Err(Error::new(
                "the configured vault changed after it was read; try again",
            ));
        }
        Ok(())
    }

    /// After this join saved its configuration, only its automatic permission is checked.
    /// The original source expectation no longer describes that committed state.
    fn check_sharing(self, paths: &Paths) -> crate::Result<()> {
        if let Self::AdmittedAny(current) | Self::AdmittedRead(_, current) = self {
            current()?;
        }
        if matches!(self, Self::WhileSharing(_)) && !crate::swarm::enabled_in_home(&paths.home)? {
            return Err(Error::new("sharing is off; ALab bootstrap is paused"));
        }
        Ok(())
    }
}

/// How a join ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    /// In, and synced.
    Joined {
        /// The seq applied.
        seq: u64,
        /// Names written or already right.
        names: Vec<String>,
    },
    /// Asked to join; an admin must approve. `code` is what the admin compares.
    Pending {
        /// This machine's code.
        code: String,
        /// The machine name it asked for.
        host: String,
    },
    /// Configured to read a plain text file; synced.
    Plain {
        /// Names written or already right.
        names: Vec<String>,
    },
}

/// How a sync ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// A newer version, applied.
    Applied {
        /// The seq.
        seq: u64,
        /// What changed.
        report: Report,
    },
    /// Nothing newer.
    Unchanged {
        /// The seq held.
        seq: u64,
    },
    /// The source held an older version than this machine verified, so this machine's copy was
    /// put back.
    PutBack {
        /// The seq held, and now the source's.
        seq: u64,
        /// The seq the source held, `None` when it held none.
        served: Option<u64>,
    },
    /// No locker opens for this machine (yet, or any more).
    NotIn,
}

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// How closely `state.toml` follows the moment the vault last answered this Mac
/// ([`State::heard`]): a minute. An answer that changes nothing else within a minute of the
/// one the state names leaves the state, and so its file ([`save_state`]), as it is; the
/// subscriber hears its vault about every 25 s, and the state names an answer at most this
/// much older than the last. What reads it judges it by the day ([`swarm_member_heard_now`]'s
/// callers) or shows it to a person ([`status`]).
const HEARD_GRAIN_SECS: u64 = 60;

/// When a state last heard at `heard` is heard now ([`heard_at`] by this Mac's clock).
fn heard_now(heard: u64) -> u64 {
    heard_at(now(), heard)
}

/// When a state last heard at `heard` is heard at `now`: `now`, or `heard` itself where that
/// is less than [`HEARD_GRAIN_SECS`] before it. A `heard` after `now` (the clock was set back)
/// is no moment the vault answered at: `now`.
const fn heard_at(now: u64, heard: u64) -> u64 {
    match now.checked_sub(heard) {
        Some(since) if since < HEARD_GRAIN_SECS => heard,
        _ => now,
    }
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

pub(crate) fn toml_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn decode_key32(text: &str) -> Option<[u8; 32]> {
    crate::b64::decode_padded(text).and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
}

/// Largest serialized client state accepted by the reader and writer, in bytes.
pub const MAX_STATE: usize = 4 * text::MAX_TEXT;

fn read_toml(
    path: &Path,
    what: &str,
    max: usize,
) -> crate::Result<Option<std::sync::Arc<aterm_toml::Table>>> {
    toml_cache::read(path, what, max)
}

/// A lock this process holds on the vault's state lock file (or, for a peer cursor, another lock
/// file), released when it drops.
pub(crate) struct StateLock {
    /// The locked descriptor, until this drops.
    file: Option<std::fs::File>,
    /// Whether this is a shared observation ([`StateLock::classify`]): no writer holds the lock
    /// while it does. Every other take is a writer's own, exclusive.
    shared: bool,
    /// The device and inode of the lock file an observation's descriptor (opened read-only)
    /// goes back to [`IDLE_LOCKS`] as once it is unlocked. A writer's take has none.
    idle_as: Option<(u64, u64)>,
}

/// Descriptors of vault state lock files that shared observations opened and that hold no lock,
/// each by its file's device and inode ([`IdleLocks`]). A quiet Commons round observes its vault's
/// state lock at every check of an access (`commons::roster::Access::is_current_observed`) and
/// every access a table session asks (`Bootstrap::access_in`), and each observation opened and
/// closed the lock file again: the `open` most of 14.6% of a sustained long round's on-CPU samples
/// and 17.9% of a short one's (2026-11-08). An observation takes one for the file at its lock's
/// path and judges it by the checks a file it opens gets ([`StateLock::open_observed`]), and puts
/// it back once unlocked. Each was opened read-only, close-on-exec and without following a final
/// symlink ([`StateLock::open_existing`]). In this crate's test process each test thread keeps
/// its own.
#[cfg(not(test))]
static IDLE_LOCKS: std::sync::Mutex<IdleLocks> = std::sync::Mutex::new(IdleLocks::new());

#[cfg(test)]
std::thread_local! {
    static THREAD_IDLE_LOCKS: std::cell::RefCell<IdleLocks> =
        const { std::cell::RefCell::new(IdleLocks::new()) };
}

/// How many idle lock descriptors [`IDLE_LOCKS`] keeps: a few for each of the one or two vaults a
/// process observes at once.
const IDLE_LOCKS_KEPT: usize = 8;

/// One idle descriptor: its lock file's device and inode, which name that file while the
/// descriptor holds it open (an open file's inode is not given to another), and when it came back.
struct IdleLock {
    id: (u64, u64),
    file: std::fs::File,
    returned: u64,
}

/// [`IDLE_LOCKS`]' descriptors, in fixed slots: taking or keeping one allocates and frees no
/// memory, so a read measured for what its thread allocates (`tests/denial_entry_memory.rs`)
/// frees nothing another read allocated.
struct IdleLocks {
    slots: [Option<IdleLock>; IDLE_LOCKS_KEPT],
    returns: u64,
}

impl IdleLocks {
    const fn new() -> Self {
        Self {
            slots: [const { None }; IDLE_LOCKS_KEPT],
            returns: 0,
        }
    }

    /// An idle descriptor of the file `id` names, taken out.
    fn take(&mut self, id: (u64, u64)) -> Option<std::fs::File> {
        self.slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|idle| idle.id == id))?
            .take()
            .map(|idle| idle.file)
    }

    /// Keep `file`, an unlocked descriptor of the file `id` names, in an empty slot or in place of
    /// the one that came back longest ago, which closes.
    fn keep(&mut self, id: (u64, u64), file: std::fs::File) {
        let returned = self.returns;
        self.returns = self.returns.wrapping_add(1);
        if let Some(slot) = self
            .slots
            .iter_mut()
            .min_by_key(|slot| slot.as_ref().map(|idle| idle.returned))
        {
            *slot = Some(IdleLock { id, file, returned });
        }
    }
}

/// An idle descriptor of the lock file `id` names, taken out of [`IDLE_LOCKS`].
fn idle_lock(id: (u64, u64)) -> Option<std::fs::File> {
    #[cfg(not(test))]
    return IDLE_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take(id);
    #[cfg(test)]
    THREAD_IDLE_LOCKS
        .try_with(|idle| idle.borrow_mut().take(id))
        .ok()
        .flatten()
}

/// Put `file`, an unlocked descriptor of the lock file `id` names, back into [`IDLE_LOCKS`].
fn keep_idle_lock(id: (u64, u64), file: std::fs::File) {
    #[cfg(not(test))]
    IDLE_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keep(id, file);
    // A lock dropped while its thread's store is torn down closes its descriptor with the closure.
    #[cfg(test)]
    let _ = THREAD_IDLE_LOCKS.try_with(|idle| idle.borrow_mut().keep(id, file));
}

/// A missing observation place and a writer holding it are different read outcomes.
enum StateObservation {
    Absent,
    Busy,
    Held(StateLock),
}

#[cfg(test)]
std::thread_local! {
    static STATE_OBSERVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(debug_assertions)]
std::thread_local! {
    /// The state lock files this thread observes across reads that must see one state
    /// ([`Observing`]).
    static OBSERVING: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A shared observation of this Mac's state a thread holds across reads that must see one state:
/// a local author's, across its snapshot and draft (`log_entries::author_and_commit_records`, its
/// bounded variant and a publish's draft), so no vault pass takes the state mid-draft and the
/// draft's own reads of it (its signer, its roster) see the state its snapshot was read from
/// rather than finding it busy; a pass's look, until it is done reading
/// (`log_entries::look_until`); and a vault look's, across the memos it asks ([`VaultLook`]),
/// which for `aterm-db`'s table-repair read batch is the batch's source read, held until the batch
/// returns ([`CommonsSourceRead::into_look`]). The holder drops it before any exclusive take of
/// the same lock. flock(2) grants this thread neither lock while it holds the other, so in a build
/// with debug assertions an exclusive take of the lock on this thread under it panics rather than
/// wait on it for good.
pub(crate) struct Observing {
    lock: StateLock,
    #[cfg(debug_assertions)]
    path: PathBuf,
}

impl Observing {
    fn new(lock: StateLock, paths: &Paths) -> Self {
        #[cfg(debug_assertions)]
        let path = paths.lock_file();
        #[cfg(debug_assertions)]
        OBSERVING.with(|held| held.borrow_mut().push(path.clone()));
        #[cfg(not(debug_assertions))]
        let _ = paths;
        Self {
            lock,
            #[cfg(debug_assertions)]
            path,
        }
    }

    /// The observation itself.
    const fn lock(&self) -> &StateLock {
        &self.lock
    }
}

#[cfg(debug_assertions)]
impl Drop for Observing {
    fn drop(&mut self) {
        OBSERVING.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(at) = held.iter().rposition(|path| *path == self.path) {
                held.remove(at);
            }
        });
    }
}

impl StateLock {
    /// A writer's take of `file`, which this process has locked exclusively.
    fn exclusive(file: std::fs::File) -> Self {
        Self {
            file: Some(file),
            shared: false,
            idle_as: None,
        }
    }

    /// Whether this is a shared observation of the vault's state lock that still guards the lock
    /// file at `paths`' path ([`Self::guards`]): while it does, no writer holds that lock, and none
    /// can take it until this drops. A writer's own take, or a lock that guards another file (one
    /// renamed away, another vault's), is not.
    pub(crate) fn observes(&self, paths: &Paths) -> bool {
        self.observes_in(paths, None)
    }

    /// [`Self::observes`], the `lstat` of the lock's path `glance`'s where given: the look of the
    /// check this judges for ([`fsx::Glance`]), whose first ask of the path stands for the rest of
    /// its asks. An observation that guarded the lock at a look's first ask is taken to guard it
    /// while that look lives: no writer takes the lock while this observes it, and a lock file
    /// renamed or replaced meanwhile by something that does not hold the lock at the vault's path
    /// is seen at the next look, as any file is that changes without the state lock
    /// ([`VaultLook`]).
    pub(crate) fn observes_in(&self, paths: &Paths, glance: Option<&fsx::Glance>) -> bool {
        self.shared && self.guards_in(&paths.lock_file(), glance)
    }

    /// Panics where an exclusive take of the lock at `path` would wait on this thread's own
    /// observation of it ([`Observing`]) for good.
    #[cfg(debug_assertions)]
    fn not_under_own_observation(path: &Path) {
        OBSERVING.with(|held| {
            assert!(
                !held.borrow().iter().any(|observing| observing == path),
                "an exclusive take of {} under this thread's own observation of it waits for \
                 good: drop the observation first",
                path.display()
            );
        });
    }

    fn open_lock_file(paths: &Paths, path: &Path) -> crate::Result<std::fs::File> {
        fsx::ensure_private_dir(&paths.dir)?;
        fsx::open_lock(path)
    }

    fn take(paths: &Paths) -> crate::Result<StateLock> {
        Self::take_at(paths, &paths.lock_file())?.recovered(paths)
    }

    /// Finish only a storage transition a previous writer already prepared. This lock keeps
    /// every writer out, so with nothing to finish one read serves the whole recovery, as it
    /// does every take: only a step that wrote makes the next read afresh.
    fn recovered(self, paths: &Paths) -> crate::Result<Self> {
        crate::records_storage::recover_pending(&paths.records_file())?;
        let mut reads = Reads::new(paths);
        if retained_history::recover_enrollment(&reads)? {
            reads = Reads::new(paths);
        }
        let config = reads.config()?;
        let (vault, store) = records_store_selected(&reads, config)?;
        let store = records_cache::read(paths, vault, store)?;
        if let Some(config) = config {
            retained_history::acknowledge_under(&reads, &store, config)?;
        }
        Ok(self)
    }

    /// [`StateLock::take`] of the lock file at `path` in `paths.dir` ([`publish_lock`]).
    fn take_at(paths: &Paths, path: &Path) -> crate::Result<StateLock> {
        #[cfg(debug_assertions)]
        Self::not_under_own_observation(path);
        loop {
            let file: std::fs::File = Self::open_lock_file(paths, path)?;
            #[expect(
                clippy::disallowed_methods,
                reason = "the take of StateLock, whose Drop releases by LOCK_UN (File::unlock)"
            )]
            file.lock()
                .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
            let lock = StateLock::exclusive(file);
            if lock.guards(path) {
                return Ok(lock);
            }
        }
    }

    /// The lock file this holds is still the one at `path`: a lab identity's set-aside renames
    /// the vault folder while it holds the lock (`start_over_as`), so a take that was waiting
    /// on that file wakes holding a lock on the kept folder, which guards nothing at `path`. A
    /// take that finds that drops it and takes the file at `path` again.
    fn guards(&self, path: &Path) -> bool {
        self.guards_in(path, None)
    }

    /// [`Self::guards`], the `lstat` of `path` `glance`'s where given and otherwise its own. The
    /// file the descriptor holds is an observation's `idle_as`, which the `fstat` its observation
    /// judged found, and otherwise its own `fstat`: an open descriptor's device and inode never
    /// change.
    fn guards_in(&self, path: &Path, glance: Option<&fsx::Glance>) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        let Some(file) = &self.file else {
            return false;
        };
        let held = self
            .idle_as
            .or_else(|| file.metadata().ok().map(|held| (held.dev(), held.ino())));
        let at = match glance {
            Some(glance) => glance
                .symlink_metadata(path)
                .ok()
                .flatten()
                .map(|at| (at.dev(), at.ino())),
            None => {
                #[cfg(test)]
                count_lock_use(LockUse::Judged, path);
                fsx::file_id(path)
            }
        };
        held.is_some_and(|held| at == Some(held))
    }

    fn take_at_until(paths: &Paths, path: &Path, deadline: Instant) -> crate::Result<StateLock> {
        #[cfg(debug_assertions)]
        Self::not_under_own_observation(path);
        publication_current(Some(deadline))?;
        let lock = loop {
            let file: std::fs::File = Self::open_lock_file(paths, path)?;
            aterm_uds::file_lock::lock_exclusive_until(
                &file,
                deadline,
                "vault publication lock is busy",
            )?;
            let lock = StateLock::exclusive(file);
            if lock.guards(path) {
                break lock;
            }
        };
        publication_current(Some(deadline))?;
        if path == paths.lock_file() {
            lock.recovered(paths)
        } else {
            Ok(lock)
        }
    }

    fn try_take(paths: &Paths) -> crate::Result<Option<StateLock>> {
        let path = paths.lock_file();
        #[cfg(debug_assertions)]
        Self::not_under_own_observation(&path);
        loop {
            let file: std::fs::File = Self::open_lock_file(paths, &path)?;
            #[expect(
                clippy::disallowed_methods,
                reason = "the non-blocking take of StateLock, whose Drop releases by LOCK_UN (File::unlock)"
            )]
            match file.try_lock() {
                Ok(()) => {
                    let lock = StateLock::exclusive(file);
                    if lock.guards(&path) {
                        return lock.recovered(paths).map(Some);
                    }
                }
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(Error::new(format!("{}: {e}", path.display())));
                }
            }
        }
    }

    /// Try an existing private lock without creating files or repairing their permissions.
    fn try_take_existing(paths: &Paths) -> crate::Result<Option<StateLock>> {
        use std::os::unix::fs::MetadataExt as _;
        #[cfg(debug_assertions)]
        Self::not_under_own_observation(&paths.lock_file());
        let Some((file, metadata)) = Self::open_existing(paths, true)? else {
            return Ok(None);
        };
        let dir =
            std::fs::symlink_metadata(&paths.dir).map_err(|error| Error::new(error.to_string()))?;
        if dir.uid() != fsx::euid() || dir.mode() & 0o7777 != 0o700 || metadata.mode() & 0o7077 != 0
        {
            return Ok(None);
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "nonblocking exclusive take of the existing state lock, released by StateLock Drop"
        )]
        match file.try_lock() {
            Ok(()) => {
                let lock = Self::exclusive(file);
                // A lock file renamed away since it was opened guards nothing here: busy.
                if lock.guards(&paths.lock_file()) {
                    lock.recovered(paths).map(Some)
                } else {
                    Ok(None)
                }
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(Error::new(error.to_string())),
        }
    }

    fn try_take_until(paths: &Paths, deadline: Instant) -> crate::Result<Option<StateLock>> {
        let mut backoff = Duration::from_millis(1);
        loop {
            if let Some(lock) = Self::try_take(paths)? {
                return Ok(Some(lock));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            std::thread::sleep(backoff.min(remaining));
            backoff = (backoff * 2).min(Duration::from_millis(50));
        }
    }

    /// Whether the vault folder is there to observe: `false` where it is missing; an error where
    /// it is a symlink or no directory.
    fn folder_present(paths: &Paths) -> crate::Result<bool> {
        match fsx::kind(&paths.dir)? {
            fsx::Kind::Missing => Ok(false),
            fsx::Kind::Dir => Ok(true),
            fsx::Kind::Symlink => Err(Error::new(format!(
                "{}: is a symlink; refusing to keep secrets through it",
                paths.dir.display()
            ))),
            fsx::Kind::File | fsx::Kind::Other => Err(Error::new(format!(
                "{}: not a directory",
                paths.dir.display()
            ))),
        }
    }

    /// [`Self::open_existing`] for an observation, which takes an idle descriptor of the file at
    /// the lock's path ([`IDLE_LOCKS`], by the device and inode an `lstat` of the path names)
    /// once the folder is judged as an open judges it, and judges one `fstat` of the descriptor
    /// afresh ([`fsx::judge_lock`]), as a file just opened is: a folder made a symlink and a lock
    /// given another owner, a second link or (in [`Self::classify`]) a mode others can use are
    /// refused as after an open, and a lock file renamed over the lock, or a folder moved, names
    /// another file, which is opened. The kernel's access check of an open is not made again: a
    /// lock its owner made unreadable is still observed through a descriptor opened before, as it
    /// was while one observation held it, since only its `flock` is used, never its bytes.
    fn open_observed(paths: &Paths) -> crate::Result<Option<(std::fs::File, std::fs::Metadata)>> {
        if !Self::folder_present(paths)? {
            return Ok(None);
        }
        let path = paths.lock_file();
        if let Some(id) = fsx::file_id(&path)
            && let Some(file) = idle_lock(id)
            && let Ok(metadata) = file.metadata()
        {
            return Ok(Some((file, fsx::judge_lock(metadata, &path)?)));
        }
        Self::open_present(paths, false)
    }

    fn open_existing(
        paths: &Paths,
        writable: bool,
    ) -> crate::Result<Option<(std::fs::File, std::fs::Metadata)>> {
        if !Self::folder_present(paths)? {
            return Ok(None);
        }
        Self::open_present(paths, writable)
    }

    /// [`Self::open_existing`] in a vault folder [`Self::folder_present`] found.
    fn open_present(
        paths: &Paths,
        writable: bool,
    ) -> crate::Result<Option<(std::fs::File, std::fs::Metadata)>> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = paths.lock_file();
        #[cfg(test)]
        count_lock_use(LockUse::Opened, &path);
        fsx::look_afresh();
        let file = match aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(writable)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path)
        }) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::new(format!("{}: {error}", path.display()))),
        };
        let metadata = fsx::lock_metadata(&file, &path)?;
        Ok(Some((file, metadata)))
    }

    /// Narrow this user's state lock to `0600` where an older build left it wider (group or other
    /// bits, setuid or setgid), so the observation after this takes it as a writer's take would
    /// have left it. It is narrowed through the descriptor this opened (`fchmod`), and only where
    /// a writer's take would narrow it ([`fsx::open_lock`]): a regular file reached without a
    /// symlink, owned by this user, with one hard link, which its owner can already read and
    /// write, in this user's own directory, which no other user may write (so no one else can
    /// have renamed one of this user's files to the lock's name). Any other lock is left exactly
    /// as it is, for the observation to refuse as before. `true` when it narrowed one.
    ///
    /// Called before an observation of the lock by the deny list's and roster's reads
    /// ([`swarm_denied_nodes`], [`swarm_roster_until`] and the memos), this Mac's own denial
    /// check, and the membership log's observations for a roster, a signer, a draft and a pass's
    /// look, among others: a listener that cannot read its deny list refuses every peer until it
    /// can, so a lock an upgrade found wide must not wait for the next vault sync. An observation
    /// that does not call this repairs nothing.
    ///
    /// # Errors
    /// The lock is this user's to narrow and the `fchmod` fails.
    fn narrow_owned(paths: &Paths) -> crate::Result<bool> {
        Self::narrow_owned_then(paths, || {})
    }

    /// [`Self::narrow_owned`], running `before_narrowing` between its checks and its `fchmod`: a
    /// test's stand-in for whatever renames the lock in that moment.
    fn narrow_owned_then(paths: &Paths, before_narrowing: impl FnOnce()) -> crate::Result<bool> {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(Some((file, metadata))) = Self::open_existing(paths, false) else {
            return Ok(false);
        };
        let mode = metadata.mode();
        if mode & 0o077 == 0 && mode & 0o6000 == 0 {
            return Ok(false);
        }
        // A writer opens it read-write, which its owner's bits must already allow.
        if mode & 0o600 != 0o600 {
            return Ok(false);
        }
        let Ok(dir) = std::fs::symlink_metadata(&paths.dir) else {
            return Ok(false);
        };
        if !dir.is_dir() || dir.uid() != fsx::euid() || dir.mode() & 0o022 != 0 {
            return Ok(false);
        }
        before_narrowing();
        fsx::narrow_lock(&file, &paths.lock_file())?;
        Ok(true)
    }

    fn permissions_need_repair(paths: &Paths) -> crate::Result<bool> {
        use std::os::unix::fs::MetadataExt as _;
        let Some((_file, metadata)) = Self::open_existing(paths, true)? else {
            return Ok(false);
        };
        let dir = std::fs::symlink_metadata(&paths.dir)
            .map_err(|error| Error::new(format!("{}: {error}", paths.dir.display())))?;
        if !dir.is_dir() || dir.uid() != fsx::euid() {
            return Err(Error::new(
                "the vault state directory is not this user's directory",
            ));
        }
        let mode = metadata.mode();
        // open_lock needs owner read/write before it can narrow the mode. Opening without
        // create or truncate above also refuses a read-only filesystem or denied write access.
        Ok(mode & 0o600 == 0o600 && (mode & 0o077 != 0 || mode & 0o6000 != 0))
    }

    fn observe(paths: &Paths) -> crate::Result<Option<StateLock>> {
        match Self::observe_classified(paths)? {
            StateObservation::Held(lock) => Ok(Some(lock)),
            StateObservation::Absent | StateObservation::Busy => Ok(None),
        }
    }

    /// Classify one nonblocking read without another filesystem probe or permission repair.
    fn observe_classified(paths: &Paths) -> crate::Result<StateObservation> {
        #[cfg(test)]
        STATE_OBSERVATIONS.set(STATE_OBSERVATIONS.get().saturating_add(1));
        Self::classify(paths, Self::open_observed(paths)?)
    }

    /// [`Self::narrow_owned`] and then [`Self::observe_classified`], on one open of the lock
    /// unless it needs narrowing.
    fn observe_narrowed(paths: &Paths) -> crate::Result<StateObservation> {
        use std::os::unix::fs::MetadataExt as _;
        let opened = Self::open_observed(paths)?;
        if opened
            .as_ref()
            .is_some_and(|(_, metadata)| metadata.mode() & 0o6077 != 0)
        {
            drop(opened);
            Self::narrow_owned(paths)?;
            return Self::observe_classified(paths);
        }
        Self::classify(paths, opened)
    }

    fn classify(
        paths: &Paths,
        opened: Option<(std::fs::File, std::fs::Metadata)>,
    ) -> crate::Result<StateObservation> {
        use std::os::unix::fs::MetadataExt as _;
        let Some((file, metadata)) = opened else {
            return Ok(StateObservation::Absent);
        };
        let file: std::fs::File = file;
        let path = paths.lock_file();
        #[cfg(test)]
        count_lock_use(LockUse::Observed, &path);
        if metadata.mode() & 0o077 != 0 || metadata.mode() & 0o6000 != 0 {
            return Err(Error::new(format!(
                "{}: mode {:o} lets other users read it; run chmod 600 on it",
                path.display(),
                metadata.mode() & 0o7777
            )));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the shared take of StateLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        match file.try_lock_shared() {
            Ok(()) => {
                let lock = StateLock {
                    file: Some(file),
                    shared: true,
                    idle_as: Some((metadata.dev(), metadata.ino())),
                };
                // A lock file renamed away since it was opened guards nothing here: busy, and a
                // wait opens the one at the path again.
                Ok(if lock.guards(&path) {
                    StateObservation::Held(lock)
                } else {
                    StateObservation::Busy
                })
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(StateObservation::Busy),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(Error::new(format!("{}: {error}", path.display())))
            }
        }
    }

    fn observe_until(paths: &Paths, deadline: Instant) -> crate::Result<Option<StateLock>> {
        match Self::observe_classified_until(paths, deadline)? {
            StateObservation::Held(lock) => Ok(Some(lock)),
            StateObservation::Absent | StateObservation::Busy => Ok(None),
        }
    }

    /// [`Self::observe_classified`], waiting until `deadline` while a writer holds the lock:
    /// `Busy` only where one still held it then.
    fn observe_classified_until(
        paths: &Paths,
        deadline: Instant,
    ) -> crate::Result<StateObservation> {
        let mut backoff = Duration::from_millis(1);
        loop {
            let observation = Self::observe_classified(paths)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !matches!(observation, StateObservation::Busy) || remaining.is_zero() {
                return Ok(observation);
            }
            #[cfg(any(test, debug_assertions))]
            OBSERVE_BLOCKED.with(|hook| {
                if let Some(hook) = hook.borrow_mut().as_mut() {
                    hook();
                }
            });
            std::thread::sleep(backoff.min(remaining));
            backoff = (backoff * 2).min(Duration::from_millis(50));
        }
    }

    /// The observation of a reader that judges what the vault's state holds (whether a snapshot
    /// is still current, what the applied copy carries): a writer holding the lock is waited
    /// for, up to [`CURRENT_WAIT`], since a busy lock says nothing either way. Writers hold it
    /// for milliseconds as a rule (the subscriber takes it for its pass over every long-poll
    /// answer, an unchanged one included). `Busy` only where one still held it at the end, which
    /// the reader words as that ([`writer_outlasted_wait`]), never as a change.
    fn observe_judging(paths: &Paths) -> crate::Result<StateObservation> {
        Self::observe_classified_until(paths, Instant::now() + current_wait())
    }
}

/// What this test process did with a state lock file, counted for each path (each tempdir path is
/// one test's).
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LockUse {
    /// Opened it to observe it ([`StateLock::open_existing`]).
    Opened,
    /// Observed it, by an open or an idle descriptor ([`StateLock::classify`]).
    Observed,
    /// Judged on its own call whether a descriptor still holds the file at its path: an `lstat`
    /// of the path that no look took ([`StateLock::guards`]).
    Judged,
}

#[cfg(test)]
static LOCK_USES: std::sync::Mutex<BTreeMap<(LockUse, PathBuf), usize>> =
    std::sync::Mutex::new(BTreeMap::new());

#[cfg(test)]
fn count_lock_use(what: LockUse, path: &Path) {
    *LOCK_USES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry((what, path.to_path_buf()))
        .or_default() += 1;
}

#[cfg(test)]
fn lock_uses_of(what: LockUse, paths: &Paths) -> usize {
    LOCK_USES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(what, paths.lock_file()))
        .copied()
        .unwrap_or(0)
}

/// How many times this test process opened the state lock file of `paths` to observe it.
#[cfg(test)]
pub(crate) fn lock_opens_of(paths: &Paths) -> usize {
    lock_uses_of(LockUse::Opened, paths)
}

/// How many times this test process observed the state lock file of `paths`.
#[cfg(test)]
pub(crate) fn lock_observations_of(paths: &Paths) -> usize {
    lock_uses_of(LockUse::Observed, paths)
}

/// How many times this test process judged on its own call whether a descriptor still holds the
/// state lock file of `paths` ([`StateLock::guards`]), a look's judgement through its glance
/// apart.
#[cfg(test)]
pub(crate) fn lock_judgements_of(paths: &Paths) -> usize {
    lock_uses_of(LockUse::Judged, paths)
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// A test's action on each blocked try of [`StateLock::observe_classified_until`] (and so of
    /// [`StateLock::observe_until`] and [`StateLock::observe_judging`]) that will try again,
    /// run in the waiting thread, so a hold can end exactly while a reader waits on it.
    pub(crate) static OBSERVE_BLOCKED: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `read` with `blocked` as this thread's action on each try of a waiting reader that finds
/// a writer holding the state lock and will try again ([`OBSERVE_BLOCKED`]), so a hold (a
/// fixture's [`test_hold_state_lock`]) ends exactly while the reader waits on it; and count
/// those tries. This observes contention; it does not change whether the reader waits.
#[cfg(any(test, debug_assertions))]
pub fn test_while_state_lock_held<T>(
    mut blocked: impl FnMut() + 'static,
    read: impl FnOnce() -> T,
) -> (T, usize) {
    struct Restore(Option<Box<dyn FnMut()>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OBSERVE_BLOCKED.with(|hook| *hook.borrow_mut() = self.0.take());
        }
    }
    let tries = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = std::rc::Rc::clone(&tries);
    let restore = Restore(OBSERVE_BLOCKED.with(|hook| {
        hook.replace(Some(Box::new(move || {
            counted.set(counted.get() + 1);
            blocked();
        })))
    }));
    let read = read();
    drop(restore);
    (read, tries.get())
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        // Only a descriptor that holds no lock goes back; one whose unlock failed closes, which
        // releases it.
        if file.unlock().is_ok()
            && let Some(id) = self.idle_as.take()
        {
            keep_idle_lock(id, file);
        }
    }
}

/// The lock a publish from this Mac holds while it PUTs its sealed envelope to the servers
/// ([`ledger_publish::publish_to_servers_for_admin`]), so every server hears two publishes from
/// this Mac in the order they began. Nothing else takes it: a server that takes a PUT and never
/// answers holds the next publish from this Mac for one exchange deadline, and no other secrets
/// command, which the state lock ([`StateLock::take`]) would hold.
fn publish_lock(paths: &Paths) -> crate::Result<StateLock> {
    StateLock::take_at(paths, &paths.publish_lock_file())
}

/// Default machine name from the computer, LAN or host name (an `m<N>` tag first), or `"machine"`.
pub fn default_host_name() -> String {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        for key in ["ComputerName", "LocalHostName"] {
            let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/scutil");
            cmd.arg("--get").arg(key);
            if let Ok(out) =
                aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(5))
                && out.status.success()
            {
                candidates.push(String::from_utf8_lossy(&out.stdout).trim().to_string());
            }
        }
    }
    if let Some(name) = aterm_uds::process::lan_host_name() {
        candidates.push(name);
    }
    if let Some(name) = read_etc_hostname() {
        candidates.push(name);
    }
    host_name_hint(&candidates)
}

/// Prefer ComputerName, then LocalHostName. A log name is a display hint, not an owner label.
fn host_name_hint(candidates: &[String]) -> String {
    for candidate in candidates {
        let first = candidate
            .split(|c: char| c.is_whitespace() || c == '.' || c == '-' || c == '_')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if text::valid_machine_name(&first) {
            return first;
        }
    }
    "machine".to_string()
}

fn read_etc_hostname() -> Option<String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/etc/hostname")
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() || meta.len() > 1024 {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(1025).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= 1024)
        .then(|| {
            std::str::from_utf8(&bytes)
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .flatten()
}

/// 80-bit SHA-256 prefix of `recipient` in lowercase RFC 4648 base32 (`xxxx-xxxx-xxxx-xxxx`).
pub fn join_code(recipient: &Recipient) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let digest = sha256(&recipient.0);
    let mut bits: u128 = 0;
    for &byte in &digest[..10] {
        bits = (bits << 8) | u128::from(byte);
    }
    let mut out = String::with_capacity(19);
    for i in 0..16 {
        let index = ((bits >> (75 - 5 * i)) & 31) as usize;
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        out.push(char::from(ALPHABET[index]));
    }
    out
}

pub(crate) fn read_identity(paths: &Paths) -> crate::Result<Option<Identity>> {
    Ok(read_identity_marked(paths)?.map(|(identity, _)| identity))
}

/// The comment line that marks a lab identity in its file ([`crate::LabMark`]), followed by the
/// mark's word. A comment, so every build reads the key past it as before (the key is the last
/// line that is not one).
const LAB_LINE: &str = "# aterm lab identity: ";

/// The most bytes an identity file holds that a read of it takes.
const IDENTITY_MAX: usize = 16 << 10;

/// This machine's age identity and its lab mark, read together from one read of
/// `~/.config/aterm/secrets/identity`: the mark is a line starting [`LAB_LINE`], written when the
/// identity was made ([`install_identity`]).
pub(crate) fn read_identity_marked(
    paths: &Paths,
) -> crate::Result<Option<(Identity, Option<crate::LabMark>)>> {
    let Some(bytes) = fsx::read_private(&paths.identity_file(), IDENTITY_MAX)? else {
        return Ok(None);
    };
    identity_marked_in(paths, &bytes).map(Some)
}

/// The recipient of this machine's age identity ([`read_identity`]), `None` where there is none,
/// kept for the digest of the bytes it was parsed from. The identity file's digest is kept while
/// the file stands ([`fsx::private_digest`]), so a check that compares this Mac's recipient opens
/// the secret only when the file changed; the file's first `lstat` is `glance`'s where the
/// caller's check looks through one. Only digests and recipients are kept, never the key.
pub(crate) fn identity_recipient(
    paths: &Paths,
    glance: Option<&fsx::Glance>,
) -> crate::Result<Option<Recipient>> {
    type Kept = std::sync::Mutex<std::collections::VecDeque<([u8; 32], Recipient)>>;
    #[cfg(not(test))]
    static RECIPIENTS: Kept = std::sync::Mutex::new(std::collections::VecDeque::new());
    #[cfg(test)]
    std::thread_local! {
        /// A test thread's own recipients, so one test's asks never let go of another's.
        static THREAD_RECIPIENTS: Kept =
            const { std::sync::Mutex::new(std::collections::VecDeque::new()) };
    }
    /// The identities whose recipients are kept, the oldest let go first.
    const KEPT: usize = 8;
    let recipient_in = |kept: &Kept| -> crate::Result<Option<Recipient>> {
        let path = paths.identity_file();
        let Some(digest) = fsx::private_digest(&path, IDENTITY_MAX, glance)? else {
            return Ok(None);
        };
        if let Ok(kept) = kept.lock()
            && let Some((_, recipient)) = kept.iter().find(|(of, _)| *of == digest)
        {
            return Ok(Some(*recipient));
        }
        let Some(bytes) = fsx::read_private(&path, IDENTITY_MAX)? else {
            return Ok(None);
        };
        let recipient = identity_marked_in(paths, &bytes)?.0.recipient();
        let parsed = aterm_digest::Sha256::digest(bytes.as_slice());
        if let Ok(mut kept) = kept.lock() {
            kept.retain(|(of, _)| *of != parsed);
            if kept.len() >= KEPT {
                kept.pop_front();
            }
            kept.push_back((parsed, recipient));
        }
        Ok(Some(recipient))
    };
    #[cfg(not(test))]
    {
        recipient_in(&RECIPIENTS)
    }
    #[cfg(test)]
    THREAD_RECIPIENTS.with(recipient_in)
}

/// The identity and lab mark `bytes`, the identity file's, hold ([`read_identity_marked`]).
fn identity_marked_in(
    paths: &Paths,
    bytes: &SecretBytes,
) -> crate::Result<(Identity, Option<crate::LabMark>)> {
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new("identity: not UTF-8"))?;
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or_else(|| Error::new("identity: no key in the file"))?;
    let mark = text.lines().map(str::trim).find_map(|line| {
        line.strip_prefix(LAB_LINE)
            .map(|rest| crate::LabMark::of_word(rest.split_whitespace().next().unwrap_or("")))
    });
    let identity = Identity::parse(line).map_err(|_| {
        Error::new(format!(
            "{}: not an age identity",
            paths.identity_file().display()
        ))
    })?;
    Ok((identity, mark))
}

/// Read this machine's age identity from `~/.config/aterm/secrets/identity` without creating one.
///
/// # Errors
/// Returns an [`Error`] if the identity file cannot be read privately or is malformed.
pub fn read_machine_identity(paths: &Paths) -> crate::Result<Option<Identity>> {
    read_identity(paths)
}

/// This machine's key, made (0600, age-keygen format) if there is none: a lab identity where
/// this process makes lab identities ([`crate::lab_identity`] of [`crate::Making::now`]).
pub fn machine_identity(paths: &Paths) -> crate::Result<Identity> {
    machine_identity_making(paths, crate::Making::now())
}

/// [`machine_identity`] as a process that is `making` would make it: the seam a test makes a
/// release build's unmarked identity with, or a virtual machine's, whatever binary it runs in.
#[cfg(any(test, debug_assertions))]
pub fn machine_identity_as(paths: &Paths, making: crate::Making) -> crate::Result<Identity> {
    machine_identity_making(paths, making)
}

fn machine_identity_making(paths: &Paths, making: crate::Making) -> crate::Result<Identity> {
    if let Some(identity) = read_identity(paths)? {
        return Ok(identity);
    }
    fsx::ensure_private_dir(&paths.dir)?;
    install_identity(paths, Identity::generate()?, crate::lab_identity(making))
}

/// Write `identity` as this machine's key with its lab mark, `mark` (A12): written with the key,
/// in one new file, so no identity is ever without the mark it was made with, and none gains one
/// later. Where another process wrote the file first, its key and mark win.
fn install_identity(
    paths: &Paths,
    identity: Identity,
    mark: Option<crate::LabMark>,
) -> crate::Result<Identity> {
    let secret = identity.to_bech32();
    let mut file = format!(
        "# aterm secrets machine key: created {} (unix), never copied off this machine.\n# public key: {}\n",
        now(),
        identity.recipient().to_bech32()
    )
    .into_bytes();
    if let Some(mark) = mark {
        file.extend_from_slice(
            format!(
                "{LAB_LINE}{} (it founds its own lab and never joins the live ALab)\n",
                mark.word()
            )
            .as_bytes(),
        );
    }
    file.extend_from_slice(secret.as_slice());
    file.push(b'\n');
    let file = SecretBytes::new(file);
    if fsx::write_new(&paths.identity_file(), file.as_slice(), 0o600)? {
        return Ok(identity);
    }
    read_identity(paths)?
        .ok_or_else(|| Error::new("the machine key changed while it was being created; try again"))
}

/// The record a lab identity's set-aside leaves in the new vault folder ([`start_over_as`]): one
/// line, `<unix seconds>\t<mark word>\t<the kept folder's name>`, never a key or a value. A fact
/// for later, read on demand ([`lab_set_aside`]); never an alert.
const LAB_SET_ASIDE_FILE: &str = "lab-set-aside";

/// How long a set-aside waits for the vault folder's state lock that another process holds. A
/// lock still held then leaves the folder as it is: the next process to start tries again.
const LAB_SET_ASIDE_LOCK_WAIT: Duration = Duration::from_secs(5);

/// What one lab identity's set-aside did (A16): the mark the identity carried and the folder its
/// vault state is kept in, beside the vault folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabSetAside {
    /// The set-aside identity's mark.
    pub mark: crate::LabMark,
    /// The folder the lab identity's vault state is kept in (`<folder>.lab-<date>`).
    pub kept: PathBuf,
}

impl LabSetAside {
    /// The line `aterm secrets status` shows for it: what was set aside and where it is kept.
    #[must_use]
    pub fn status_line(&self) -> String {
        format!(
            "set aside: a lab identity ({}), kept as {}; this Mac started over as a new one",
            self.mark.word(),
            self.kept.display()
        )
    }
}

/// [`Paths::for_home`]'s once-per-process [`start_over_as`] of this process ([`crate::Making::now`])
/// on this Mac's vault folder. What it did, or why it could not, is a line in this process's
/// log; a failure leaves the folder as it was, so this Mac stays the lab identity every desk on
/// the live ALab refuses until a later process sets it aside.
fn start_over_live(paths: &Paths) {
    // Most processes find an unmarked identity or none; only a marked one reads what this
    // process is (its virtual-machine probe included).
    if !matches!(read_identity_marked(paths), Ok(Some((_, Some(_))))) {
        return;
    }
    match start_over_as(paths, crate::Making::now(), now()) {
        Ok(Some(set)) => crate::log_line(&lab_set_aside_line(set.mark, &set.kept)),
        Ok(None) => {}
        Err(error) => crate::log_line(&format!(
            "aterm secrets: this Mac's vault identity is a lab identity, and setting its vault \
             folder aside did not finish ({error}); a later start tries again"
        )),
    }
}

fn lab_set_aside_line(mark: crate::LabMark, kept: &Path) -> String {
    format!(
        "aterm secrets: this Mac's vault identity was a lab identity ({}); its vault folder is \
         kept as {} and this Mac starts as a new one",
        mark.word(),
        kept.display()
    )
}

/// A lab identity under a release app starts over (A16): where `making`, what this process is,
/// would make an unmarked identity ([`crate::lab_identity`] says `None`) and `paths`' vault folder
/// holds a lab-marked one, rename that folder to `<folder>.lab-<date>` (`-2`, `-3`, … where that
/// name is taken; `now_secs` dates it, UTC), and run the new-Mac path: a new, unmarked identity in
/// a new folder, which the LAN rule admits as any new Mac. Nothing is deleted and nothing asks
/// (first-run setup asks none); the new folder's [`LAB_SET_ASIDE_FILE`] names what was set aside.
///
/// `None`, changing nothing: a process that would itself make a lab identity (a test binary, a
/// debug build, a lab lane's, one in a virtual machine), whose identity a lab identity is meant to
/// be; an unmarked identity; no identity.
///
/// # Errors
/// The identity file cannot be read, another process holds the folder's state lock past
/// [`LAB_SET_ASIDE_LOCK_WAIT`], or the rename or the new identity's write fails. A failure before
/// the rename changes nothing; one after it names the kept folder and leaves a vault folder the
/// next [`machine_identity`] completes as a new Mac's (or, where a debug build's process made a
/// lab identity there first, one the next start sets aside again).
fn start_over_as(
    paths: &Paths,
    making: crate::Making,
    now_secs: u64,
) -> crate::Result<Option<LabSetAside>> {
    start_over_then(paths, making, now_secs, || {}, || {})
}

/// [`start_over_as`], running `after_take` once its first take of the state lock returns, and
/// `before_identity` after the new folder's record is written and before its identity is made: a
/// test's stand-ins for another set-aside that moved the folder while this one waited on its lock,
/// and for another process that makes an identity in the new folder in that moment.
fn start_over_then(
    paths: &Paths,
    making: crate::Making,
    now_secs: u64,
    after_take: impl FnOnce(),
    before_identity: impl FnOnce(),
) -> crate::Result<Option<LabSetAside>> {
    if crate::lab_identity(making).is_some() {
        return Ok(None);
    }
    if !matches!(read_identity_marked(paths)?, Some((_, Some(_)))) {
        return Ok(None);
    }
    let (Some(parent), Some(name)) = (paths.dir.parent(), paths.dir.file_name()) else {
        return Err(Error::new(format!(
            "{}: the vault folder has no parent to keep it beside",
            paths.dir.display()
        )));
    };
    let lock_path = paths.lock_file();
    let deadline = Instant::now() + LAB_SET_ASIDE_LOCK_WAIT;
    let mut after_take = Some(after_take);
    // A set-aside that waited on a lock file another set-aside moved away with its folder wakes
    // holding the kept folder's lock, which guards nothing at the vault's path: it takes the lock
    // there instead, as `StateLock::take_at`, `take_at_until` and `try_take` do
    // ([`StateLock::guards`]), so it renames a folder only while it holds that folder's lock, and
    // none while another holds or observes it.
    let lock = loop {
        let file = fsx::open_lock(&lock_path)?;
        aterm_uds::file_lock::lock_exclusive_until(
            &file,
            deadline,
            "the vault folder's state lock is busy",
        )
        .map_err(|e| Error::new(format!("{}: {e}", lock_path.display())))?;
        let lock = StateLock::exclusive(file);
        if let Some(after_take) = after_take.take() {
            after_take();
        }
        if lock.guards(&lock_path) {
            break lock;
        }
        drop(lock);
        if !matches!(read_identity_marked(paths)?, Some((_, Some(_)))) {
            return Ok(None);
        }
    };
    // Under the lock, the folder at the vault's path is still the lab one: another process's
    // set-aside, which holds this lock while it renames, moved none, or this reads its new one.
    let Some((_, Some(mark))) = read_identity_marked(paths)? else {
        return Ok(None);
    };
    let kept = rename_aside(parent, &name.to_string_lossy(), &paths.dir, now_secs)?;
    // The lock file moved with the folder. A take waiting on it wakes to a file no longer at the
    // lock's path and takes the new folder's instead ([`StateLock::guards`]).
    drop(lock);
    let kept_name = kept
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let started = move || -> crate::Result<()> {
        aterm_durable::durable_dir(parent)?;
        fsx::ensure_private_dir(&paths.dir)?;
        fsx::write_atomic(
            &paths.dir.join(LAB_SET_ASIDE_FILE),
            format!(
                "{now_secs}\t{}\t{}\n",
                mark.word(),
                kept_name.replace(['\t', '\n'], " ")
            )
            .as_bytes(),
            0o600,
        )?;
        before_identity();
        machine_identity_making(paths, making)?;
        // A process that makes lab identities (a debug build's) may have made one in the new
        // folder first; its key wins the write ([`install_identity`]), so say so rather than
        // report a new Mac. The next start sets that one aside too.
        if let Some((_, Some(again))) = read_identity_marked(paths)? {
            return Err(Error::new(format!(
                "another process made a lab identity ({}) in the new vault folder first",
                again.word()
            )));
        }
        Ok(())
    };
    started().map_err(|error| {
        Error::new(format!(
            "the lab identity's vault folder is kept as {}, and the new one is not finished: \
             {error}",
            kept.display()
        ))
    })?;
    Ok(Some(LabSetAside { mark, kept }))
}

/// Rename `dir` (named `name` in `parent`) to the first free `<name>.lab-<YYYY-MM-DD>[-<n>]`,
/// never over an existing entry (`RenameFlags::NOREPLACE`).
fn rename_aside(parent: &Path, name: &str, dir: &Path, now_secs: u64) -> crate::Result<PathBuf> {
    let days = i64::try_from(now_secs / 86_400).unwrap_or(0);
    let (year, month, day) = aterm_types::rfc3339::civil_from_days(days);
    let stem = format!("{name}.lab-{year:04}-{month:02}-{day:02}");
    let parent_dir = std::fs::File::open(parent)
        .map_err(|e| Error::new(format!("{}: {e}", parent.display())))?;
    for n in 1..=1000_u32 {
        let target = if n == 1 {
            stem.clone()
        } else {
            format!("{stem}-{n}")
        };
        match aterm_dirfd::renameat_with(
            &parent_dir,
            name,
            &parent_dir,
            target.as_str(),
            aterm_dirfd::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
        {
            Ok(()) => return Ok(parent.join(target)),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(e) => {
                return Err(Error::new(format!(
                    "{} -> {}: {e}",
                    dir.display(),
                    parent.join(target).display()
                )));
            }
        }
    }
    Err(Error::new(format!(
        "{}: every name {stem}[-n] up to 1000 is taken",
        dir.display()
    )))
}

/// The set-aside this vault folder's Mac started over from ([`start_over_as`]), as its record
/// says: the mark and the kept folder (beside the vault folder). `None` where this folder was not
/// made by one.
///
/// # Errors
/// The record exists and cannot be read.
pub fn lab_set_aside(paths: &Paths) -> crate::Result<Option<LabSetAside>> {
    let Some(bytes) = fsx::read_private(&paths.dir.join(LAB_SET_ASIDE_FILE), 4 << 10)? else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(bytes.as_slice()).into_owned();
    let mut fields = text.lines().next().unwrap_or("").split('\t').skip(1);
    let (Some(mark), Some(kept)) = (fields.next(), fields.next()) else {
        return Ok(None);
    };
    let kept = paths
        .dir
        .parent()
        .map_or_else(|| PathBuf::from(kept), |parent| parent.join(kept));
    Ok(Some(LabSetAside {
        mark: crate::LabMark::of_word(mark),
        kept,
    }))
}

/// [`start_over_as`], for a test that states what the process is and the time.
#[cfg(any(test, debug_assertions))]
pub fn start_over_lab_identity_as(
    paths: &Paths,
    making: crate::Making,
    now_secs: u64,
) -> crate::Result<Option<LabSetAside>> {
    start_over_as(paths, making, now_secs)
}

impl crate::Making {
    /// What this process is, read now ([`crate::lab_identity`]): a test binary by its executable's
    /// name (`aterm_uds::test_binary::is_test_binary`), a build with debug assertions on, a
    /// process a lab lane marked ([`Self::lab_lane_of`]: `ATERM_LAB_MEMBER_SERVICE` or
    /// `ATERM_LAB_VAULT_SERVICE` set, whatever its value), and
    /// whether it runs inside a virtual machine (macOS's `kern.hv_vmm_present`,
    /// [`aterm_sysprobe::hv_vmm_present`]), as [`Self::reading`] reads them.
    #[must_use]
    pub fn now() -> Self {
        Self::reading(
            std::env::current_exe().is_ok_and(|exe| aterm_uds::test_binary::is_test_binary(&exe)),
            aterm_sysprobe::hv_vmm_present,
        )
    }

    /// Whether a lab lane marked a process whose environment has each variable `set` says it
    /// has: [`crate::discover::LAB_MEMBER_SERVICE_ENV`] or
    /// [`crate::discover::LAB_VAULT_SERVICE_ENV`], whatever its value (one this build cannot take
    /// browses for nothing, and still marks the process a lab's).
    pub(crate) fn lab_lane_of(set: impl Fn(&str) -> bool) -> bool {
        set(crate::discover::LAB_MEMBER_SERVICE_ENV) || set(crate::discover::LAB_VAULT_SERVICE_ENV)
    }

    /// [`Self::now`] for a process that is a test binary or not (`test_binary`), its
    /// virtual-machine fact read through `virtual_machine`. A test binary, in any crate, reads no
    /// probe: a test states the fact through [`crate::Making`], so no test's outcome hangs on the
    /// host it runs on, a macOS VM included, and every identity a test binary makes is marked
    /// [`crate::LabMark::TestBinary`] first anyway. A probe that does not answer (off macOS, a
    /// kernel with no such name) is no virtual machine, so a Mac's own hardware never makes a lab
    /// identity because a probe failed.
    pub(crate) fn reading(test_binary: bool, virtual_machine: fn() -> Option<bool>) -> Self {
        Self {
            test_binary,
            debug_build: cfg!(debug_assertions),
            lab_lane: Self::lab_lane_of(|knob| std::env::var_os(knob).is_some()),
            virtual_machine: !test_binary && virtual_machine() == Some(true),
        }
    }
}

/// The lab mark a joiner's lines carry (`members::wire::lab_field`) for an identity whose file
/// carries `file` (its own mark), in a process that is `making`: the file's mark, or, for an
/// identity an older build made unmarked, [`crate::LabMark::VirtualMachine`] in a virtual
/// machine, as [`lab_standing`] counts it, so a member on the live genesis refuses that guest's
/// ask, run and proof too, whatever members it names.
pub(crate) fn wire_mark(
    file: Option<crate::LabMark>,
    making: crate::Making,
) -> Option<crate::LabMark> {
    file.or_else(|| {
        making
            .virtual_machine
            .then_some(crate::LabMark::VirtualMachine)
    })
}

/// This Mac's lab mark (A12): its vault identity's, as the identity file says; where it holds no
/// identity yet, the mark of the one this process would make ([`crate::lab_identity`] of
/// [`crate::Making::now`]); and in a virtual machine, [`crate::LabMark::VirtualMachine`] for an
/// identity an older build made there unmarked. `None` for an identity a release build made on a
/// Mac's own hardware, whatever build reads it now.
///
/// # Errors
/// The identity file cannot be read or holds no key.
pub fn lab_mark(paths: &Paths) -> crate::Result<Option<crate::LabMark>> {
    Ok(lab_standing(paths, crate::Making::now())?.map(|(mark, _)| mark))
}

/// [`lab_mark`] for a process that is `making`, and what makes this Mac one, as its refusal says
/// it ([`refused_as_lab`]).
fn lab_standing(
    paths: &Paths,
    making: crate::Making,
) -> crate::Result<Option<(crate::LabMark, &'static str)>> {
    Ok(match read_identity_marked(paths)? {
        Some((_, Some(mark))) => Some((mark, "this Mac's vault identity is a lab identity")),
        // A VM guest's ask reaches a member from its host's address on the home LAN, so an
        // identity a build before this one made in a VM (a legacy-upgrade guest's) is a lab
        // identity there all the same. Only the VM counts here: an identity a release build made
        // stays unmarked under a debug build, so a developer's own Mac keeps its seat.
        Some((_, None)) => making.virtual_machine.then_some((
            crate::LabMark::VirtualMachine,
            "this Mac runs in a virtual machine, so its vault identity is a lab identity",
        )),
        None => crate::lab_identity(making).map(|mark| (mark, "this build makes lab identities")),
    })
}

/// What a lab identity is told, before anything is dialed, when a join would ask the live ALab.
pub const LAB_IDENTITY_REFUSAL: &str = "a lab identity founds its own lab; pass its URL";

/// Refuse, before anything is dialed, a join that would ask the live ALab where this Mac is a lab
/// identity ([`lab_mark`]): [`crate::ErrorKind::LabIdentity`]. The callers decide what asks the
/// live ALab: `aterm secrets join` with no URL or the default one ([`fn@join`]), a browse of
/// ALab's members ([`refuse_lab_member_browse`]), a proof over the fleet
/// ([`refuse_lab_identity_for_fleet`]).
///
/// # Errors
/// [`crate::ErrorKind::LabIdentity`], or the identity file cannot be read.
pub fn refuse_lab_identity(paths: &Paths) -> crate::Result<()> {
    refuse_lab_identity_as(paths, crate::Making::now())
}

/// [`refuse_lab_identity`] for a process that is `making`.
fn refuse_lab_identity_as(paths: &Paths, making: crate::Making) -> crate::Result<()> {
    refused_as_lab(paths, making, lab_standing(paths, making)?)
}

/// The refusal of [`lab_standing`]'s `standing` for a process that is `making`. Where this build
/// on this hardware would make no lab identity, the mark is a held identity's that a test, a debug
/// build or a lab lane made, and the refusal says how the Mac rejoins with the release app: once
/// its vault folder is set aside (kept, never deleted; a release app's start does that to this
/// user's own folder, [`start_over_as`]), this build makes it a new, unmarked identity, which
/// joins as a reinstalled Mac does, its node key proving it on the home LAN (`client/readmit.rs`).
fn refused_as_lab(
    paths: &Paths,
    making: crate::Making,
    standing: Option<(crate::LabMark, &str)>,
) -> crate::Result<()> {
    let Some((mark, whose)) = standing else {
        return Ok(());
    };
    let rejoin = if crate::lab_identity(making).is_none() {
        format!(
            "; this build makes no lab identity, so with this Mac's vault folder ({}) set aside \
             (a release app's start does that to its own) it makes a new one that can join ALab",
            paths.dir.display()
        )
    } else {
        String::new()
    };
    Err(Error::of_kind(
        crate::ErrorKind::LabIdentity,
        format!("{whose} ({}): {LAB_IDENTITY_REFUSAL}{rejoin}", mark.word()),
    ))
}

/// [`refuse_lab_identity`] before a browse for the members on this network with no member named
/// (`aterm members join`, a waiting Mac's ask): a browse of a lab's own members service
/// (`ATERM_LAB_MEMBER_SERVICE`, [`crate::discover::member_service`]) finds that lab's members
/// alone and goes; a browse of ALab's `_alab._tcp` finds the live ALab's.
///
/// # Errors
/// As [`refuse_lab_identity`].
pub fn refuse_lab_member_browse(paths: &Paths) -> crate::Result<()> {
    refuse_lab_member_browse_as(
        paths,
        std::env::var_os(crate::discover::LAB_MEMBER_SERVICE_ENV).as_deref(),
        crate::Making::now(),
    )
}

fn refuse_lab_member_browse_as(
    paths: &Paths,
    service: Option<&std::ffi::OsStr>,
    making: crate::Making,
) -> crate::Result<()> {
    if crate::discover::member_service(service)
        .is_ok_and(|service| service != crate::discover::MEMBER_SERVICE)
    {
        return Ok(());
    }
    refused_as_lab(paths, making, lab_standing(paths, making)?)
}

/// [`refuse_lab_identity`] before a proof over the fleet (`vault-join`), which names no vault:
/// where this Mac follows a vault that is not ALab's (a lab's, joined by its URL), the proof goes
/// to that lab's members; where it follows ALab's, or none yet, the fleet's members are the live
/// ALab's.
///
/// # Errors
/// As [`refuse_lab_identity`], or `source.toml` cannot be read.
pub fn refuse_lab_identity_for_fleet(paths: &Paths) -> crate::Result<()> {
    match config(paths)? {
        Some(config) if !alab_vault(&config) => Ok(()),
        _ => refuse_lab_identity(paths),
    }
}

#[cfg(test)]
thread_local! {
    /// What [`live_genesis_store`] answers on one test thread, where this crate's unit tests
    /// compile no ALab genesis: `Some(true)` makes a scratch vault's desk the live ALab's.
    static LIVE_GENESIS: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` on this thread with every vault read as the live ALab's (`on`) or not
/// ([`on_live_genesis`]): the seam a test puts a scratch desk on the compiled ALab genesis with.
#[cfg(test)]
pub(crate) fn with_live_genesis<T>(on: bool, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LIVE_GENESIS.with(|live| live.set(self.0));
        }
    }
    let _restore = Restore(LIVE_GENESIS.with(|live| live.replace(Some(on))));
    f()
}

/// Whether `store` pins the ALab genesis this build compiles ([`crate::membership_log::Store::for_vault`]):
/// the membership log it holds, or will, is the live ALab's.
pub(crate) fn live_genesis_store(store: &crate::membership_log::Store) -> bool {
    #[cfg(test)]
    if let Some(on) = LIVE_GENESIS.with(std::cell::Cell::get) {
        return on;
    }
    store.pinned_network().is_some_and(|pinned| {
        crate::membership_log::genesis::compiled().is_ok_and(|compiled| compiled == Some(pinned))
    })
}

/// Whether the vault at `paths` is the live ALab's by its genesis (A12): its record store pins
/// the ALab genesis this build compiles ([`live_genesis_store`]), so the log a member here signs
/// in is that genesis's. A desk here refuses a lab identity's ask, run and proof; a desk on any
/// other genesis, a lab's, admits one. A store that cannot be read is the live ALab's unless the
/// vault config names a lab's vault: a read that fails lets no lab identity in.
#[must_use]
pub fn on_live_genesis(paths: &Paths) -> bool {
    live_genesis_read(paths)
        .unwrap_or_else(|_| !matches!(config(paths), Ok(Some(config)) if !alab_vault(&config)))
}

/// [`on_live_genesis`] by the record store alone, or why it cannot be read, which
/// `on_live_genesis` answers by the vault config: for a reader that keeps what it read only where
/// nothing failed (`members::desk::Local::whole`).
///
/// # Errors
/// The record store cannot be read.
pub(crate) fn live_genesis_read(paths: &Paths) -> crate::Result<bool> {
    Ok(live_genesis_store(&records_store(paths)?.1))
}

/// The configured source, if any.
pub fn config(paths: &Paths) -> crate::Result<Option<Config>> {
    config_with(paths, |pin| Ok(cached_chain(paths, pin)))
}

/// [`config`] for a read that is kept ([`config_now`], the members desk's
/// [`Local::of`](crate::members::desk::Local::of)): where `source.toml` names no `followed`, a
/// held envelope that cannot be read is this read's error, never the empty chain [`config`]
/// takes it as, so a failed read is never kept.
///
/// # Errors
/// As [`config`], or the held envelope cannot be read.
pub(crate) fn config_whole(paths: &Paths) -> crate::Result<Option<Config>> {
    config_with(paths, |pin| {
        Ok(chain_cache::held(&paths.cache_file())?
            .map_or_else(Vec::new, |parsed| parsed.chain(pin)))
    })
}

/// `source.toml`, with `chain` answering the keys its signer was reached from where it names no
/// `followed`.
fn config_with(
    paths: &Paths,
    chain: impl FnOnce(&[u8; 32]) -> crate::Result<Vec<[u8; 32]>>,
) -> crate::Result<Option<Config>> {
    let Some(table) = read_toml(&paths.source_file(), "source.toml", 4 << 20)? else {
        return Ok(None);
    };
    let get = |key: &str| table.get(key).and_then(aterm_toml::Value::as_str);
    let source = match (get("url"), get("path")) {
        (Some(url), None) => Source::Url(url.to_string()),
        (None, Some(path)) => Source::Path(PathBuf::from(path)),
        _ => return Err(Error::new("source.toml: needs exactly one of url or path")),
    };
    let name = get("name")
        .ok_or_else(|| Error::new("source.toml: no name"))?
        .to_string();
    let signer = match get("signer") {
        Some(text) => {
            Some(decode_key32(text).ok_or_else(|| Error::new("source.toml: bad signer"))?)
        }
        None => None,
    };
    let mut followed = Vec::new();
    if let Some(keys) = table.get("followed") {
        let keys = keys
            .as_array()
            .ok_or_else(|| Error::new("source.toml: followed is not a list"))?;
        for key in keys {
            followed.push(
                key.as_str()
                    .and_then(decode_key32)
                    .ok_or_else(|| Error::new("source.toml: bad key in followed"))?,
            );
        }
    } else if let Some(pin) = &signer {
        followed = chain(pin)?;
    }
    let host = get("host").unwrap_or("machine").to_string();
    // A mirror carries no authority, so no entry of `mirrors` fails the read: one this build
    // does not keep (another vault's, a pin, a later build's form) costs that address alone.
    let mirrors = mirrors::sorted(&source, &name, table.get("mirrors")).kept;
    Ok(Some(Config {
        source,
        mirrors,
        name,
        signer,
        followed,
        host,
    }))
}

fn save_config(paths: &Paths, config: &Config) -> crate::Result<()> {
    fsx::ensure_private_dir(&paths.dir)?;
    let mut text =
        String::from("# aterm secrets: where this machine's vault is (aterm help secrets).\n");
    match &config.source {
        Source::Url(url) => text.push_str(&format!("url = {}\n", toml_str(url))),
        Source::Path(path) => {
            text.push_str(&format!("path = {}\n", toml_str(&path.to_string_lossy())))
        }
    }
    // Written only when there are some, so a file with none is rewritten as before.
    if matches!(config.source, Source::Url(_)) && !config.mirrors.is_empty() {
        let urls: Vec<String> = config.mirrors.iter().map(|url| toml_str(url)).collect();
        text.push_str(&format!(
            "# other addresses of this vault, read beside it and never written to (aterm secrets mirrors)\nmirrors = [{}]\n",
            urls.join(", ")
        ));
    }
    text.push_str(&format!("name = {}\n", toml_str(&config.name)));
    if let Some(signer) = &config.signer {
        text.push_str(&format!(
            "# the vault's signing key, pinned at join (fingerprint {})\nsigner = {}\n",
            envelope::fingerprint(signer),
            toml_str(&crate::b64::encode_padded(signer))
        ));
    }
    if !config.followed.is_empty() {
        let keys: Vec<String> = config
            .followed
            .iter()
            .map(|key| toml_str(&crate::b64::encode_padded(key)))
            .collect();
        text.push_str(&format!(
            "# the keys that pin was reached through, oldest first\nfollowed = [{}]\n",
            keys.join(", ")
        ));
    }
    text.push_str(&format!("host = {}\n", toml_str(&config.host)));
    fsx::write_atomic(&paths.source_file(), text.as_bytes(), 0o600)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stored {
    #[default]
    None,
    Pending,
    In,
    Revoked,
    Plain,
}

impl Stored {
    const fn word(self) -> &'static str {
        match self {
            Stored::None => "none",
            Stored::Pending => "pending",
            Stored::In => "in",
            Stored::Revoked => "revoked",
            Stored::Plain => "plain",
        }
    }

    fn from_word(word: &str) -> Stored {
        match word {
            "pending" => Stored::Pending,
            "in" => Stored::In,
            "revoked" => Stored::Revoked,
            "plain" => Stored::Plain,
            _ => Stored::None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct State {
    seq: u64,
    digest: Option<[u8; 32]>,
    records_digest: Option<[u8; 32]>,
    /// Complete selected entry semantics processed by the last actual materialization.
    materialization_selection: Option<materialization_selection::Meaning>,
    /// The source, vault name and signer this version and membership belong to.
    source_identity: Option<[u8; 32]>,
    /// The last successful actual URL body admission, kept separately from heartbeats.
    network_materialization: Option<network_observation::Receipt>,
    /// A retained actual configured-URL response; never inferred from the heard clock.
    http204_response: Option<http204_observation::Receipt>,
    /// Last retained actual network-refresh result, separate from local cache access.
    network_refresh: Option<network_refresh::Receipt>,
    heard: u64,
    requested: u64,
    membership: Stored,
    /// The join request's proof of the password, re-sent with each hourly request.
    proof: Option<String>,
    /// The tag ([`join::proof_tag`]) of a proof an admin Mac's verdict refused
    /// ([`read_verdict`]).
    refused: Option<String>,
    /// The last apply left a file of one of the vault's entries unwritten ([`apply_failed`]):
    /// the next fetch of the same version applies again.
    failed: bool,
    manifest: Manifest,
    /// Entry name → digest of each value a pass is to write, saved before it writes and cleared
    /// by its own save (a save that applies nothing keeps it; an apply keeps the digests of
    /// the entries it failed, whose write may have landed all the same), so the next apply
    /// still counts the files a cut-short pass wrote as the vault's
    /// ([`materialize::apply_resuming`]).
    pending: materialize::Pending,
    /// The entries the last apply left this Mac's own (an unanchored ` if-absent` entry left in
    /// place, a plain entry adopted): out of the manifest, so [`gaps`] looks at them apart, and
    /// one whose file is gone since is a gap the next pass fills. `None` in a state no apply of
    /// this record wrote (an older aterm's), until the next apply.
    own: Option<BTreeSet<String>>,
    /// The names `aterm secrets keep` made this Mac's own ([`keep`]): each stays in the
    /// manifest while its entry does, so its anchored set and a new value behave as before,
    /// and the pass that drops its entry is handed a manifest without it, so the file is left
    /// as the Mac's own instead of taken back ([`apply_entries`]).
    kept: BTreeSet<String>,
}

/// Fingerprint the successful-round inputs relevant to a service health receipt. Ordinary
/// heartbeat timestamps are excluded, while source, envelope, membership, pending writes and
/// signed record progress invalidate a previously answered challenge.
pub(crate) fn service_health_version(paths: &Paths) -> crate::Result<[u8; 32]> {
    let state = load_state(paths)?;
    let source = config(paths)?.as_ref().map(source_identity);
    let value = aterm_json::json!({
        "sequence":state.seq,
        "envelope":state.digest.map(|d|hex(&d)),
        "records":state.records_digest.map(|d|hex(&d)),
        "materialization_selection":state.materialization_selection.map(|meaning| hex(meaning.as_bytes())),
        "retained_history":hex(&retained_history::version(paths)?),
        "state_source":state.source_identity.map(|d|hex(&d)),
        "configured_source":source.map(|d|hex(&d)),
        "membership":state.membership.word(),
        "failed":state.failed,
        "pending":state.pending,
    });
    let bytes = aterm_json::to_vec(&value).map_err(|e| Error::new(e.to_string()))?;
    Ok(sha256(&bytes))
}

fn load_state(paths: &Paths) -> crate::Result<State> {
    Ok(read_state(paths)?.unwrap_or_default())
}

/// `state.toml`, `None` where there is none.
fn read_state(paths: &Paths) -> crate::Result<Option<State>> {
    let Some(table) = read_toml(&paths.state_file(), "state.toml", MAX_STATE)? else {
        return Ok(None);
    };
    let int = |key: &str| {
        table
            .get(key)
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0)
    };
    let digest = |key: &str| {
        table
            .get(key)
            .and_then(aterm_toml::Value::as_str)
            .and_then(|text| aterm_codec::hex::decode_vec(text.as_bytes()))
            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
    };
    let digests = |key: &str| {
        let mut digests = BTreeMap::new();
        if let Some(files) = table.get(key).and_then(aterm_toml::Value::as_table) {
            for (name, digest) in files {
                if let Some(digest) = digest.as_str() {
                    digests.insert(name.clone(), digest.to_string());
                }
            }
        }
        digests
    };
    let manifest = Manifest {
        files: digests("manifest"),
    };
    Ok(Some(State {
        seq: int("seq"),
        digest: digest("digest"),
        records_digest: digest("records_digest"),
        materialization_selection: digest("materialization_selection")
            .map(materialization_selection::Meaning::from_digest),
        source_identity: digest("source_identity"),
        network_materialization: network_observation::Receipt::parse(&table),
        http204_response: http204_observation::Receipt::parse(&table),
        network_refresh: network_refresh::Receipt::parse(&table),
        heard: int("heard"),
        requested: int("requested"),
        membership: Stored::from_word(
            table
                .get("membership")
                .and_then(aterm_toml::Value::as_str)
                .unwrap_or("none"),
        ),
        proof: table
            .get("proof")
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_string),
        refused: table
            .get("refused")
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_string),
        failed: table
            .get("failed")
            .and_then(aterm_toml::Value::as_bool)
            .unwrap_or(false),
        manifest,
        pending: digests("pending"),
        kept: table
            .get("kept")
            .and_then(aterm_toml::Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(aterm_toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        own: table
            .get("own")
            .and_then(aterm_toml::Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(aterm_toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            }),
    }))
}

/// What `state.toml` holds but for when the vault last heard this Mac and the receipts of the
/// last rounds (`heard`, the network materialization, `204` and refresh receipts), digested:
/// what the rounds of the vault's subscriber rewrite, each about once a minute
/// ([`HEARD_GRAIN_SECS`], and the receipts' own coalescing), while nothing else in the file
/// moves. `None` where there is no `state.toml`. A
/// reader that keeps what it read while this stands takes none of its answer from those fields
/// ([`vault_memo::desk_inputs`], aterm-agent's `fleet_log::LocalLog`): of the file's readers,
/// only [`status`] answers with `heard`, and only the receipts' own projections with a receipt.
/// The file is read only where it shows another stamp than the one it was last read under in
/// this process ([`vault_memo::state_standing_in`]): one `lstat` while it stands.
///
/// # Errors
/// `state.toml` cannot be read.
pub fn state_standing(paths: &Paths) -> crate::Result<Option<[u8; 32]>> {
    vault_memo::state_standing_in(paths, None)
}

/// [`state_standing`], and from the same read of `state.toml` when the vault last heard this Mac
/// where the state holds it in ([`Stored::In`]), as [`status`]'s [`Membership::In`] gives it
/// where a vault is configured: what [`vault_memo`]'s memos stamp the file by.
///
/// # Errors
/// `state.toml` cannot be read.
pub(super) fn state_standing_heard(
    paths: &Paths,
) -> crate::Result<Option<([u8; 32], Option<u64>)>> {
    Ok(read_state(paths)?.map(|state| {
        (
            sha256(ordinary_state(&state, false).as_bytes()),
            (state.membership == Stored::In).then_some(state.heard),
        )
    }))
}

fn save_state(paths: &Paths, state: &State) -> crate::Result<()> {
    fsx::ensure_private_dir(&paths.dir)?;
    let mut text = ordinary_state(state, true);
    if text.len() > MAX_STATE {
        return Err(Error::new(format!(
            "{}: would be larger than {MAX_STATE} bytes: the vault holds more entries than this machine can track",
            paths.state_file().display()
        )));
    }
    if let Some(receipt) = &state.network_materialization {
        let ordinary_size = text.len();
        receipt.write(&mut text);
        // Optional visibility must not make a previously writable ordinary state too large.
        if text.len() > MAX_STATE {
            text.truncate(ordinary_size);
        }
    }
    if let Some(receipt) = &state.http204_response {
        let previous_size = text.len();
        receipt.write(&mut text);
        if text.len() > MAX_STATE {
            text.truncate(previous_size);
        }
    }
    if let Some(receipt) = &state.network_refresh {
        let previous_size = text.len();
        receipt.write(&mut text);
        if text.len() > MAX_STATE {
            text.truncate(previous_size);
        }
    }
    // A round that changed nothing (the vault heard again within `HEARD_GRAIN_SECS`, no new
    // receipt) saves the text the file holds: it is left as it is, the same file for every
    // reader that keeps what it read by its stamp.
    fsx::write_atomic_if_changed(&paths.state_file(), text.as_bytes(), 0o600).map(|_| ())
}

/// The text of `state` that [`save_state`] writes before the receipts of the last rounds, with
/// `heard` or without it ([`state_standing`]).
fn ordinary_state(state: &State, heard: bool) -> String {
    let mut text = String::from(
        "# aterm secrets: what this machine holds. Written by aterm; names and digests only.\n",
    );
    text.push_str(&format!(
        "membership = {}\n",
        toml_str(state.membership.word())
    ));
    text.push_str(&format!(
        "seq = {}\n",
        i64::try_from(state.seq).unwrap_or(i64::MAX)
    ));
    if let Some(digest) = &state.digest {
        text.push_str(&format!("digest = {}\n", toml_str(&hex(digest))));
    }
    if let Some(records_digest) = &state.records_digest {
        text.push_str(&format!(
            "records_digest = {}\n",
            toml_str(&hex(records_digest))
        ));
    }
    if let Some(selection) = &state.materialization_selection {
        text.push_str(&format!(
            "materialization_selection = {}\n",
            toml_str(&hex(selection.as_bytes()))
        ));
    }
    if let Some(identity) = &state.source_identity {
        text.push_str(&format!("source_identity = {}\n", toml_str(&hex(identity))));
    }
    if heard {
        text.push_str(&format!(
            "heard = {}\n",
            i64::try_from(state.heard).unwrap_or(i64::MAX)
        ));
    }
    text.push_str(&format!(
        "requested = {}\n",
        i64::try_from(state.requested).unwrap_or(i64::MAX)
    ));
    if let Some(proof) = &state.proof {
        text.push_str(&format!("proof = {}\n", toml_str(proof)));
    }
    if let Some(refused) = &state.refused {
        text.push_str(&format!("refused = {}\n", toml_str(refused)));
    }
    if state.failed {
        text.push_str("failed = true\n");
    }
    if !state.kept.is_empty() {
        let names: Vec<String> = state.kept.iter().map(|name| toml_str(name)).collect();
        text.push_str(&format!("kept = [{}]\n", names.join(", ")));
    }
    if let Some(own) = &state.own {
        let names: Vec<String> = own.iter().map(|name| toml_str(name)).collect();
        text.push_str(&format!("own = [{}]\n", names.join(", ")));
    }
    text.push_str("\n[manifest]\n");
    for (name, digest) in &state.manifest.files {
        text.push_str(&format!("{} = {}\n", toml_str(name), toml_str(digest)));
    }
    if !state.pending.is_empty() {
        text.push_str("\n[pending]\n");
        for (name, digest) in &state.pending {
            text.push_str(&format!("{} = {}\n", toml_str(name), toml_str(digest)));
        }
    }
    text
}

fn source_identity(config: &Config) -> [u8; 32] {
    let mut identity = Vec::new();
    match &config.source {
        Source::Url(url) => {
            identity.push(0);
            identity.extend_from_slice(url.as_bytes());
        }
        Source::Path(path) => {
            identity.push(1);
            identity.extend_from_slice(path.to_string_lossy().as_bytes());
        }
    }
    identity.push(0);
    identity.extend_from_slice(config.name.as_bytes());
    identity.push(0);
    if let Some(signer) = config.signer {
        identity.push(1);
        identity.extend_from_slice(&signer);
    } else {
        identity.push(0);
    }
    sha256(&identity)
}

/// The source identity a Mac's `state.toml` binds its applied version to, for a test fixture in
/// another crate that moves a Mac's vault and binds its state to the new path (aterm-db's
/// `Lab::fork`).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn test_source_identity(config: &Config) -> [u8; 32] {
    source_identity(config)
}

fn source_bound(config: &Config, state: &State) -> bool {
    state.source_identity == Some(source_identity(config))
        && (config.signer.is_none() || state.digest.is_some())
}

fn log_only_source_bound(config: &Config, state: &State) -> bool {
    state.source_identity == Some(source_identity(config))
        && config.signer.is_some()
        && state.digest.is_none()
        && state.records_digest.is_some()
}

fn log_only_intact(paths: &Paths, config: &Config, state: &State) -> bool {
    if state.membership != Stored::In
        || !log_only_source_bound(config, state)
        || pin_gives_way(config)
        || fsx::kind(&paths.cache_file()).ok() != Some(fsx::Kind::Missing)
    {
        return false;
    }
    let Ok(Some(identity)) = read_identity(paths) else {
        return false;
    };
    let Ok(Some(configured)) = log_entries::trusted_configured_log(paths, config) else {
        return false;
    };
    Some(log_entries::records_heads_digest(&configured.log)) == state.records_digest
        && log_entries::log_alone_admits(&configured.log, &identity).unwrap_or(false)
}

fn prepare_materialization(paths: &Paths, state: &mut State) -> crate::Result<()> {
    state.failed = true;
    state.source_identity = None;
    state.materialization_selection = None;
    save_state(paths, state)
}

/// Whether `next`, a configuration written over `previous`, configures the vault `previous`
/// configures: the same name, and a key line (the signer and the keys it followed) that keeps a
/// key of the one before, as the history this Mac acknowledged ties a vault's membership to its
/// name and keys. A rotation (either side of it), another address or another host name keep it;
/// another vault, or the name on a line that keeps no key of the one before, is another.
fn same_vault(previous: &Config, next: &Config) -> bool {
    let line = |config: &Config| {
        config
            .signer
            .into_iter()
            .chain(config.followed.iter().copied())
            .collect::<Vec<_>>()
    };
    let before = line(previous);
    previous.name == next.name && line(next).iter().any(|key| before.contains(key))
}

/// Whether a configuration of a member's vault, `next` written over `previous` (a member's
/// `[everyone]` files, a log bundle where this Mac holds no log, or the log-member seam), starts
/// that vault afresh ([`forget_vault_left`]): no vault before, or another than [`same_vault`]
/// keeps. A log bundle where this Mac holds a log is judged by the two logs' genesis instead
/// (`admit::checked_log_bundle`).
fn moves_vault(previous: Option<&Config>, next: &Config) -> bool {
    !previous.is_some_and(|previous| same_vault(previous, next))
}

/// What a move to another vault forgets of `state` before the new vault's records or files are
/// written (a member's bundle that [`moves_vault`], or the log-member seam): the version, the
/// envelope digest and the records digest it applied of the vault left, so the new vault's first
/// records are acknowledged as a first enrollment (`retained_history`), not held to the old vault's
/// records digest, and its first version is no rollback of (nor a fork at) the old vault's
/// sequence. The caller removes the vault left's cached envelope too, so this Mac reads the new
/// vault from its log until its own envelope applies. A configuration of the same vault forgets
/// none of it, so what this Mac applied stays that vault's floor.
fn forget_vault_left(state: &mut State) {
    state.seq = 0;
    state.digest = None;
    state.records_digest = None;
}

/// What `aterm secrets status` shows; reads only local state.
pub fn status(paths: &Paths) -> crate::Result<Status> {
    let config = config(paths)?;
    let machine = read_identity(paths)?.map(|identity| {
        let recipient = identity.recipient();
        (recipient, join_code(&recipient))
    });
    let state = load_state(paths)?;
    let membership = match (&config, state.membership) {
        (None, _) => Membership::Unconfigured,
        (Some(_), Stored::In) => Membership::In {
            seq: state.seq,
            heard: state.heard,
        },
        (Some(_), Stored::Revoked) => Membership::Revoked { seq: state.seq },
        (Some(_), Stored::Plain) => Membership::Plain,
        (Some(_), Stored::Pending | Stored::None) => Membership::Pending,
    };
    Ok(Status {
        config,
        machine,
        membership,
        names: registry_names(&state),
    })
}

/// Names in this machine's applied registry, read from metadata without opening its
/// vault identity or any materialized credential.
///
/// # Errors
/// The saved client state cannot be read or parsed.
pub fn registered_names(paths: &Paths) -> crate::Result<Vec<String>> {
    Ok(registry_names(&load_state(paths)?))
}

fn registry_names(state: &State) -> Vec<String> {
    state
        .manifest
        .files
        .keys()
        .chain(state.own.iter().flatten())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// One entry whose register holds concurrent values ([`conflicts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Its section header and name (`[everyone] gh-token`), never a value.
    pub entry: String,
    /// Whether it is a Claude Code token or its record ([`crate::entry_class::claude_token`]),
    /// which no `aterm secrets set` writes: the token call's own write picks one
    /// ([`TOKEN_CONFLICT_PICK`]).
    pub token: bool,
}

/// What picks one value of a conflicted entry other than a Claude Code token's.
pub const CONFLICT_PICK: &str = "aterm secrets set <name> picks it";

/// What picks one value of a conflicted Claude Code token: the token call's write, which deletes
/// the concurrent values ahead of its own (DESIGN-claude-setup-token §3.5).
pub const TOKEN_CONFLICT_PICK: &str = "`aterm harness accounts token <label|email> --replace` picks a Claude Code token's, for the account `aterm harness accounts` shows it under";

impl Conflict {
    /// The entries of `conflicts`, in two lists: those [`CONFLICT_PICK`] picks, then the Claude
    /// Code tokens [`TOKEN_CONFLICT_PICK`] picks.
    #[must_use]
    pub fn split(conflicts: &[Self]) -> (Vec<&str>, Vec<&str>) {
        let named = |token: bool| {
            conflicts
                .iter()
                .filter(|conflict| conflict.token == token)
                .map(|conflict| conflict.entry.as_str())
                .collect()
        };
        (named(false), named(true))
    }
}

/// The entries this Mac reads from the membership log whose register holds concurrent values
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.3): two Macs saved different values while apart.
/// Every Mac writes the same one, and the other stays a candidate until a person saves or removes
/// the entry ([`CONFLICT_PICK`]), or, for a Claude Code token, until the token call writes it
/// again ([`TOKEN_CONFLICT_PICK`]). Each is named by its section header and name, never a value.
/// Empty where this Mac reads its entries from the envelope.
/// # Errors
/// The local state, identity or records cannot be read, or a record addressed to this Mac does
/// not open.
pub fn conflicts(paths: &Paths) -> crate::Result<Vec<Conflict>> {
    if load_state(paths)?.records_digest.is_none() {
        return Ok(Vec::new());
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(Vec::new());
    };
    let Some(config) = config(paths)? else {
        return Ok(Vec::new());
    };
    let store = load_records_for(&Reads::new(paths), &config)?;
    let store = log_entries::checked_author_projection_for(paths, &config, &store)?;
    let Some(opened) = store
        .log()
        .map(|log| log.open_entry_set(&identity))
        .transpose()?
        .flatten()
    else {
        return Ok(Vec::new());
    };
    Ok(opened
        .conflicts
        .into_iter()
        .map(|(audience, name)| Conflict {
            entry: format!("{} {name}", audience.header()),
            token: crate::entry_class::claude_token(&name),
        })
        .collect())
}

/// The files the vault wrote on this Mac, each at its path under `paths.home`
/// ([`materialize::target_path`]): its manifest's entries, and those a pass that was cut short
/// was writing. Read from local state alone: no key, source or written file is read, and a Mac
/// with no vault has none.
/// # Errors
/// The vault's local state cannot be read.
pub fn delivered_files(paths: &Paths) -> crate::Result<BTreeSet<PathBuf>> {
    let state = load_state(paths)?;
    Ok(state
        .manifest
        .files
        .keys()
        .chain(state.pending.keys())
        .filter_map(|name| materialize::target_path(&paths.home, name).ok())
        .collect())
}

/// What [`keep`] did with one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// Recorded kept: its file, which holds what the vault wrote, is left as the Mac's own once
    /// its entry goes, as an adopted file is.
    Kept,
    /// Nothing changed for this name: why (never the value).
    Not(String),
}

/// `aterm secrets keep <name>…`: make each file the vault wrote on this Mac the Mac's own
/// (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.9, step A14), so a later `Delete` or dropped
/// entry leaves it. A name is kept only while the manifest (or a cut pass's pending) holds it
/// and its file holds, by digest, what the vault wrote; a name the manifest does not hold, a
/// reserved name, an AI CLI's login and a missing or changed file change nothing.
///
/// The name is recorded in [`State::kept`] and stays in the manifest while its entry does: taken
/// out at once, an ` if-absent` file would count as the Mac's own and the vault's files anchored
/// to it would be withdrawn by the next pass. The pass that drops its entry is handed a manifest
/// without it, so that pass leaves the file and the name leaves both ([`apply_entries`]). Under
/// the state lock, so no pass runs between the look and the save. Reads only local state and
/// digests of the files: no value is returned, and a Mac with no vault state keeps nothing and
/// creates nothing.
/// # Errors
/// The vault's local state cannot be read, locked or saved.
pub fn keep(paths: &Paths, names: &[String]) -> crate::Result<Vec<(String, Kept)>> {
    let mut unique: Vec<&String> = Vec::new();
    for name in names {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    let not_held = |name: &str| {
        Kept::Not(format!(
            "{name} is not in this Mac's manifest: the vault did not write its file here"
        ))
    };
    if matches!(fsx::kind(&paths.state_file()), Ok(fsx::Kind::Missing)) {
        return Ok(unique
            .into_iter()
            .map(|name| (name.clone(), not_held(name)))
            .collect());
    }
    let _lock = StateLock::take(paths)?;
    let mut state = load_state(paths)?;
    let mut done = Vec::with_capacity(unique.len());
    let mut changed = false;
    for name in unique {
        let wrote: Vec<&String> = [state.manifest.files.get(name), state.pending.get(name)]
            .into_iter()
            .flatten()
            .collect();
        let verdict = match materialize::target_path(&paths.home, name) {
            Err(why) => Kept::Not(format!("{name}: {}", why.0)),
            Ok(_) if wrote.is_empty() => not_held(name),
            Ok(target) if matches!(fsx::kind(&target), Ok(fsx::Kind::Missing)) => {
                Kept::Not(format!(
                    "{name}: {} is missing: there is no file to keep",
                    target.display()
                ))
            }
            Ok(target)
                if !wrote
                    .iter()
                    .any(|digest| materialize::holds_digest(&paths.home, name, digest)) =>
            {
                Kept::Not(format!(
                    "{name}: {} does not hold what the vault wrote (changed, unreadable or not a plain file)",
                    target.display()
                ))
            }
            Ok(_) => {
                changed |= state.kept.insert(name.clone());
                Kept::Kept
            }
        };
        done.push((name.clone(), verdict));
    }
    if changed {
        save_state(paths, &state)?;
    }
    Ok(done)
}

/// Whether this machine's join request included a join-password proof.
pub fn asked_with_password(paths: &Paths) -> bool {
    load_state(paths).is_ok_and(|state| state.proof.is_some())
}

/// Whether an admin Mac's verdict, as last read ([`read_verdict`]), refused the proof this
/// waiting machine's join request carries now: its join password is not the vault's. From the
/// local records alone; a request with a new proof is not refused until a verdict on it is read.
pub fn password_refused(paths: &Paths) -> bool {
    load_state(paths).is_ok_and(|state| refused_now(&state))
}

fn refused_now(state: &State) -> bool {
    matches!(state.membership, Stored::Pending | Stored::None)
        && state
            .proof
            .as_deref()
            .zip(state.refused.as_deref())
            .is_some_and(|(proof, refused)| join::proof_tag(proof) == refused)
}

/// Ask the source whether an admin Mac refused this waiting machine's join request because its
/// proof of the join password did not match (`GET <url>/join/verdict`), and keep a refusal for
/// [`password_refused`]. Only a machine waiting with a proof at a URL asks; a server that keeps
/// no verdicts answers none. A verdict is unauthenticated, so a forged one only asks the person
/// for the password again.
/// # Errors
/// The local records cannot be read or written, or the server could not be asked.
pub fn read_verdict(paths: &Paths) -> crate::Result<bool> {
    read_verdict_within(paths, Duration::MAX)
}

/// [`read_verdict`], its exchange with the source within `total` (a bounded wait's).
/// # Errors
/// As [`read_verdict`]'s, and a source that did not answer within `total`.
pub fn read_verdict_within(paths: &Paths, total: Duration) -> crate::Result<bool> {
    read_verdict_within_admitted(paths, total, &|| Ok(()))
}

/// Read an admitted join's password verdict within `total`, retaining the caller's authority
/// before the fetch and before recording a refusal under the vault lock. `current` must be
/// read-only and acquire neither that lock nor one that waits for it.
///
/// # Errors
/// As [`read_verdict_within`], or the caller's authority is withdrawn.
pub fn read_verdict_within_admitted(
    paths: &Paths,
    total: Duration,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<bool> {
    current()?;
    let Some(config) = config(paths)? else {
        return Ok(false);
    };
    let Source::Url(url) = &config.source else {
        return Ok(false);
    };
    let state = load_state(paths)?;
    let Some(proof) = state
        .proof
        .clone()
        .filter(|_| matches!(state.membership, Stored::Pending | Stored::None))
    else {
        return Ok(false);
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(false);
    };
    let recipient = identity.recipient().to_bech32();
    let tag = join::proof_tag(&proof);
    let refused = transport::list_verdicts_within(url, total)?
        .iter()
        .any(|(key, refused)| *key == recipient && *refused == tag);
    if refused && state.refused.as_deref() != Some(tag.as_str()) {
        change_request_until_admitted(paths, &config, None, current, |state| {
            if state.proof.as_deref() != Some(proof.as_str()) {
                return false;
            }
            state.refused = Some(tag);
            true
        })?;
    }
    Ok(refused)
}

/// Whether an admitted vault applied the current selected entries and its tracked files match.
/// A writer holding the state lock (an apply, or the subscriber's pass over a long-poll answer)
/// is waited for, as [`MaterializationSnapshot::is_current`] waits for one, so a reader deciding
/// whether to act never reads that pass as a vault not ready. A status read that answers at once
/// is [`observe_materialization`].
/// # Errors
/// The local configuration or state cannot be safely read, or a writer held the state lock past
/// the wait ([`crate::ErrorKind::VaultBusy`]).
pub fn materialization_ready(paths: &Paths) -> crate::Result<bool> {
    materialization_ready_with(paths, None, local_files_intact)
}

/// Setup readiness, including an owned legacy lock that ordinary sync can repair.
/// This reads only: a repairable lock is not ready, and grants no credential access.
/// The setup's normal apply must still take and validate the lock before narrowing it.
/// # Errors
/// Readiness fails for any reason other than repairable permissions on an owned,
/// regular, single-link state lock that can already be opened for reading and writing.
pub fn materialization_ready_for_setup(paths: &Paths) -> crate::Result<bool> {
    let ready = materialization_ready(paths);
    if ready.is_err() && StateLock::permissions_need_repair(paths)? {
        return Ok(false);
    }
    ready
}

/// [`materialization_ready`] only while `expected` is still configured, waiting for a writer as
/// it does.
/// # Errors
/// As [`materialization_ready`]'s.
pub fn materialization_ready_for(paths: &Paths, expected: &Config) -> crate::Result<bool> {
    materialization_ready_with(paths, Some(expected), local_files_intact)
}

/// Local materialization facts that can change a setup retry's answer.
/// This is an observation, never authority to use credentials or change a vault.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MaterializationMark([u8; 32]);

impl std::fmt::Debug for MaterializationMark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MaterializationMark")
            .finish_non_exhaustive()
    }
}

impl MaterializationMark {
    /// Encode an opaque identity in a private retry cache, not in diagnostics.
    pub fn cache_key(&self) -> String {
        hex(&self.0)
    }

    /// Read an identity from a private retry cache.
    pub fn from_cache_key(key: &str) -> Option<Self> {
        let bytes = aterm_codec::hex::decode_vec(key.as_bytes())?;
        Some(Self(<[u8; 32]>::try_from(bytes.as_slice()).ok()?))
    }
}

/// Observe the configured version and whether its local files are complete.
/// A same-version repair and a plain vault's changed values move this mark;
/// heartbeats, join proofs and unchanged syncs do not. A busy or missing lock
/// gives no reading. No state is created, and no source is fetched.
pub fn materialization_mark(paths: &Paths) -> crate::Result<Option<MaterializationMark>> {
    let Some(_observation) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let state = load_state(paths)?;
    let ready = pin_admitted(&config, None)
        && matches!(state.membership, Stored::In | Stored::Plain)
        && (source_bound(&config, &state) || log_only_source_bound(&config, &state))
        && files_intact(paths, &state)
        && !(state.membership == Stored::In
            && config.signer.is_some()
            && steward::local_recipient_denied_in_roster(paths));
    let mut mark = aterm_digest::Sha256::new();
    mark.update(b"aterm materialization observation/1\0");
    mark.update(source_identity(&config));
    mark.update(retained_history::version(paths)?);
    mark.update(state.seq.to_be_bytes());
    mark.update([u8::from(ready), u8::from(state.digest.is_some())]);
    if let Some(digest) = state.digest.or(state.records_digest) {
        mark.update(digest);
    }
    for (name, digest) in &state.manifest.files {
        mark.update((name.len() as u64).to_be_bytes());
        mark.update(name.as_bytes());
        mark.update((digest.len() as u64).to_be_bytes());
        mark.update(digest.as_bytes());
    }
    Ok(Some(MaterializationMark(mark.finalize())))
}

/// The source and complete materialized version whose local inputs were read.
/// Heartbeats and pending join proofs are not part of this identity.
#[derive(Clone, Debug)]
pub struct MaterializationSnapshot {
    config: Config,
    seq: u64,
    digest: Option<[u8; 32]>,
    manifest: Manifest,
    /// A caller's registered input entries; `None` observes the whole materialization.
    entries: Option<BTreeSet<String>>,
    network_materialization: Option<VaultNetworkMaterialization>,
    selection: std::sync::Arc<materialization_selection::Fact>,
}

impl PartialEq for MaterializationSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
            && self.seq == other.seq
            && self.digest == other.digest
            && self.manifest == other.manifest
            && self.entries == other.entries
    }
}

impl Eq for MaterializationSnapshot {}

/// Local inputs read while their materialized version was held against writers.
pub struct Materialized<T> {
    pub inputs: T,
    pub snapshot: MaterializationSnapshot,
}

#[cfg(test)]
#[path = "client/materialization_scope_tests.rs"]
mod materialization_scope_tests;

/// Why one guarded local materialization observation could not read its inputs.
/// These reasons reveal no entry names, credentials, paths or source payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterializationBlocker {
    /// A writer (a vault pass on this machine) holds the state lock, which says nothing either
    /// way. Only a status observation ([`observe_materialization`]) answers this: a reader that
    /// gates work waits the writer out ([`StateLock::observe_judging`]) and errs where it outlasts
    /// the wait ([`writer_outlasted_wait`]).
    WriterBusy,
    /// No state lock file is there to observe: nothing was applied here, or it was removed.
    LockMissing,
    /// No vault source is configured under the observation lock.
    Unconfigured,
    /// The configured source no longer equals the caller's expected source.
    ConfigChanged,
    /// The configured signing authority is not admitted.
    UntrustedPin,
    /// The current local state records no admitted vault membership.
    NotAdmitted,
    /// The current local state records revoked vault membership and [`dropped`] does not hold:
    /// an act this Mac can read in its log took it out (a `Remove`, a node removal, a superseded
    /// seat), or its log does not say a rotation left it out on purpose (no trusted log; one that
    /// still seats or wraps it while it waits on a heal; or one whose newest epoch never knew
    /// it).
    Revoked,
    /// The vault let this Mac go with no act against it that it can read ([`dropped`], one-mesh
    /// E2): a rotation left out its recipient, which no seat bound, so it comes back only as a
    /// new Mac.
    Dropped,
    /// The applied state is not bound to the configured source and signing identity.
    SourceChanged,
    /// Apply, signed cache or tracked files are not complete and intact.
    LocalFilesIncomplete,
}

/// A local readiness verdict taken under the same guard as its successful input read.
pub enum MaterializationObservation<T> {
    Ready(Box<Materialized<T>>),
    Blocked(MaterializationBlocker),
}

/// How long a reader that judges the vault's state waits for a writer holding the state lock
/// ([`StateLock::observe_judging`]).
const CURRENT_WAIT: Duration = Duration::from_secs(30);

#[cfg(test)]
std::thread_local! {
    /// A test's own [`CURRENT_WAIT`] on this thread ([`with_current_wait`]).
    static TEST_CURRENT_WAIT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// [`CURRENT_WAIT`], or a test's own on this thread.
fn current_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = TEST_CURRENT_WAIT.with(std::cell::Cell::get) {
        return wait;
    }
    CURRENT_WAIT
}

/// Run `read` with `wait` in place of [`CURRENT_WAIT`] on this thread, so a test of a writer
/// that outlasts the wait ends in a moment.
#[cfg(test)]
pub(crate) fn with_current_wait<T>(wait: Duration, read: impl FnOnce() -> T) -> T {
    struct Restore(Option<Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_CURRENT_WAIT.with(|wait| wait.set(self.0));
        }
    }
    let _restore = Restore(TEST_CURRENT_WAIT.with(|old| old.replace(Some(wait))));
    read()
}

/// What a reader that judges the vault's state says where a writer still held the state lock
/// at the end of its wait ([`StateLock::observe_judging`]): a vault pass that long, not a change,
/// for nothing was read. `then` says what reads it once that pass ends.
pub(crate) fn writer_outlasted_wait(then: &str) -> Error {
    Error::of_kind(
        crate::ErrorKind::VaultBusy,
        format!(
            "a vault pass on this machine has held the vault's state lock for over {} s: {then}",
            CURRENT_WAIT.as_secs()
        ),
    )
}

/// [`writer_outlasted_wait`] for a read of the materialized vault.
fn materialization_writer_busy() -> Error {
    writer_outlasted_wait("the vault is read again once it ends")
}

impl MaterializationSnapshot {
    /// The historical materialized version admitted beside the captured local inputs.
    /// This is not their immutable origin or proof of current currency or signed delivery;
    /// unversioned Plain-file materialization records zero.
    pub const fn sequence(&self) -> u64 {
        self.seq
    }

    /// Observe authenticated public version evidence for this exact paired source. This
    /// performs no custody establishment or writes, and returns no provider-acceptance claim.
    /// # Errors
    /// The captured source/version or requested materialization no longer holds.
    pub fn authenticated_version(
        &self,
        paths: &Paths,
        entries: &[String],
    ) -> crate::Result<Option<version::AuthenticatedVersion>> {
        self.at_current_version(paths, entries, || {
            Ok(sight(paths)?.and_then(|sight| sight.version))
        })
    }

    /// An actual refusal effect may establish the existing retained-history owner before
    /// taking the shared observation used for its ledger mutation. Reads never call this.
    pub(crate) fn retain_for_refusal(&self, paths: &Paths) -> crate::Result<()> {
        if config(paths)?.as_ref() != Some(&self.config) {
            return Err(Error::new("the refused credential source changed"));
        }
        let lock = StateLock::take(paths)?;
        let (_, state) = materialization_source_state(paths, Some(&self.config), &lock)?
            .map_err(|_| Error::new("the refused credential source is unavailable"))?;
        if state.seq != self.seq
            || state.digest.or(state.records_digest) != self.digest
            || !self.matches_manifest(&state.manifest)
            || !files_intact(paths, &state)
        {
            return Err(Error::new("the refused credential version changed"));
        }
        Ok(())
    }

    /// Hold the ordinary readiness guard while using this exact source/version, rather
    /// than the looser scoped-file currency that permits unrelated later versions. A writer
    /// holding the state lock is waited for, as [`Self::is_current`] waits for one.
    pub(crate) fn at_current_version<T>(
        &self,
        paths: &Paths,
        entries: &[String],
        read: impl FnOnce() -> crate::Result<T>,
    ) -> crate::Result<T> {
        if entries.is_empty()
            || entries
                .iter()
                .any(|entry| !self.manifest.files.contains_key(entry))
            || self
                .entries
                .as_ref()
                .is_some_and(|covered| entries.iter().any(|entry| !covered.contains(entry)))
        {
            return Err(Error::new(
                "the captured version does not cover this credential want",
            ));
        }
        match with_ready_state_reusing(
            paths,
            Some(&self.config),
            local_files_intact,
            None,
            Some(&self.selection),
            OnWriter::Wait,
            |now| {
                if now.seq != self.seq || now.digest != self.digest || !now.holds_the_files_of(self)
                {
                    return Err(Error::new("the captured credential version changed"));
                }
                read()
            },
        )? {
            MaterializationObservation::Ready(ready) => Ok(ready.inputs),
            MaterializationObservation::Blocked(_) => {
                Err(Error::new("the captured credential source is not ready"))
            }
        }
    }

    /// Historical evidence from an actual signed URL body whose admitted context matched
    /// this guarded observation. It is not a claim of remote currency or provider sign-in.
    pub const fn network_materialization(&self) -> Option<VaultNetworkMaterialization> {
        self.network_materialization
    }

    /// Whether the same vault is still configured, ready and intact, holding the files this
    /// snapshot read: a later version that writes none of them (a machine let in, which on an
    /// open vault every join publishes) leaves it current, and one that changes, adds or
    /// removes a file does not. An entry-scoped observation compares only its named inputs,
    /// while still requiring the whole current materialization to be ready and intact.
    /// A writer holding the state lock (an apply, or the subscriber's pass over a long-poll
    /// answer) is waited for, up to [`CURRENT_WAIT`], since a busy lock says nothing either way
    /// ([`StateLock::observe_judging`]); one that outlasts the wait is an error saying so,
    /// never `false`. This is a point-in-time check, not a lease over later effects.
    /// # Errors
    /// The local configuration or state cannot be safely read, or a writer held the state lock
    /// past the wait ([`crate::ErrorKind::VaultBusy`]).
    pub fn is_current(&self, paths: &Paths) -> crate::Result<bool> {
        Ok(
            match with_ready_state_reusing(
                paths,
                Some(&self.config),
                local_files_intact,
                None,
                Some(&self.selection),
                OnWriter::Wait,
                |_| Ok(()),
            )? {
                MaterializationObservation::Ready(current) => {
                    current.snapshot.holds_the_files_of(self)
                }
                MaterializationObservation::Blocked(_) => false,
            },
        )
    }

    /// Whether the captured source's recorded membership and input digests still hold.
    /// Observes the state lock once, without waiting: a writer holding it says nothing either
    /// way, so the work goes on (`true`) and the next check reads again; a missing lock returns
    /// `false`. This is only a cancellation check for work admitted by a full readiness check:
    /// it does not verify cached authority, the membership log, or file contents, and must never
    /// admit an effect or report materialization ready. Call [`Self::is_current`] again at
    /// effect boundaries.
    /// # Errors
    /// The local configuration or state cannot be safely read.
    pub fn source_continues(&self, paths: &Paths) -> crate::Result<bool> {
        let observation = match StateLock::observe_classified(paths)? {
            StateObservation::Held(lock) => lock,
            StateObservation::Busy => return Ok(true),
            StateObservation::Absent => return Ok(false),
        };
        let Ok((_, state)) = materialization_source_state(paths, Some(&self.config), &observation)?
        else {
            return Ok(false);
        };
        Ok(!state.failed && state.pending.is_empty() && self.matches_manifest(&state.manifest))
    }

    /// The same vault, with the same files at the same digests ([`Self::is_current`]).
    fn holds_the_files_of(&self, then: &Self) -> bool {
        self.config == then.config && then.matches_manifest(&self.manifest)
    }

    fn matches_manifest(&self, manifest: &Manifest) -> bool {
        match &self.entries {
            None => self.manifest == *manifest,
            Some(entries) => entries.iter().all(|name| {
                self.manifest
                    .files
                    .get(name)
                    .is_some_and(|digest| manifest.files.get(name) == Some(digest))
            }),
        }
    }
}

/// Read local inputs under the ready vault's shared state lock, releasing it on return.
/// `read` must do local reads only: no network, subprocess or other effects. A writer holding
/// the state lock is waited for, as [`MaterializationSnapshot::is_current`] waits for one.
/// # Errors
/// The local configuration/state or the supplied input reader fails, or a writer held the
/// state lock past the wait ([`crate::ErrorKind::VaultBusy`]).
pub fn with_materialization_ready<T>(
    paths: &Paths,
    expected: &Config,
    read: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<Option<Materialized<T>>> {
    with_ready_state(
        paths,
        Some(expected),
        local_files_intact,
        None,
        OnWriter::Wait,
        |_| read(),
    )
    .map(MaterializationObservation::ready)
}

/// Capture local inputs under the complete ready vault's shared state lock, retaining only
/// the named registered entries as the subsequent snapshot's change dependencies. Unrelated
/// entry updates may leave that snapshot current; incomplete materialization, changed source,
/// revoked membership, or a missing or changed named entry may not. `read` must do local reads
/// only, with no network, subprocesses or effects. Missing entries return `None` before `read`.
/// A writer holding the state lock is waited for, as [`with_materialization_ready`] waits.
///
/// # Errors
/// The entry set is empty, local configuration/state cannot be read, the input reader fails,
/// or a writer held the state lock past the wait ([`crate::ErrorKind::VaultBusy`]).
pub fn with_materialization_ready_for_entries<T>(
    paths: &Paths,
    expected: &Config,
    entries: &[&str],
    read: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<Option<Materialized<T>>> {
    if entries.is_empty() {
        return Err(Error::new(
            "entry-scoped materialization needs at least one registered entry",
        ));
    }
    with_ready_state(
        paths,
        Some(expected),
        local_files_intact,
        Some(entries),
        OnWriter::Wait,
        |_| read(),
    )
    .map(MaterializationObservation::ready)
}

/// Observe readiness and read local inputs under one shared state lock.
/// `read` runs only when the configured vault still equals `expected` and its
/// materialization passes the ordinary readiness guard. It must perform only
/// local reads, with no network, subprocesses or other effects. This is a status
/// observation: a writer holding the state lock is its answer at once
/// ([`MaterializationBlocker::WriterBusy`]), not waited for.
/// # Errors
/// The local configuration/state or the supplied input reader fails.
pub fn observe_materialization<T>(
    paths: &Paths,
    expected: &Config,
    read: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<MaterializationObservation<T>> {
    with_ready_state(
        paths,
        Some(expected),
        local_files_intact,
        None,
        OnWriter::Answer,
        |_| read(),
    )
}

fn materialization_ready_with(
    paths: &Paths,
    expected: Option<&Config>,
    intact: impl FnOnce(&Paths, &State) -> bool,
) -> crate::Result<bool> {
    Ok(matches!(
        with_ready_state(paths, expected, intact, None, OnWriter::Wait, |_| Ok(()))?,
        MaterializationObservation::Ready(_)
    ))
}

impl<T> MaterializationObservation<T> {
    /// The inputs a ready observation read; `None` where it was blocked.
    fn ready(self) -> Option<Materialized<T>> {
        match self {
            Self::Ready(inputs) => Some(*inputs),
            Self::Blocked(_) => None,
        }
    }
}

/// How a readiness read meets a writer holding the state lock.
#[derive(Clone, Copy)]
enum OnWriter {
    /// A status observation answers [`MaterializationBlocker::WriterBusy`] at once.
    Answer,
    /// A reader that gates work waits it out ([`StateLock::observe_judging`]) and errs where it
    /// outlasts the wait ([`writer_outlasted_wait`]).
    Wait,
}

/// The common recorded source checks, with the caller holding the state lock.
/// Full readiness additionally verifies denial evidence, cached authority and files.
fn materialization_source_state(
    paths: &Paths,
    expected: Option<&Config>,
    _observation: &StateLock,
) -> crate::Result<Result<(Config, State), MaterializationBlocker>> {
    use MaterializationBlocker as Blocker;
    let Some(config) = config(paths)? else {
        return Ok(Err(Blocker::Unconfigured));
    };
    if expected.is_some_and(|expected| *expected != config) {
        return Ok(Err(Blocker::ConfigChanged));
    }
    if !pin_admitted(&config, None) {
        return Ok(Err(Blocker::UntrustedPin));
    }
    let state = load_state(paths)?;
    if state.membership == Stored::Revoked {
        return Ok(Err(Blocker::Revoked));
    }
    if !matches!(state.membership, Stored::In | Stored::Plain) {
        return Ok(Err(Blocker::NotAdmitted));
    }
    if !source_bound(&config, &state) && !log_only_source_bound(&config, &state) {
        return Ok(Err(Blocker::SourceChanged));
    }
    Ok(Ok((config, state)))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "Unready",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
fn with_ready_state<T>(
    paths: &Paths,
    expected: Option<&Config>,
    intact: impl FnOnce(&Paths, &State) -> bool,
    entries: Option<&[&str]>,
    on_writer: OnWriter,
    read: impl FnOnce(&MaterializationSnapshot) -> crate::Result<T>,
) -> crate::Result<MaterializationObservation<T>> {
    with_ready_state_reusing(paths, expected, intact, entries, None, on_writer, read)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "StaleSelection",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ReappliedSelection",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
fn with_ready_state_reusing<T>(
    paths: &Paths,
    expected: Option<&Config>,
    intact: impl FnOnce(&Paths, &State) -> bool,
    entries: Option<&[&str]>,
    prior: Option<&std::sync::Arc<materialization_selection::Fact>>,
    on_writer: OnWriter,
    read: impl FnOnce(&MaterializationSnapshot) -> crate::Result<T>,
) -> crate::Result<MaterializationObservation<T>> {
    use MaterializationBlocker as Blocker;
    let blocked = |why| Ok(MaterializationObservation::Blocked(why));
    let observation = match on_writer {
        OnWriter::Answer => StateLock::observe_classified(paths)?,
        OnWriter::Wait => StateLock::observe_judging(paths)?,
    };
    let observation = match (observation, on_writer) {
        (StateObservation::Held(lock), _) => lock,
        (StateObservation::Absent, _) => return blocked(Blocker::LockMissing),
        (StateObservation::Busy, OnWriter::Answer) => return blocked(Blocker::WriterBusy),
        (StateObservation::Busy, OnWriter::Wait) => return Err(materialization_writer_busy()),
    };
    let (config, state) = match materialization_source_state(paths, expected, &observation)? {
        Ok(ready) => ready,
        Err(Blocker::Revoked) if dropped(paths) => return blocked(Blocker::Dropped),
        Err(why) => return blocked(why),
    };
    if state.membership == Stored::In
        && config.signer.is_some()
        && steward::local_recipient_denied_in_roster_held(paths, &observation)
    {
        return blocked(Blocker::Revoked);
    }
    if !intact(paths, &state) {
        return blocked(Blocker::LocalFilesIncomplete);
    }
    let Some(applied) = state.materialization_selection else {
        return blocked(Blocker::LocalFilesIncomplete);
    };
    let Some(selection) =
        materialization_selection::observe(paths, &config, &state, &observation, prior)
            .ok()
            .flatten()
    else {
        return blocked(Blocker::LocalFilesIncomplete);
    };
    if selection.meaning != applied {
        return blocked(Blocker::LocalFilesIncomplete);
    }
    let entries = if let Some(entries) = entries {
        if entries
            .iter()
            .any(|name| !state.manifest.files.contains_key(*name))
        {
            return blocked(Blocker::LocalFilesIncomplete);
        }
        Some(entries.iter().map(|name| (*name).to_owned()).collect())
    } else {
        None
    };
    let snapshot = MaterializationSnapshot {
        network_materialization: network_observation::project(paths, &config, &state),
        selection,
        config,
        seq: state.seq,
        digest: state.digest.or(state.records_digest),
        manifest: state.manifest,
        entries,
    };
    Ok(MaterializationObservation::Ready(Box::new(Materialized {
        inputs: read(&snapshot)?,
        snapshot,
    })))
}

struct OpenedLockers {
    everyone: BTreeMap<String, Entry>,
    hosts: BTreeMap<String, Entry>,
}

fn open_lockers(envelope: &Envelope, identity: &Identity) -> crate::Result<Option<Vec<Entry>>> {
    let Some(mut entries) = open_locker_audiences(envelope, identity)? else {
        return Ok(None);
    };
    entries.everyone.extend(entries.hosts);
    Ok(Some(entries.everyone.into_values().collect()))
}

/// Validate every readable locker before applying any cross-audience precedence.
fn open_locker_audiences(
    envelope: &Envelope,
    identity: &Identity,
) -> crate::Result<Option<OpenedLockers>> {
    let identities = std::slice::from_ref(identity);
    let mut everyone: BTreeMap<String, Entry> = BTreeMap::new();
    let mut hosts: BTreeMap<String, Entry> = BTreeMap::new();
    let mut opened = false;
    for locker in envelope
        .lockers
        .iter()
        .filter(|locker| locker.label != "admin")
    {
        let plain = match age::decrypt(identities, locker.armored.as_bytes()) {
            Ok(plain) => plain,
            Err(age::AgeError::NoMatch) => continue,
            Err(error) => {
                return Err(Error::new(format!("locker {}: {error}", locker.label)));
            }
        };
        opened = true;
        let entries = text::parse_locker(plain.as_slice())
            .map_err(|e| Error::new(format!("locker {}: {e}", locker.label)))?;
        for entry in entries {
            let into = match entry.audience {
                Audience::Everyone => &mut everyone,
                Audience::Hosts(_) => &mut hosts,
            };
            if into.contains_key(&entry.name) {
                return Err(Error::new(format!(
                    "entry {} is addressed to this machine twice",
                    entry.name
                )));
            }
            into.insert(entry.name.clone(), entry);
        }
    }
    if !opened {
        return Ok(None);
    }
    Ok(Some(OpenedLockers { everyone, hosts }))
}

/// What this Mac's applied vault version shows for credential requests' absence proof
/// (docs/DESIGN-credential-requests-2026-10-27.md §9.4): the entry names in every locker this
/// Mac opens, and whether any locker but `admin` is one it cannot. Read under the same shared
/// lock, pin and version checks as [`observe_commons`], from the cached envelope the subscriber
/// last applied; `None` when this Mac holds no applied signed version (none joined, a plain
/// file, a pending apply, or a concurrent update holding the lock). Values are decrypted to
/// read their names and dropped wiped; none leaves this function.
pub(crate) fn sight(paths: &Paths) -> crate::Result<Option<crate::need::Sight>> {
    let Some(_observation) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let Some(pin) = config.signer else {
        return Ok(None);
    };
    let state = load_state(paths)?;
    if state.membership != Stored::In || state.failed || !state.pending.is_empty() {
        return Ok(None);
    }
    if state.digest.is_none() && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing {
        if state.source_identity != Some(source_identity(&config)) || !pin_admitted(&config, None) {
            return Ok(None);
        }
        let Some(identity) = read_identity(paths)? else {
            return Ok(None);
        };
        if let Some(configured) = log_entries::trusted_configured_log(paths, &config)? {
            if !log_entries::log_alone_admits(&configured.log, &identity)?
                || steward::local_recipient_denied_in_roster(paths)
            {
                return Ok(None);
            }
            let Some(opened) = configured.log.open_entry_set(&identity)? else {
                return Ok(None);
            };
            let held: BTreeSet<String> =
                opened.entries.into_iter().map(|entry| entry.name).collect();
            let alab = alab_vault(&config);
            let host = if config.host.is_empty() {
                let rec = identity.recipient();
                let membership = configured.log.membership();
                membership
                    .members()
                    .iter()
                    .filter(|(_, m)| m.binding.recipient() == rec)
                    .max_by_key(|(actor, m)| {
                        (configured.log.record_index(&m.entry).unwrap_or(0), *actor)
                    })
                    .map(|(_, m)| {
                        if m.name.is_empty() {
                            m.label.clone()
                        } else {
                            m.name.clone()
                        }
                    })
                    .or_else(|| {
                        membership
                            .unproved()
                            .iter()
                            .find(|f| f.recipient == rec)
                            .map(|f| {
                                if f.name.is_empty() {
                                    f.label.clone()
                                } else {
                                    f.name.clone()
                                }
                            })
                    })
                    .unwrap_or_default()
            } else {
                config.host
            };
            return Ok(Some(crate::need::Sight {
                version: Some(version::AuthenticatedVersion::from_source(
                    &config.name,
                    pin,
                    config.followed.iter().copied(),
                    None,
                    Some(&configured.log),
                )?),
                alab,
                vault: config.name,
                host,
                seq: state.seq,
                signer: pin,
                held,
            }));
        }
    }
    let Some(digest) = state.digest else {
        return Ok(None);
    };
    if !source_bound(&config, &state) {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let Some(held_vault) = chain_cache::held(&paths.cache_file())? else {
        return Ok(None);
    };
    let (envelope, pinned, verdict) = envelope::verify_for_reader(
        held_vault.bytes(),
        &config.name,
        &pin,
        &config.followed,
        Some(&Floor::version(state.seq, digest)),
    )?;
    if verdict != Verdict::Unchanged || !matches!(pinned, Pin::Same) {
        return Ok(None);
    }
    let seeded = log_entries::trusted_seeded_log(paths, &config, &envelope)?;
    if !log_entries::applied_log_available(&state, seeded.as_ref().map(|(_, digest)| *digest)) {
        return Ok(None);
    }
    let mut held = BTreeSet::new();
    let mut accepted_log = None;
    if steward::local_recipient_denied_in_roster(paths) {
        return Ok(None);
    }
    if let Some((log, _)) = &seeded
        && (log.removes(&identity.recipient())
            || (log_entries::log_base(Some(log), &envelope, &identity)?
                != log_entries::LogBase::Behind
                && log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &identity.recipient(),
                )))
    {
        return Ok(None);
    }
    if let Some((log, _)) = &seeded
        && log_entries::log_base(Some(log), &envelope, &identity)? == log_entries::LogBase::Covers
        && log_entries::reads_log(log, &identity)
    {
        accepted_log = Some(log);
        let Some(opened) = log.open_entry_set(&identity)? else {
            return Ok(None);
        };
        held.extend(opened.entries.into_iter().map(|entry| entry.name));
    } else {
        for locker in envelope
            .lockers
            .iter()
            .filter(|locker| locker.label != "admin")
        {
            let plain =
                match age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes()) {
                    Ok(plain) => plain,
                    Err(age::AgeError::NoMatch) => continue,
                    Err(error) => {
                        return Err(Error::new(format!("locker {}: {error}", locker.label)));
                    }
                };
            let entries = text::parse_locker(plain.as_slice())
                .map_err(|error| Error::new(format!("locker {}: {error}", locker.label)))?;
            held.extend(entries.into_iter().map(|entry| entry.name));
        }
    }
    Ok(Some(crate::need::Sight {
        version: Some(version::AuthenticatedVersion::from_source(
            &config.name,
            envelope.signer,
            config
                .followed
                .iter()
                .copied()
                .chain(envelope.reached_from()),
            Some(version::EnvelopeVersion {
                seq: envelope.seq,
                digest: envelope.digest(),
            }),
            accepted_log,
        )?),
        alab: alab_vault(&config),
        vault: config.name,
        host: config.host,
        seq: state.seq,
        signer: envelope.signer,
        held,
    }))
}

/// A shared vault-state lock for a bounded local Commons read. It grants no
/// authority: the caller still validates custody, membership and policy.
/// Drop it before network I/O or any vault mutation.
#[must_use = "hold the source read guard for the local batch"]
pub struct CommonsSourceRead {
    lock: StateLock,
}

/// Try to hold existing vault state for a local Commons batch. Missing or busy
/// state returns `None`; acquisition never waits or creates state. This grants
/// no authority: admit and revalidate the source after acquiring its guard, and
/// drop it before network I/O or a vault mutation.
///
/// # Errors
/// The existing lock cannot be opened safely or its shared lock fails.
pub fn try_commons_source_read(paths: &Paths) -> crate::Result<Option<CommonsSourceRead>> {
    Ok(StateLock::observe(paths)?.map(|lock| CommonsSourceRead { lock }))
}

impl CommonsSourceRead {
    /// This read as a look at the vault at `paths` ([`VaultLook`]), which stands on the read's
    /// observation of the state lock for as long as it held the read, rather than observing the
    /// lock again: `aterm-db`'s table-repair read batch, which holds its vault's source read from
    /// before its admission until its batch returns, judges the batch through it, and its store
    /// opens and store read batches take it as their source read of the vault
    /// (`Bootstrap::local_source_read_in`). `paths` must be
    /// the vault this read was taken of ([`try_commons_source_read`]); whether the observation
    /// guards the lock at that vault's path the look judges at its first ask, through its glance
    /// ([`StateLock::observes_in`]).
    #[must_use]
    pub fn into_look(self, paths: &Paths) -> VaultLook<'_> {
        VaultLook::standing_on(paths, self.lock)
    }
}

/// How long a file must have stood unchanged before a Commons journal view or a kept file digest
/// keeps what it read of it (`fsx`'s settled rule; a file changed since is read again at each
/// ask): a measurement of quiet rounds waits it out, and a tenth of a second past it, after its
/// fixture's last write, so its rounds read the vault as an idle Mac's do (`aterm-db`'s
/// `quiet_rounds`).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub const fn test_settled_after() -> Duration {
    crate::fsx::SETTLED
}

/// Run `f` with each kept file digest this thread asks, and each look at the vault it begins, as
/// if asked `by` later than the clock (`fsx`'s settled rule): a test's vault files have stood
/// settled ([`test_settled_after`]) without its waiting, so its reads keep what an idle Mac's
/// keep, whatever time its fixture took. Where `by` is not zero, the digests kept within it are
/// this thread's own, which no other thread's asks let go of.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn test_as_if_later<T>(by: Duration, f: impl FnOnce() -> T) -> T {
    crate::fsx::as_if_later(by, f)
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// How many times this thread asked to hold a vault's source read ([`hold_commons_source`]),
    /// whether or not it held one.
    static SOURCE_READS_HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread asked to hold a source read of a vault for a local batch's checks
/// (`Bootstrap::local_source_read`), each a shared observation of its state lock where no writer
/// holds it, whether or not it held one; one a look already holding the vault stands for
/// (`Bootstrap::local_source_read_in`) adds none.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_source_reads_held() -> usize {
    SOURCE_READS_HELD.with(std::cell::Cell::get)
}

pub(crate) fn hold_commons_source(paths: &Paths) -> crate::Result<CommonsSourceRead> {
    use crate::commons::Unavailable;
    #[cfg(any(test, debug_assertions))]
    SOURCE_READS_HELD.with(|reads| reads.set(reads.get() + 1));
    let unavailable = |reason: Unavailable| {
        reason.failure("Commons vault became unavailable while acquiring custody")
    };
    match StateLock::observe_classified(paths)? {
        StateObservation::Held(lock) => Ok(CommonsSourceRead { lock }),
        StateObservation::Absent => Err(unavailable(Unavailable::MissingStateLock)),
        StateObservation::Busy => Err(unavailable(Unavailable::Busy)),
    }
}

/// The offline Commons reader, kept beside the vault's lock, pin and version
/// checks so it cannot accidentally bypass the locally applied source binding.
pub(crate) fn observe_commons(paths: &Paths) -> crate::Result<crate::commons::Observation> {
    use crate::commons::{Observation, Unavailable};
    type ObserveCommonsKey = (
        Config,
        u64,
        Option<[u8; 32]>,
        [u8; 32],
        [u8; 32],
        Option<[u8; 32]>,
    );
    static OBSERVE_COMMONS_CACHE: std::sync::Mutex<
        std::collections::VecDeque<(ObserveCommonsKey, crate::commons::Snapshot)>,
    > = std::sync::Mutex::new(std::collections::VecDeque::new());

    let unavailable = |reason| Ok(Observation::Unavailable(reason));
    let Some(_observation) = StateLock::observe(paths)? else {
        if fsx::kind(&paths.lock_file())? != fsx::Kind::Missing {
            return unavailable(Unavailable::Busy);
        }
        if config(paths)?.is_none() {
            return unavailable(Unavailable::Unconfigured);
        }
        return unavailable(Unavailable::MissingStateLock);
    };
    let reads = Reads::new(paths);
    let Some(config) = reads.config()? else {
        return unavailable(Unavailable::Unconfigured);
    };
    let Some(pin) = config.signer else {
        return unavailable(Unavailable::UnsignedSource);
    };
    let state = reads.state()?;
    if state.membership == Stored::Revoked {
        return unavailable(if dropped(paths) {
            Unavailable::Dropped
        } else {
            Unavailable::Revoked
        });
    }
    if state.membership != Stored::In {
        return unavailable(Unavailable::NotAdmitted);
    }
    if state.failed || !state.pending.is_empty() {
        return unavailable(Unavailable::IncompleteApply);
    }
    if state.digest.is_none() && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing {
        if state.source_identity != Some(source_identity(config)) {
            return unavailable(Unavailable::SourceChanged);
        }
        if !pin_admitted(config, None) {
            return unavailable(Unavailable::UntrustedPin);
        }
        let Some(identity) = reads.identity()? else {
            return unavailable(Unavailable::MissingIdentity);
        };
        let Some(configured) = reads.configured()? else {
            return unavailable(Unavailable::IncompleteApply);
        };
        let log = configured.log();
        let obs_key = (
            config.clone(),
            state.seq,
            None,
            identity.recipient().0,
            [0; 32],
            Some(configured.digest()),
        );
        if log.recipient_is_removed(&identity.recipient())
            || log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
            || steward::local_recipient_denied(&reads)?
        {
            return unavailable(Unavailable::Revoked);
        }
        if log.audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Commons,
            &identity.recipient(),
        ) {
            return unavailable(Unavailable::ControlExcluded);
        }
        if let Ok(cache) = OBSERVE_COMMONS_CACHE.lock()
            && let Some((_, snapshot)) = cache.iter().find(|(k, _)| *k == obs_key)
        {
            return Ok(Observation::Visible(snapshot.clone()));
        }
        return Ok(match open_commons_control_from_log(log, identity)? {
            Some(control) => {
                let snapshot = crate::commons::Snapshot::verified_from_log(
                    config.clone(),
                    pin,
                    *log.genesis().id(),
                    identity.recipient(),
                    control,
                );
                if let Ok(mut cache) = OBSERVE_COMMONS_CACHE.lock() {
                    if cache.len() >= 16 {
                        cache.pop_front();
                    }
                    cache.push_back((obs_key, snapshot.clone()));
                }
                Observation::Visible(snapshot)
            }
            None => Observation::Unavailable(Unavailable::ControlNotVisible),
        });
    }
    let Some(digest) = state.digest else {
        return unavailable(Unavailable::IncompleteApply);
    };
    if !source_bound(config, state) {
        return unavailable(Unavailable::SourceChanged);
    }
    if !pin_admitted(config, None) {
        return unavailable(Unavailable::UntrustedPin);
    }
    let Some(identity) = reads.identity()? else {
        return unavailable(Unavailable::MissingIdentity);
    };
    let Some(vault) = reads.vault()? else {
        return unavailable(Unavailable::MissingCache);
    };
    let (envelope, pinned, verdict) = vault.verify_for_reader(
        &config.name,
        &pin,
        &config.followed,
        Some(&Floor::version(state.seq, digest)),
    )?;
    if verdict != Verdict::Unchanged || !matches!(pinned, Pin::Same) {
        return unavailable(Unavailable::PendingApply);
    }
    let mut control = None;
    let seeded = reads.seeded()?;
    if !log_entries::applied_log_available(state, seeded.as_ref().map(HeldLog::digest)) {
        return unavailable(Unavailable::IncompleteApply);
    }
    if steward::local_recipient_denied(&reads)? {
        return unavailable(Unavailable::Revoked);
    }
    // How the seeded log stands to this envelope, asked once it holds no explicit removal.
    let seeded_base = match &seeded {
        Some(seeded) if seeded.log().removes(&identity.recipient()) => {
            return unavailable(Unavailable::Revoked);
        }
        Some(seeded) => Some((
            seeded.log(),
            log_entries::log_base(Some(seeded.log()), envelope, identity)?,
        )),
        None => None,
    };
    if let Some((log, base)) = seeded_base
        && base != log_entries::LogBase::Behind
        && log.audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Everyone,
            &identity.recipient(),
        )
    {
        return unavailable(Unavailable::Revoked);
    }
    if let Some((log, base)) = seeded_base
        && log.has_commons_epochs()
        && base != log_entries::LogBase::Behind
        && log.audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Commons,
            &identity.recipient(),
        )
    {
        return unavailable(Unavailable::ControlExcluded);
    }
    let obs_key = (
        config.clone(),
        state.seq,
        Some(digest),
        identity.recipient().0,
        envelope.digest(),
        seeded.as_ref().map(HeldLog::digest),
    );
    if let Ok(cache) = OBSERVE_COMMONS_CACHE.lock()
        && let Some((_, snapshot)) = cache.iter().find(|(k, _)| *k == obs_key)
    {
        return Ok(Observation::Visible(snapshot.clone()));
    }
    if let Some((log, base)) = seeded_base
        && log.has_commons_epochs()
        && base != log_entries::LogBase::Behind
        && log_entries::reads_log(log, identity)
    {
        control = log.open_commons_control(identity)?;
    } else if let Some((log, base)) = seeded_base
        && base == log_entries::LogBase::Covers
        && log_entries::reads_log(log, identity)
    {
        control = open_commons_control_from_log(log, identity)?;
    } else {
        for locker in envelope
            .lockers
            .iter()
            .filter(|locker| locker.label != "admin")
        {
            let plain =
                match age::decrypt(std::slice::from_ref(identity), locker.armored.as_bytes()) {
                    Ok(plain) => plain,
                    Err(age::AgeError::NoMatch) => continue,
                    Err(error) => return Err(Error::new(format!("Commons locker: {error}"))),
                };
            let entries = text::parse_locker(plain.as_slice())
                .map_err(|error| Error::new(format!("Commons locker: {error}")))?;
            for entry in entries
                .into_iter()
                .filter(|entry| entry.name == crate::commons::ENTRY)
            {
                // Host labels can be renamed while a device is offline. The vault
                // signer addressed this ciphertext to the proven age recipient;
                // its consumer must check stable member identities, not labels.
                if !matches!(&entry.audience, Audience::Hosts(_))
                    || entry.if_absent
                    || entry.anchor.is_some()
                    || control.is_some()
                {
                    return Err(Error::new(
                        "Commons control audience is not an unambiguous explicit recipient grant",
                    ));
                }
                control = Some(entry.value);
            }
        }
    }
    Ok(match control {
        Some(control) => {
            let snapshot = crate::commons::Snapshot::verified(
                config.clone(),
                envelope,
                identity.recipient(),
                control,
            );
            if let Ok(mut cache) = OBSERVE_COMMONS_CACHE.lock() {
                if cache.len() >= 16 {
                    cache.pop_front();
                }
                cache.push_back((obs_key, snapshot.clone()));
            }
            Observation::Visible(snapshot)
        }
        None => Observation::Unavailable(Unavailable::ControlNotVisible),
    })
}

fn open_commons_control_from_log(
    log: &crate::membership_log::Log,
    identity: &Identity,
) -> crate::Result<Option<crate::SecretBytes>> {
    if log.audience_left_out_recipient(
        &crate::membership_log::EpochAudience::Everyone,
        &identity.recipient(),
    ) {
        return Ok(None);
    }
    if log.has_commons_epochs() {
        return log.open_commons_control(identity);
    }
    let Some(opened) = log.open_audience_entries(identity)? else {
        return Ok(None);
    };
    let mut control = None;
    // Approvals on both sides of a split leave the control entry live in two host lists;
    // the log's reader takes the winning one, as `Log::open_entry_set` does.
    let winning = opened
        .winning_host_audience
        .get(crate::commons::ENTRY)
        .cloned();
    for (audience, map) in opened.by_audience {
        if matches!(&audience, Audience::Hosts(_)) && winning.as_ref() != Some(&audience) {
            continue;
        }
        if let Some(entry) = map.into_values().find(|e| e.name == crate::commons::ENTRY) {
            if !matches!(&audience, Audience::Hosts(_))
                || entry.if_absent
                || entry.anchor.is_some()
                || control.is_some()
            {
                return Err(Error::new(
                    "Commons control audience is not an unambiguous explicit recipient grant",
                ));
            }
            control = Some(entry.value);
        }
    }
    Ok(control)
}

fn plain_entries(config: &Config, path: &Path) -> crate::Result<Vec<Entry>> {
    let bytes = fsx::read_private(path, text::MAX_TEXT)?
        .ok_or_else(|| Error::new(format!("{}: no such file", path.display())))?;
    text::parse_plain(bytes.as_slice())
        .and_then(|entries| text::merge_for(&config.host, entries))
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

fn is_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(envelope::MAGIC.as_bytes())
}

/// What the vault at a source answers now ([`look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Look {
    /// A vault answers at the URL or path.
    Answers,
    /// Transient transport failure or unreached source; on macOS a source on the local network
    /// whose LAN broker is unavailable, in the broker's words.
    Unreached(String),
    /// The endpoint or path holds no vault.
    NoVault(String),
}

/// How long a look waits between two asks of the LAN broker while it is unavailable.
const LOOK_PAUSE: Duration = Duration::from_secs(2);

/// How long one try of the look before a join or a sync ([`look`]'s `wait`) waits for the vault.
pub const REACH_WAIT: Duration = Duration::from_secs(30);

/// The patience a setup's look takes ([`look`]'s `patience`): how long it waits for this Mac's
/// LAN broker to answer before it says the broker is unavailable.
pub const CONSENT_WAIT: Duration = Duration::from_secs(3 * 60);

/// The longest a [`look`] with [`CONSENT_WAIT`]'s patience and [`REACH_WAIT`]'s try runs: the
/// patience, its last pause, then the try. A watcher of a setup that runs one waits this long
/// before its own hang detector starts to count.
pub const PATIENT_LOOK_MAX: Duration =
    Duration::from_secs(CONSENT_WAIT.as_secs() + REACH_WAIT.as_secs() + LOOK_PAUSE.as_secs());

trait Tries {
    /// Whether the LAN broker `url` needs answers: `Err` in its words while it is unavailable,
    /// `Ok` once it answers, or when `url` needs no broker.
    fn broker(&mut self, url: &str) -> Result<(), String>;
    fn get(&mut self, url: &str, wait: Duration) -> Result<transport::Answer, transport::Unheard>;
    fn now(&mut self) -> Instant;
    fn pause(&mut self, pause: Duration);
}

struct LiveTries;

/// Whether `url` is reached through the LAN broker: on macOS, a host [`aterm_uds::lan`] names
/// local network (a `.local` or single-label name, or a local-network address).
fn needs_broker(url: &str) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let Some(uri) = aterm_http::Uri::parse(url) else {
        return false;
    };
    let host = uri.host().trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => aterm_uds::lan::is_local_network(&ip),
        Err(_) => aterm_uds::lan::is_local_name(host),
    }
}

impl Tries for LiveTries {
    fn broker(&mut self, url: &str) -> Result<(), String> {
        if !needs_broker(url) {
            return Ok(());
        }
        match aterm_uds::lan::broker::hello() {
            Err(aterm_uds::lan::LanError::Unavailable(why)) => Err(why.to_string()),
            // A broker that answered, or an exchange that failed otherwise: the try says what
            // the vault answers.
            Ok(_) | Err(aterm_uds::lan::LanError::Io(_)) => Ok(()),
        }
    }

    fn get(&mut self, url: &str, wait: Duration) -> Result<transport::Answer, transport::Unheard> {
        if crate::is_alab_default_url(url)
            && alab_signer_built_in()
            && let Ok(ProbeOutcome::Found(probed)) = probe_default(PROBE_DEADLINE.min(wait))
        {
            return Ok(transport::Answer::Fetched(Fetched::Body(probed.envelope)));
        }
        transport::look_bounded(url, wait, Some(transport::LOOK_CONNECT))
    }

    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// Probe whether a vault answers at `source` within `wait` without modifying local state. A URL
/// on the local network goes through this Mac's LAN broker: while it is unavailable the look
/// waits for its socket to answer, asking again every 2 s for up to `patience`, and `held` hears
/// the broker's words once as the wait starts; a broker still unavailable then is
/// [`Look::Unreached`] in its words. The look's one try runs as soon as the broker answers.
pub fn look(source: &Source, wait: Duration, patience: Duration, held: &dyn Fn(&str)) -> Look {
    look_with(source, wait, patience, held, &mut LiveTries)
}

fn look_with(
    source: &Source,
    wait: Duration,
    patience: Duration,
    held: &dyn Fn(&str),
    tries: &mut dyn Tries,
) -> Look {
    let shown = match source {
        Source::Url(url) => transport::describe(url),
        Source::Path(path) => path.display().to_string(),
    };
    let fetched = match source {
        Source::Url(url) => {
            let start = tries.now();
            let mut told = false;
            while let Err(why) = tries.broker(url) {
                if tries
                    .now()
                    .saturating_duration_since(start)
                    .saturating_add(LOOK_PAUSE)
                    >= patience
                {
                    return Look::Unreached(why);
                }
                if !told {
                    told = true;
                    held(&why);
                }
                tries.pause(LOOK_PAUSE);
            }
            match tries.get(url, wait) {
                Ok(transport::Answer::Fetched(fetched)) => fetched,
                Ok(transport::Answer::Foreign(why)) => return Look::NoVault(why),
                Err(unheard) => return Look::Unreached(unheard.why.0),
            }
        }
        Source::Path(_) => match transport::fetch(source, None, wait) {
            Ok(fetched) => fetched,
            Err(error) => return Look::Unreached(error.to_string()),
        },
    };
    match fetched {
        Fetched::Body(bytes) if is_envelope(&bytes) || matches!(source, Source::Path(_)) => {
            Look::Answers
        }
        Fetched::Body(_) => Look::NoVault(format!("{shown} does not serve an aterm secrets vault")),
        Fetched::Missing => Look::NoVault(format!("{shown}: no vault there")),
        Fetched::NoNewer(_) => Look::NoVault("the source answered with no vault".into()),
    }
}

/// Copy `source.toml` and `state.toml` under the state lock into a newly created `into` (`0700`).
/// # Errors
/// The existing state cannot be locked or read, `into` already exists, or the new directory
/// or a copy cannot be written. A partial new copy is retained and is never reused.
pub fn keep_config(paths: &Paths, into: &Path) -> crate::Result<()> {
    keep_config_with(paths, into, ExpectedConfig::Any)
}

/// Keep only the configuration the caller read, refusing a different or unreadable one.
/// # Errors
/// As [`keep_config`], or the current configuration differs from `expected`.
pub fn keep_config_if_unchanged(
    paths: &Paths,
    into: &Path,
    expected: Option<&Config>,
) -> crate::Result<()> {
    keep_config_with(paths, into, ExpectedConfig::Read(expected))
}

fn keep_config_with(paths: &Paths, into: &Path, expected: ExpectedConfig<'_>) -> crate::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let files = match StateLock::observe(paths)? {
        Some(_lock) => {
            expected.check(paths)?;
            let mut files = Vec::new();
            // Each file under the bound its own reader keeps ([`config`], [`load_state`]).
            for (from, name, max) in [
                (paths.source_file(), "source.toml", 4 << 20),
                (paths.state_file(), "state.toml", MAX_STATE),
            ] {
                if let Some(bytes) = fsx::read_private(&from, max)? {
                    files.push((name, bytes));
                }
            }
            files
        }
        None if fsx::kind(&paths.dir)? == fsx::Kind::Missing => {
            expected.check(paths)?;
            Vec::new()
        }
        None => {
            return Err(Error::new(
                "the vault state lock is busy or unavailable; try again",
            ));
        }
    };
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(into)
        .map_err(|error| Error::new(format!("{}: {error}", into.display())))?;
    for (name, bytes) in files {
        fsx::write_atomic(&into.join(name), bytes.as_slice(), 0o600)?;
    }
    Ok(())
}

fn apply_entries(paths: &Paths, state: &mut State, entries: &[Entry]) -> crate::Result<Report> {
    let mut resumed = std::mem::take(&mut state.pending);
    // A kept name whose entry this pass drops is the Mac's own from here: held back from the
    // manifest (and from a cut pass's pending) this pass is handed, so it is not taken back.
    let names: BTreeSet<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    let released: Vec<String> = state
        .kept
        .iter()
        .filter(|name| !names.contains(name.as_str()))
        .cloned()
        .collect();
    let mut held = state.manifest.clone();
    for name in &released {
        held.files.remove(name);
        resumed.remove(name);
    }
    let mut pended = materialize::Pending::new();
    let applied = materialize::apply_resuming(
        &paths.home,
        entries,
        &held,
        &resumed,
        now(),
        &mut |held, pending| {
            pended.clone_from(pending);
            if pending.is_empty() {
                return Ok(());
            }
            let mut ahead = load_state(paths)?;
            ahead.manifest = held.clone();
            ahead.pending = pending.clone();
            ahead.materialization_selection = None;
            save_state(paths, &ahead)
        },
    );
    match applied {
        Ok((manifest, report)) => {
            state.pending = landed(&paths.home, pended, &report.failed);
            state.own = Some(own_entries(entries, &manifest, &report));
            state.manifest = manifest;
            state.materialization_selection = Some(materialization_selection::Meaning::of(entries));
            state.kept.retain(|name| names.contains(name.as_str()));
            Ok(report)
        }
        Err(error) => {
            state.pending = resumed;
            Err(error)
        }
    }
}

/// The digests a pass pended (`pended`) of the entries it `failed` whose file holds that value
/// all the same: a write that failed after its rename (the directory's flush failing). Kept
/// pending, the file counts as the vault's next pass, never adopted as this Mac's own. A failed
/// write whose file does not hold it wrote nothing, so a file a person writes with that value
/// later stays theirs.
fn landed(
    home: &Path,
    mut pended: materialize::Pending,
    failed: &[(String, String)],
) -> materialize::Pending {
    pended.retain(|name, digest| {
        failed.iter().any(|(failed, _)| failed == name)
            && materialize::holds_digest(home, name, digest)
    });
    pended
}

/// The entries an apply left this Mac's own that it would write once their file is gone
/// ([`State::own`]): plain entries adopted, and ` if-absent` entries left whose anchors, all
/// the way along, the vault wrote this pass (an anchor of this Mac's own, or one not looked
/// at, leaves a gone file unfilled, so a file anchored to one is never named: it would be a
/// gap no pass fills).
fn own_entries(entries: &[Entry], manifest: &Manifest, report: &Report) -> BTreeSet<String> {
    let by_name: BTreeMap<&str, &Entry> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    let vaults = |name: &str| {
        manifest.files.contains_key(name) && !report.left.iter().any(|left| left == name)
    };
    let anchored_to_vaults = |entry: &Entry| {
        let mut seen = BTreeSet::new();
        let mut anchor = entry.anchor.as_deref();
        while let Some(name) = anchor {
            if !seen.insert(name) || !vaults(name) {
                return false;
            }
            anchor = by_name
                .get(name)
                .and_then(|anchored| anchored.anchor.as_deref());
        }
        true
    };
    entries
        .iter()
        .filter(|entry| !manifest.files.contains_key(&entry.name))
        .filter(|entry| {
            if entry.if_absent {
                report.left.contains(&entry.name) && anchored_to_vaults(entry)
            } else {
                report.unchanged.contains(&entry.name)
            }
        })
        .map(|entry| entry.name.clone())
        .collect()
}

fn names_of(report: &Report) -> Vec<String> {
    let mut names: Vec<String> = report
        .written
        .iter()
        .chain(report.unchanged.iter())
        .cloned()
        .collect();
    names.sort();
    names
}

fn floor(paths: &Paths, state: &State, name: &str, config: &Config) -> Option<Floor> {
    let pin = config.signer?;
    let cached = chain_cache::held(&paths.cache_file()).ok().flatten();
    let verified = |parsed: &chain_cache::Parsed| {
        envelope::verify_for_reader(parsed.bytes(), name, &pin, &config.followed, None)
            .ok()
            .map(|(envelope, _, _)| envelope)
    };
    if state.membership != Stored::None {
        let Some(digest) = state.digest else {
            return Some(Floor::below(&verified(cached.as_deref()?)?));
        };
        let held = cached
            .as_deref()
            .and_then(|parsed| parsed.envelope().ok())
            .filter(|cached| cached.digest() == digest);
        return Some(match held {
            Some(held) => Floor::of(held),
            None => Floor::version(state.seq, digest),
        });
    }
    Some(Floor::of(&verified(cached.as_deref()?)?))
}

fn pin_gives_way(config: &Config) -> bool {
    config
        .signer
        .is_some_and(|pin| Retired::compiled().gives_way(&pin, &config.followed))
}

fn cached_chain(paths: &Paths, pin: &[u8; 32]) -> Vec<[u8; 32]> {
    chain_cache::held(&paths.cache_file())
        .ok()
        .flatten()
        .map_or_else(Vec::new, |parsed| parsed.chain(pin))
}

fn chain_keys(envelope: &Envelope) -> Vec<[u8; 32]> {
    envelope.handoff.iter().map(|(key, _)| *key).collect()
}

fn files_intact(paths: &Paths, state: &State) -> bool {
    local_files_intact(paths, state) && materialization_selection_current(paths, state)
}

/// Physical ownership and cache checks, separate from the current selected meaning.
fn local_files_intact(paths: &Paths, state: &State) -> bool {
    !state.failed
        && state.pending.is_empty()
        && (cache_intact(paths, state)
            || config(paths)
                .ok()
                .flatten()
                .is_some_and(|config| log_only_intact(paths, &config, state)))
        && unsettled(paths, state).is_empty()
}

fn materialization_selection_current(paths: &Paths, state: &State) -> bool {
    state.materialization_selection.is_some_and(|applied| {
        materialization_selection::current(paths, state)
            .is_ok_and(|current| current.is_some_and(|current| current.meaning == applied))
    })
}

/// The files a pass would write again at the version `state` holds: [`changed_files`] and
/// [`gaps`].
fn unsettled(paths: &Paths, state: &State) -> BTreeSet<String> {
    let mut unsettled = changed_files(paths, state);
    unsettled.extend(gaps(paths, state));
    unsettled
}

/// The entries [`State::own`] names whose file is gone since, where a pass would write it
/// ([`materialize::is_gap`], never a reserved name or an AI CLI's login, which have no file the
/// vault writes): the vault fills a gap at the version it holds.
fn gaps(paths: &Paths, state: &State) -> BTreeSet<String> {
    state
        .own
        .iter()
        .flatten()
        .filter(|name| materialize::is_gap(&paths.home, name))
        .cloned()
        .collect()
}

/// The files `state`'s manifest names that are not as the vault wrote them (changed, missing or
/// unreadable); one behind a link is left out, as the vault never writes through one, and so is
/// a name set apart that a build that did not refuse it wrote (an AI CLI's login, a home path
/// aterm does not name), the Mac's own file now ([`materialize::Report::refused`]).
fn changed_files(paths: &Paths, state: &State) -> BTreeSet<String> {
    state
        .manifest
        .files
        .iter()
        .filter(|(name, digest)| {
            !materialize::behind_a_link(&paths.home, name)
                && !matches!(
                    crate::entry_class::of(name),
                    crate::entry_class::EntryClass::SetApart(_)
                )
                && materialize::target_path(&paths.home, name)
                    .ok()
                    .and_then(|path| fsx::read_private(&path, text::MAX_VALUE).ok().flatten())
                    .is_none_or(|bytes| materialize::digest_hex(bytes.as_slice()) != **digest)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

fn cache_intact(paths: &Paths, state: &State) -> bool {
    config(paths).ok().flatten().is_some_and(|config| {
        config.signer.is_none()
            || (state.membership == Stored::In
                && verified_cached_envelope(paths, &config, state).is_some_and(|bytes| {
                    envelope::parse(&bytes).is_ok_and(|envelope| {
                        log_entries::trusted_seeded_log(paths, &config, &envelope).is_ok()
                    })
                }))
    })
}

fn place(source: &Source) -> String {
    match source {
        Source::Url(url) => transport::describe(url),
        Source::Path(path) => path.display().to_string(),
    }
}

fn held_copy(paths: &Paths, config: &Config, state: &State) -> Option<Vec<u8>> {
    read_held_copy(paths, config, state).ok().flatten()
}

/// Read the held envelope for an authorization decision, preserving an I/O failure as an error
/// rather than mistaking a temporarily unreadable copy for an authoritative absence.
fn read_held_copy(paths: &Paths, config: &Config, state: &State) -> crate::Result<Option<Vec<u8>>> {
    if state.membership != Stored::In {
        return Ok(None);
    }
    read_verified_cached_envelope(paths, config, state)
}

/// Publication additionally requires the applied log and no received local denial.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedPublication",
        action = "Revoke",
        reason = "An authenticated removal arrives before materialization"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCachedPublication",
        action = "Publish",
        project = "aterm_secrets::client::peer_selection_tests::cached_publication_transition"
    )
)]
fn publishable_copy(paths: &Paths, config: &Config, state: &State) -> Option<Vec<u8>> {
    let copy = held_copy(paths, config, state)?;
    let envelope = envelope::parse(&copy).ok()?;
    let seeded = log_entries::trusted_seeded_log(paths, config, &envelope).ok()?;
    if state.records_digest.is_some() && seeded.is_none() {
        return None;
    }
    if let Some((log, _)) = seeded {
        let identity = read_identity(paths).ok().flatten()?;
        if log.removes(&identity.recipient())
            || log_entries::recipient_denied_by_roster_or_log(None, &log, &identity.recipient())
            || (log_entries::log_base(Some(&log), &envelope, &identity).ok()?
                != log_entries::LogBase::Behind
                && log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &identity.recipient(),
                ))
        {
            return None;
        }
    }
    Some(copy)
}

/// The exact authenticated cached envelope for local reading, even while admission is pending.
/// [`publishable_copy`] additionally admits one for publication back to a server.
fn verified_cached_envelope(paths: &Paths, config: &Config, state: &State) -> Option<Vec<u8>> {
    read_verified_cached_envelope(paths, config, state)
        .ok()
        .flatten()
}

fn read_verified_cached_envelope(
    paths: &Paths,
    config: &Config,
    state: &State,
) -> crate::Result<Option<Vec<u8>>> {
    Ok(Reads::new(paths)
        .verified_under(config, state)?
        .map(|envelope| envelope.raw().to_vec()))
}

fn missing(config: &Config) -> Error {
    Error::new(format!(
        "the source holds no vault named {}; a Mac in the vault puts its copy back",
        config.name
    ))
}

/// Whether a pass left one of the vault's own entries unapplied ([`State::failed`], which
/// keeps every read of the applied copy back). A reserved name's file an older build wrote whose
/// removal failed is reported failed and kept in the manifest, so the next pass tries again, but
/// every entry the vault holds is in place: it keeps no read back (the fleet code, the Commons
/// control).
fn apply_failed(report: &Report) -> bool {
    report
        .failed
        .iter()
        .any(|(name, _)| !crate::reserved_name(name))
}

const fn quiet(report: &Report) -> bool {
    report.written.is_empty()
        && report.removed.is_empty()
        && report.moved_aside.is_empty()
        && report.failed.is_empty()
}

/// The configured source, or why there is none to sync.
fn configured(paths: &Paths) -> crate::Result<Config> {
    config(paths)?.ok_or_else(|| {
        Error::new("no vault configured: run aterm secrets join <url> (or aterm init)")
    })
}

/// Fetch, verify, decrypt, and materialize once, optionally long-polling for `wait`.
pub fn sync(paths: &Paths, wait: Option<Duration>) -> crate::Result<SyncOutcome> {
    sync_repairing(paths, wait, None)
}

/// [`sync`] of a subscriber's pass: a source that fails or holds nothing newer gives way to the
/// highest verified seq among the vault servers `found` (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// F5), the source still its first choice. It keeps in `said` what the source itself said.
fn sync_found(
    paths: &Paths,
    wait: Option<Duration>,
    found: Found<'_>,
    said: &std::cell::Cell<SourceSaid>,
) -> crate::Result<SyncOutcome> {
    sync_repairing_inner(paths, wait, None, &[], true, Some((found, said)), None)
}

/// [`sync`], also probing `candidates` (`DEFAULT_VAULT_URLS` and `_alab-vault._tcp` peers) when the
/// configured primary URL is unreachable, empty, or behind a peer's verified `seq`
/// (`DESIGN-alab-swarm` §4.1, §8.3, §10: a frozen or offline server cannot freeze or strand a
/// client). Every candidate response is signature- and floor-verified against `paths`'s pinned
/// signer before it can win.
///
/// # Errors
/// Vault configuration is missing, neither the primary nor any verified candidate answers, or
/// verification/materialization fails.
pub fn sync_with_candidates(
    paths: &Paths,
    wait: Option<Duration>,
    candidates: &[String],
) -> crate::Result<SyncOutcome> {
    sync_repairing_inner(paths, wait, None, candidates, true, None, None)
}

/// [`sync_with_candidates`] with no wait and no mirror, every records post of the pass ending by
/// `posts_by` where one is given ([`sync_repairing_inner`]): the pass a publish runs once its
/// version landed, whose posts share the deadline of the publish's own.
fn sync_without_reconcile(
    paths: &Paths,
    candidates: &[String],
    posts_by: Option<Instant>,
) -> crate::Result<SyncOutcome> {
    sync_repairing_inner(paths, None, None, candidates, false, None, posts_by)
}

/// The vault `source.toml` names and this Mac's empty record store for it
/// ([`records_store_for`]).
fn records_store(paths: &Paths) -> crate::Result<(String, crate::membership_log::Store)> {
    let reads = Reads::new(paths);
    records_store_selected(&reads, reads.config()?)
}

fn records_store_selected(
    reads: &Reads<'_>,
    config: Option<&Config>,
) -> crate::Result<(String, crate::membership_log::Store)> {
    let (name, store) = records_store_for(config)?;
    if let Some(config) = config
        && let Some(network) = retained_history::pinned_network(reads, config)?
    {
        if store
            .pinned_network()
            .is_some_and(|compiled| compiled != network)
        {
            return Err(Error::new(
                "compiled and retained membership roots disagree",
            ));
        }
        return Ok((
            name,
            crate::membership_log::Store::with_compiled(Some(network)),
        ));
    }
    let store = match config {
        Some(config) => store.under(&reads.lineage(config)),
        _ => store,
    };
    Ok((name, store))
}

/// The record store name `config`'s vault keeps and an empty store for it
/// ([`crate::membership_log::Store::for_vault`]): ALab's (whose genesis the build pins) for a
/// vault the ALab key signs, the vault's own name otherwise, and none for a vault that only calls
/// itself ALab or for no config. ALab's also takes a genesis signed by the key `config` pins or by
/// any key it reached that pin through ([`crate::membership_log::Store::also_under`]): a Mac a
/// release behind a rotation names only the old key, and a genesis signed after the rotation is
/// the new key's.
fn records_store_for(
    config: Option<&Config>,
) -> crate::Result<(String, crate::membership_log::Store)> {
    Ok(match config {
        Some(cfg) if alab_vault(cfg) => {
            let line: Vec<[u8; 32]> = cfg
                .signer
                .into_iter()
                .chain(cfg.followed.iter().copied())
                .collect();
            let store =
                crate::membership_log::Store::for_vault(crate::ALAB_VAULT)?.also_under(&line);
            (crate::ALAB_VAULT.to_owned(), store)
        }
        Some(cfg) if cfg.name != crate::ALAB_VAULT => {
            let store = crate::membership_log::Store::for_vault(&cfg.name)?;
            (cfg.name.clone(), store)
        }
        _ => (String::new(), crate::membership_log::Store::for_vault("")?),
    })
}

/// Load this machine's persisted membership-record [`crate::membership_log::Store`] (empty
/// before its first records file). A pinned layout with missing data is an error. A file this
/// build refuses as a whole keeps the records it still takes
/// ([`crate::membership_log::Store::recover`]), and the next write
/// replaces it, so one refused record never stops this Mac taking records. A file past
/// [`crate::membership_log::MAX_RECORDS_STREAM`] (a build that bounded its store by count alone
/// could write one) keeps the records its first that many bytes hold. These are the raw records,
/// a missing file read as none: an authority decision reads [`load_acknowledged_records`].
///
/// # Errors
/// `source.toml` cannot be read or parsed, the records layout is unavailable, the data cannot
/// be read, or this build's ALab genesis digest is malformed.
pub fn load_records(paths: &Paths) -> crate::Result<crate::membership_log::Store> {
    let (vault, store) = records_store(paths)?;
    records_cache::read(paths, vault, store)
}

/// This Mac's membership store as an authority decision reads it: [`load_records`], projected
/// onto the history this Mac acknowledged when a vault is configured. Where that history holds
/// the store's genesis, the store reads exactly the records it acknowledged, none a writer made
/// durable since and no writer has acknowledged yet. Every judgment of who belongs grants from
/// the store read here, admission ([`load_acknowledged_and_ahead`]) included; [`load_records`]
/// alone serves repair, replication and what this Mac states it holds, which take every record
/// it holds. With no vault configured the store is [`load_records`]' own.
///
/// # Errors
/// [`load_records`]' errors; the acknowledged history cannot be read; the store's genesis is
/// one that history holds foreign or retired, or competes with the one it holds for this vault;
/// and, of kind [`crate::ErrorKind::MembershipHistoryUnavailable`], a store that omits history
/// this Mac acknowledged (the records file missing while that history holds this vault's
/// genesis, as the flat layout's file gone reads as an empty store, or a snapshot behind it put
/// back), a log this Mac has not acknowledged once that history was established, or, before
/// then, a log other than the one this Mac last applied.
pub fn load_acknowledged_records(paths: &Paths) -> crate::Result<crate::membership_log::Store> {
    Ok(acknowledged_read(paths, false)?.acknowledged)
}

/// [`load_acknowledged_and_ahead`]'s read of this Mac's membership store.
pub struct AcknowledgedRecords {
    /// The store as [`load_acknowledged_records`] reads it, which an authority decision grants
    /// from.
    pub acknowledged: crate::membership_log::Store,
    /// The raw store ([`load_records`]) of the same read, where its log holds records the
    /// acknowledged one does not: a writer made them durable and no writer has acknowledged them
    /// yet (one cut short between the two, or whose acknowledgement failed). It holds every
    /// record the acknowledged store holds, as the read refuses a store that omits one, and its
    /// records verify as every record this Mac keeps does. `None` where the two are one.
    pub ahead: Option<crate::membership_log::Store>,
}

/// [`load_acknowledged_records`], with the raw records that read projected where they hold more
/// ([`AcknowledgedRecords::ahead`]): what admission reads, which grants only from the
/// acknowledged store and holds out every key either store holds out, so a removal or a rotation
/// this Mac made durable stands at once, before any writer acknowledges it.
///
/// # Errors
/// As [`load_acknowledged_records`].
pub fn load_acknowledged_and_ahead(paths: &Paths) -> crate::Result<AcknowledgedRecords> {
    acknowledged_read(paths, true)
}

/// One read of the store, projected under the configured vault; with `ahead`, the raw store kept
/// where the projection dropped records.
fn acknowledged_read(paths: &Paths, ahead: bool) -> crate::Result<AcknowledgedRecords> {
    acknowledged_under(&Reads::new(paths), ahead)
}

fn acknowledged_under(reads: &Reads<'_>, ahead: bool) -> crate::Result<AcknowledgedRecords> {
    let config = reads.config()?;
    let (vault, store) = records_store_selected(reads, config)?;
    let raw = records_cache::read(reads.paths(), vault, store)?;
    let Some(config) = config else {
        return Ok(AcknowledgedRecords {
            acknowledged: raw,
            ahead: None,
        });
    };
    let kept = ahead.then(|| raw.clone());
    let acknowledged = retained_history::project_store(reads, config, raw)?;
    let heads =
        |store: &crate::membership_log::Store| store.log().map(crate::membership_log::Log::heads);
    let ahead = kept.filter(|raw| heads(raw) != heads(&acknowledged));
    Ok(AcknowledgedRecords {
        acknowledged,
        ahead,
    })
}

/// An envelope-verified replacement is evaluated under its prospective configuration,
/// before that configuration can replace the currently materialized source on disk.
fn load_records_for(
    reads: &Reads<'_>,
    config: &Config,
) -> crate::Result<crate::membership_log::Store> {
    let (vault, store) = records_store_selected(reads, Some(config))?;
    records_cache::read(reads.paths(), vault, store)
}

/// Verify and merge `records` into `paths.records_file()` under the client state lock, returning
/// how many new records entered the store.
///
/// # Errors
/// `records` fails verification or `paths.records_file()` cannot be read or written.
pub fn save_records(paths: &Paths, records: &[u8]) -> crate::Result<usize> {
    let _lock = StateLock::take(paths)?;
    save_records_unlocked(paths, records)
}

pub(crate) fn save_records_unlocked(paths: &Paths, records: &[u8]) -> crate::Result<usize> {
    let mut store = load_records(paths)?;
    let added = store.ingest(records)?;
    if added > 0 {
        write_records(paths, &store)?;
    }
    Ok(added)
}

/// Buffer small signed records without collecting the complete stream. Flush before the
/// atomic writer can make the temporary durable or publish it.
fn write_record_file(
    path: &Path,
    produce: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> crate::Result<()> {
    fsx::write_atomic_with(path, 0o600, |writer| write_record_buffered(writer, produce))
}

fn write_record_buffered(
    writer: &mut dyn std::io::Write,
    produce: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut buffered = std::io::BufWriter::with_capacity(64 * 1024, writer);
    produce(&mut buffered)?;
    buffered.flush()
}

/// Make the records file hold exactly `store`, under the records writer lock
/// ([`records_file_for_write`]). Where the file there is one a read of this process took whole
/// (every byte of it a complete record this build holds, none waiting on a parent: no torn
/// tail) and `store` is those records with complete ones after
/// ([`records_cache::held_on_file`]), only the new records go on its end
/// ([`fsx::append_at`]: still the same device, inode and length, then made durable), in one
/// write. Every other store replaces the file whole by a temporary file and a rename
/// ([`write_record_file`]): one with a record waiting on a parent on file or to write, one
/// that drops or reorders what the file holds, a file no read of this process stamped (another
/// writer's since, a layout just prepared), and an append that failed, whose torn tail the
/// replacement removes.
///
/// A whole replacement of a member's store costs its whole size at each record that arrives
/// (4.8 MB on a lab Mac, twice in 30 s, 2026-11-11); an append costs the records. What an
/// append gives up is the reader's view while it lands: a reader that opens the file between
/// the write's start and its end can read a record cut short, which it reads as the store
/// without the records from that one on ([`crate::membership_log::Store::recover`]), every
/// record it takes verified and after its parents, as a read just before the append would
/// answer; and a reader whose read brackets the append fails its check and reads again
/// ([`records_cache`]). A crash or power loss mid-append leaves the same: the records before
/// the cut, read as such, and the next writer, finding bytes on file that are not exactly
/// records it holds, replaces the file whole.
fn persist_records(paths: &Paths, store: &crate::membership_log::Store) -> crate::Result<()> {
    let target = records_file_for_write(paths, store)?;
    if let Some(stamp) = fsx::stamp_if_present(&target)?
        && let Some(held) = records_cache::held_on_file(stamp, store)
        && let Ok(length) = store.held_len_from(held)
    {
        if length == 0 {
            // The file already holds exactly this store.
            return Ok(());
        }
        let mut suffix = Vec::with_capacity(length);
        store.write_held_from(held, &mut suffix)?;
        if append_records(&target, &stamp, &suffix).is_ok() {
            return Ok(());
        }
    }
    write_record_file(&target, |writer| store.write_encoded(writer))
}

/// What a test's disk does to the next appends of this thread ([`append_records`]).
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordAppends {
    /// Appends as the disk does.
    Land,
    /// Cuts each append one byte short and fails it: the part stays on the end of the file.
    Tear,
}

#[cfg(test)]
thread_local! {
    pub(crate) static RECORD_APPENDS: std::cell::Cell<RecordAppends> =
        const { std::cell::Cell::new(RecordAppends::Land) };
}

/// [`fsx::append_at`] of new records onto a member's records file ([`persist_records`]).
fn append_records(path: &Path, stamp: &fsx::FileStamp, suffix: &[u8]) -> crate::Result<()> {
    #[cfg(test)]
    if RECORD_APPENDS.with(std::cell::Cell::get) == RecordAppends::Tear {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
        file.write_all(&suffix[..suffix.len() - 1])?;
        return Err(Error::new("the test's disk cut the append short"));
    }
    fsx::append_at(path, stamp, suffix).map(|_| ())
}

/// Write `store` to `paths.records_file()` ([`persist_records`]: its new records on the end of
/// the file where the file holds exactly the rest, the file whole otherwise): where it holds
/// more records than a build before the fence takes ([`crate::crossing::past_legacy`]), the
/// crossed mark is stored first, so no records file passes that bound without it.
///
/// # Errors
/// The mark cannot be stored (the file is then left as it was), or the file cannot be written.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsCapacity",
        action = "MacTakes",
        project = "aterm_secrets::client::record_persistence_tests::records_capacity_mac"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsCapacity",
        action = "MacTakeFails",
        project = "aterm_secrets::client::record_persistence_tests::records_capacity_mac"
    )
)]
fn write_records(paths: &Paths, store: &crate::membership_log::Store) -> crate::Result<()> {
    retained_history::validate_write(paths, store)?;
    let crosses = crate::crossing::past_legacy(
        store
            .log()
            .map_or(0, crate::membership_log::Log::held_count),
    );
    let mark = crossing_mark(paths);
    // MUTANT (`crate::crossing::Faults::mark_after_take`): the file first, then the mark.
    #[cfg(test)]
    if crosses && crate::crossing::faults(mark.path()).mark_after_take {
        persist_records(paths, store)?;
        records_written(paths, store);
        return mark.store();
    }
    if crosses {
        mark.store()?;
    }
    persist_records(paths, store)?;
    records_written(paths, store);
    retained_history::acknowledge(paths, store)
}

/// The records file holds `store` durably: hand it to the process's hook
/// ([`crate::membership_log::store::on_written`]).
fn records_written(paths: &Paths, store: &crate::membership_log::Store) {
    crate::membership_log::store::written(&crate::membership_log::store::Written {
        paths_dir: &paths.dir,
        store,
    });
}

/// Ingest `records` the mesh delivered into `paths.records_file()` under the client state lock
/// ([`crate::membership_log::Store::ingest_classified`]), writing the file only where a record
/// entered the store (held or waiting), and say what became of each one.
///
/// # Errors
/// The state lock cannot be taken, or `paths.records_file()` cannot be read or written.
pub fn ingest_mesh_records(
    paths: &Paths,
    records: Vec<crate::membership_log::Record>,
) -> crate::Result<
    Vec<(
        crate::membership_log::RecordId,
        crate::membership_log::store::MemberVerdict,
    )>,
> {
    let _lock = StateLock::take(paths)?;
    let mut store = load_records(paths)?;
    let before = store.inventory();
    let verdicts = store.ingest_classified(records);
    if store.inventory() != before {
        write_records(paths, &store)?;
    }
    Ok(verdicts)
}

/// Isolate the old regular-file path before publishing custody that an older reader cannot hold.
fn records_file_for_write(
    paths: &Paths,
    store: &crate::membership_log::Store,
) -> crate::Result<crate::records_storage::WriteTarget> {
    crate::records_storage::write_path(
        &paths.records_file(),
        crate::crossing::past_legacy(store.len()) || records_marked_crossed(paths)?,
    )
}

/// This Mac's crossed mark ([`Paths::crossed_file`]).
pub(crate) fn crossing_mark(paths: &Paths) -> crate::crossing::Mark {
    crate::crossing::Mark::at(paths.crossed_file())
}

/// Whether this Mac has the durable, one-way crossed mark, without opening its records file.
/// A caller with a verified snapshot must also preserve that snapshot's crossed bound.
///
/// # Errors
/// The mark's path cannot be read.
pub fn records_marked_crossed(paths: &Paths) -> crate::Result<bool> {
    crossing_mark(paths).stored()
}

/// What this Mac states of its records ([`crate::crossing::RecordsBound`]), its log as `store`
/// holds it.
///
/// # Errors
/// The mark's path cannot be read.
pub fn records_bound(
    paths: &Paths,
    store: &crate::membership_log::Store,
) -> crate::Result<crate::crossing::RecordsBound> {
    crate::crossing::stated(&crossing_mark(paths), store)
}

#[cfg(test)]
#[path = "client/mesh_records_tests.rs"]
mod mesh_records_tests;
#[cfg(test)]
#[path = "client/record_persistence_tests.rs"]
mod record_persistence_tests;
#[cfg(test)]
#[path = "client/records_append_tests.rs"]
mod records_append_tests;
#[cfg(test)]
#[path = "client/state_writes_tests.rs"]
mod state_writes_tests;

#[cfg(test)]
#[path = "client/retained_history_tests.rs"]
mod retained_history_tests;

mod retained_frontier;
mod retained_history;

/// Retain verified membership records offered over Commons control repair for `(network,
/// authority)`, returning one durable acknowledgement per wire.
/// The legacy database protocols carry no crossed-state metadata, so a crossed local store
/// refuses them before acknowledging or retaining records.
///
/// # Errors
/// Local custody cannot be read or made durable, or the local history crossed the legacy bound.
pub fn retain_records_for_commons(
    paths: &Paths,
    network: [u8; 16],
    authority: [u8; 32],
    wires: &[Vec<u8>],
) -> crate::Result<Vec<bool>> {
    retain_records_for_commons_with(paths, network, authority, wires, || {})
}

fn require_legacy_commons_records(
    paths: &Paths,
    store: &crate::membership_log::Store,
) -> crate::Result<()> {
    if records_bound(paths, store)?.crossed {
        return Err(Error::of_kind(
            crate::ErrorKind::Refused,
            "crossed membership history requires a newer Commons database protocol",
        ));
    }
    Ok(())
}

/// Retain Commons membership records from a peer whose bound was negotiated before its records.
/// Acknowledgements name durable, complete records; they do not assert that either peer is fresh.
/// A crossed source's new complete records make this Mac crossed before they are written.
/// Duplicate-only and waiting-only batches do not cross it, and waiting-only crossed batches
/// are not retained. The caller must recheck its current bound before sending further records.
///
/// # Errors
/// Local custody cannot be read or made durable, or either capacity cannot represent the
/// other peer's possible history, or the batch exceeds the local record or byte bound.
pub fn retain_bounded_records_for_commons(
    paths: &Paths,
    network: [u8; 16],
    authority: [u8; 32],
    source: crate::crossing::RecordsBound,
    wires: &[Vec<u8>],
) -> crate::Result<Vec<bool>> {
    retain_commons_records_with(paths, network, authority, wires, Some(source), || {})
}

fn require_commons_source(
    paths: &Paths,
    store: &crate::membership_log::Store,
    source: Option<crate::crossing::RecordsBound>,
) -> crate::Result<()> {
    if let Some(source) = source {
        if !records_bound(paths, store)?.can_exchange(source) {
            return Err(Error::of_kind(
                crate::ErrorKind::Refused,
                "Commons membership record capacities are incompatible",
            ));
        }
        Ok(())
    } else {
        require_legacy_commons_records(paths, store)
    }
}

// The callback lets a test interleave a writer after the initial read and before the lock.
fn retain_records_for_commons_with(
    paths: &Paths,
    network: [u8; 16],
    authority: [u8; 32],
    wires: &[Vec<u8>],
    before_lock: impl FnOnce(),
) -> crate::Result<Vec<bool>> {
    retain_commons_records_with(paths, network, authority, wires, None, before_lock)
}

fn retain_commons_records_with(
    paths: &Paths,
    network: [u8; 16],
    authority: [u8; 32],
    wires: &[Vec<u8>],
    source: Option<crate::crossing::RecordsBound>,
    before_lock: impl FnOnce(),
) -> crate::Result<Vec<bool>> {
    let expected = crate::membership_log::Root { network, authority };
    let prefetched = if source.is_some() {
        let store = load_records(paths)?;
        require_commons_source(paths, &store, source)?;
        let bytes = wires
            .iter()
            .try_fold(0usize, |n, wire| n.checked_add(wire.len()));
        if wires.len() > store.budgets().held
            || bytes.is_none_or(|n| n > crate::membership_log::MAX_RECORDS_STREAM)
        {
            return Err(Error::of_kind(
                crate::ErrorKind::Refused,
                "Commons membership batch exceeds the record or byte bound",
            ));
        }
        Some(store)
    } else {
        None
    };
    let decoded: Vec<Option<crate::membership_log::Record>> = wires
        .iter()
        .map(|wire| {
            let record = crate::membership_log::Record::decode(wire).ok()?;
            if let crate::membership_log::Body::Genesis { root, .. } = record.body()
                && (*root != expected
                    || !log_entries::genesis_record_matches_local_lineage(paths, &record)
                        .unwrap_or(false))
            {
                return None;
            }
            Some(record)
        })
        .collect();
    if decoded.iter().all(Option::is_none) {
        return Ok(vec![false; wires.len()]);
    }
    let known = match prefetched {
        Some(store) => store,
        None => load_records(paths)?,
    };
    require_commons_source(paths, &known, source)?;
    if let Some(log) = known.log() {
        let crate::membership_log::Body::Genesis { root, .. } = log.genesis().body() else {
            return Ok(vec![false; wires.len()]);
        };
        if *root != expected || !log_entries::commons_log_matches_local_lineage(paths, log)? {
            return Ok(vec![false; wires.len()]);
        }
    }
    let held: Vec<bool> = decoded
        .iter()
        .map(|r| {
            r.as_ref()
                .is_some_and(|rec| known.complete_bytes(rec.id()).is_some())
        })
        .collect();
    if decoded.iter().zip(&held).all(|(r, &h)| r.is_none() || h) {
        if held.iter().any(|&h| h) {
            aterm_durable::durable_dir(&paths.dir).map_err(|e| Error::new(e.to_string()))?;
        }
        return Ok(held);
    }
    before_lock();
    let Some(_lock) = StateLock::try_take_until(paths, Instant::now() + Duration::from_secs(2))?
    else {
        if held.iter().any(|&h| h) {
            aterm_durable::durable_dir(&paths.dir).map_err(|e| Error::new(e.to_string()))?;
        }
        return Ok(held);
    };
    let mut store = load_records(paths)?;
    require_commons_source(paths, &store, source)?;
    if let Some(log) = store.log() {
        let crate::membership_log::Body::Genesis { root, .. } = log.genesis().body() else {
            return Ok(vec![false; wires.len()]);
        };
        if *root != expected || !log_entries::commons_log_matches_local_lineage(paths, log)? {
            return Ok(vec![false; wires.len()]);
        }
    }
    let before = store.inventory();
    let complete_before = store
        .log()
        .map_or(0, crate::membership_log::Log::held_count);
    let held_before: Vec<bool> = decoded
        .iter()
        .map(|r| {
            r.as_ref()
                .is_some_and(|rec| store.complete_bytes(rec.id()).is_some())
        })
        .collect();
    let complete =
        |id: &crate::membership_log::RecordId| store.log().and_then(|log| log.get(id)).is_some();
    for record in crate::membership_log::store::causal(decoded.iter().flatten().cloned(), complete)
    {
        let _ = store.insert(record);
    }
    let Some(log) = store.log() else {
        return Ok(vec![false; wires.len()]);
    };
    let crate::membership_log::Body::Genesis { root, .. } = log.genesis().body() else {
        return Ok(vec![false; wires.len()]);
    };
    if *root != expected || !log_entries::commons_log_matches_local_lineage(paths, log)? {
        return Ok(vec![false; wires.len()]);
    }
    let taught_by_crossed = source.is_some_and(|bound| bound.crossed);
    if taught_by_crossed && log.held_count() == complete_before {
        if held_before.iter().any(|&held| held) {
            aterm_durable::durable_dir(&paths.dir).map_err(|e| Error::new(e.to_string()))?;
        }
        return Ok(held_before);
    }
    if store.inventory() != before {
        if taught_by_crossed {
            write_taught_by_crossed(paths, &crossing_mark(paths), &store)?;
        } else {
            write_records(paths, &store)?;
        }
    } else if decoded.iter().any(|r| {
        r.as_ref()
            .is_some_and(|rec| store.complete_bytes(rec.id()).is_some())
    }) {
        aterm_durable::durable_dir(&paths.dir).map_err(|e| Error::new(e.to_string()))?;
    }
    Ok(decoded
        .iter()
        .map(|r| {
            r.as_ref()
                .is_some_and(|rec| store.complete_bytes(rec.id()).is_some())
        })
        .collect())
}

/// Pull verified membership records from the configured vault URL (with failover to configured and
/// discovered candidates), returning how many new records entered `paths.records_file()`.
///
/// # Errors
/// Vault configuration is missing, no candidate server answers cleanly, or persisting fails.
pub fn sync_records(paths: &Paths, wait: Option<Duration>) -> crate::Result<usize> {
    sync_records_inner(paths, wait, &[], None, None)
}

/// Run the ordinary records pass with an empty discovery source.
/// Configured peers and the complete read, write and repost paths remain active.
///
/// # Errors
/// As [`sync_records`].
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn test_sync_records_without_discovery(paths: &Paths) -> crate::Result<usize> {
    let discovery = PeerDiscovery::empty();
    sync_records_inner(paths, None, &[], Some(&discovery), None)
}

/// [`sync_records`] with explicit `candidates` probed alongside policy and discovered peers when
/// the primary fails or when polling immediately (`wait == None`).
///
/// # Errors
/// Vault configuration is missing, neither the primary nor any candidate answers cleanly, or
/// persisting fails.
pub fn sync_records_with_candidates(
    paths: &Paths,
    wait: Option<Duration>,
    candidates: &[String],
) -> crate::Result<usize> {
    sync_records_inner(paths, wait, candidates, None, None)
}

fn sync_records_inner(
    paths: &Paths,
    wait: Option<Duration>,
    extra_peers: &[String],
    browsed: Option<Found<'_>>,
    posts_by: Option<Instant>,
) -> crate::Result<usize> {
    sync_records_until(paths, wait, extra_peers, browsed, None, posts_by).map(|pass| pass.added)
}

/// What one records pass did ([`sync_records_until`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordsPass {
    /// How many new records entered `paths.records_file()`.
    pub(crate) added: usize,
    /// A server that answered holds records the store did not take: it sent frames the store
    /// does not hold complete after the pass, or its pull named a memo's heads ([`UNTAKEN`]),
    /// whose causal closure it then left out of its answer. A removal past a record this build
    /// cannot read is one, so such a pass did not read every record of the vault's newest
    /// (`readmit::admit_by_proof`).
    pub(crate) untaken: bool,
    /// No server that answered stated a records bound compatible with the retained history
    /// ([`crate::crossing`]). An old server can hold none of what came after the vault crossed,
    /// even where this Mac has not crossed yet. A smaller server can likewise stop following
    /// the history when this pass's merge crosses. Neither is evidence of the newest records;
    /// compatible records it sent before the merge can still be retained.
    pub(crate) unstated: bool,
}

impl RecordsPass {
    /// Whether the pass read all of the vault's newest records: it left nothing a server sent
    /// untaken, and a server able to exchange the retained history answered it.
    pub(crate) const fn fresh(&self) -> bool {
        !self.untaken && !self.unstated
    }
}

/// What a bounded records pass ([`sync_records_until`]) keeps of its deadline, once its pulls end,
/// to take the store's lock and ingest what came.
const TAKE_RESERVE: Duration = Duration::from_millis(300);

/// What a records pass ([`sync_records_until`]) makes of the records one server sent: what it
/// sent, frame by frame, whether the pass offers the store all of it, and the batch it offers.
struct RecordsJudged {
    stream: crate::membership_log::untaken::Sent,
    whole: bool,
    batch: Option<Vec<crate::membership_log::Record>>,
}

/// Judge the whole decoded stream, or its readable descendant-closed subset, against the
/// same initial Store and fresh-vault alternatives used by the records pass.
fn judge_received_records(
    local_store: &crate::membership_log::Store,
    vault_name: &str,
    received: crate::membership_log::untaken::ReceivedFrames,
) -> RecordsJudged {
    let batch_valid = |records: &[crate::membership_log::Record]| {
        local_store.clone().ingest_records(records.to_vec()).is_ok()
            || crate::membership_log::Store::for_vault(vault_name)
                .and_then(|mut store| store.ingest_records(records.to_vec()))
                .is_ok()
    };
    let (batch, whole) = match received.whole() {
        Some(records) if batch_valid(&records) => (Some(records), true),
        _ => {
            let part = received.readable();
            let part_len: usize = part.iter().map(|record| record.bytes().len()).sum();
            let part = (!part.is_empty() && part_len < received.byte_len() && batch_valid(&part))
                .then_some(part);
            (part, false)
        }
    };
    RecordsJudged {
        stream: received.into_sent(),
        whole,
        batch,
    }
}

/// What a records pass holds of a peer's answer once every pull has ended.
struct PeerRecords {
    /// The records it sent.
    records: Brought,
    /// Whether it lacks a record the pull named ([`transport::ReceivedRecordsAnswer::needs`]).
    needs: bool,
    /// Its boot id ([`transport::ReceivedRecordsAnswer::boot`]).
    boot: Option<String>,
    /// What it stated of its records ([`transport::ReceivedRecordsAnswer::bound`]).
    bound: transport::ReplyBound,
}

/// The records a peer sent, as a records pass holds them once every pull has ended.
enum Brought {
    /// No longer than [`fan_out::RECORDS`]' peek (empty for a `204` or a `404`): judged later.
    Unjudged(crate::membership_log::untaken::ReceivedFrames),
    /// Longer: judged once its whole read ended, while that read still held its slot, so no
    /// more such large responses awaiting Store judgment are held at once than
    /// [`fan_out::reads_within`] reads whole at once.
    Judged(RecordsJudged),
}

impl Brought {
    /// Whether the peer sent any record.
    fn any(&self) -> bool {
        match self {
            Self::Unjudged(received) => received.byte_len() > 0,
            Self::Judged(_) => true,
        }
    }
}

impl PeerRecords {
    /// What a pass holds of `answer`, `judge` judging its records when they are longer than the
    /// peek.
    fn of(
        answer: transport::ReceivedRecordsAnswer,
        judge: impl Fn(crate::membership_log::untaken::ReceivedFrames) -> RecordsJudged,
    ) -> Self {
        let transport::ReceivedRecordsAnswer {
            received,
            needs,
            boot,
            bound,
        } = answer;
        let records = if received.byte_len() > fan_out::RECORDS.peek {
            Brought::Judged(judge(received))
        } else {
            Brought::Unjudged(received)
        };
        Self {
            records,
            needs,
            boot,
            bound,
        }
    }
}

/// One records pass: pull from the source and its peers (beside the source where nothing waits,
/// after a long poll of it that failed), keep what verifies, and post each server that said it
/// lacks this Mac's frontier what [`Reposted::due`] makes it due: the complete records past what
/// it last judged; the store's complete records (`encode_held`) where it judged none, none for
/// [`REPOST_AGAIN`], or answers another boot id than its last judging pass heard (at most once per
/// [`REPOST_AGAIN`]); or nothing where it judged this frontier within [`REPOST_AGAIN`] and has not
/// restarted since.
/// A bounded caller (`deadline`) ends every exchange by it and posts nothing; otherwise the posts
/// end by `posts_by`, or one exchange from when they start where none is given.
///
/// Every pull and post states this Mac's records bound ([`crate::crossing`]). Once its log
/// crossed, or once a server that states it crossed sent records, the pass takes nothing from a
/// server that stated no bound, as from one that failed, and posts only to servers that stated
/// one. A Mac that has not crossed takes what crossed servers sent after the rest, and only where
/// it grows the store's complete log: the mark is then stored before the records file holds any
/// of it, and a crossed server's batch that brings nothing new, or only records waiting on a
/// parent, is not taken and stores no mark. Crossed or not, the pass is fresh
/// ([`RecordsPass::fresh`]) only where a server compatible with the retained history answered.
fn sync_records_until(
    paths: &Paths,
    wait: Option<Duration>,
    extra_peers: &[String],
    browsed: Option<Found<'_>>,
    deadline: Option<Instant>,
    posts_by: Option<Instant>,
) -> crate::Result<RecordsPass> {
    use crate::membership_log::untaken::{Named, Noted, Sent};

    let config = configured(paths)?;
    let mark = crossing_mark(paths);
    let Source::Url(primary_url) = &config.source else {
        // A vault at a path has no server, so none before the fence.
        return Ok(RecordsPass {
            added: 0,
            untaken: false,
            unstated: false,
        });
    };
    let local_store = load_records(paths)?;
    let crossed_before = crate::crossing::crossed(&mark, &local_store)?;
    let bound = crate::crossing::RecordsBound::of(local_store.budgets().held, crossed_before);
    let records_file = paths.records_file();
    let now = Instant::now();
    // The frontier of the store every pull below names `have` from: a pass notes what a server
    // sent that the store did not take only against a store that still holds it ([`UNTAKEN`]).
    let named_from = local_store.frontier();
    let have = local_store.have();
    let named = |url: &str| UNTAKEN.have(&records_file, url, &local_store, &have, now);
    let subscriber = subscriber_recipient(paths);
    let total = wait.map_or(servers_within(Duration::from_secs(10)), |w| {
        w.saturating_add(Duration::from_secs(10))
    });
    // A bounded pass ends its pulls [`TAKE_RESERVE`] before its deadline: a server that answers
    // nothing until then must not leave what the others sent untaken for want of the time to
    // take the store and ingest it.
    let pulls_by = deadline.map(|by| {
        by.checked_sub(TAKE_RESERVE)
            .filter(|cut| *cut > now)
            .unwrap_or(by)
    });
    let total = bounded::budget(
        pulls_by,
        pulls_by.map_or(total, |_| total.min(servers_within(PROBE_DEADLINE))),
    )?;
    let primary_named = named(primary_url);
    let sub_ref = subscriber.as_deref();
    let fetch_primary = || {
        transport::fetch_record_frames_sync(
            primary_url,
            &primary_named.ids,
            wait,
            total,
            sub_ref,
            bound,
        )
    };
    // A bounded pass browses for peers by its own deadline, not a fixed probe of its own.
    let bounded_browse = PeerDiscovery::direct(pulls_by);
    let browsed = browsed.or_else(|| pulls_by.map(|_| &bounded_browse));
    let collect_peers = || {
        collect_sync_peer_urls(
            paths,
            &config,
            primary_url,
            extra_peers,
            browsed,
            PeerUse::Records,
        )
    };
    let vault_name = if alab_vault(&config) {
        crate::ALAB_VAULT
    } else if config.name != crate::ALAB_VAULT {
        config.name.as_str()
    } else {
        ""
    };
    // What a non-empty answer brings: the whole stream as its batch when it verifies, else the
    // records of it this build reads that descend from none it cannot read, when those verify.
    // A server of a newer build sends a record of a kind this build does not know beside records
    // it reads, and refusing the whole stream kept those records from every server that holds
    // it.
    let judge = |received| judge_received_records(&local_store, vault_name, received);
    // A mirror this Mac lists is read and never posted to: what it lacks, it takes from the
    // vault's servers, not from a member. It is not told who reads
    // ([`mirrors::ReadOnly::subscriber_for`]), and its answer never vouches that the pass read
    // the vault's newest records (`answering_bounds` below).
    let read_only = mirrors::ReadOnly::of(paths, &config, extra_peers);
    // Each answer with whether the pull that asked for it named a memo's heads ([`UNTAKEN`]).
    type PeerRecordsFetch = (String, bool, crate::Result<PeerRecords>);
    let fetch_peers = |peers: &[String]| -> Vec<PeerRecordsFetch> {
        if peers.is_empty() {
            return Vec::new();
        }
        let peer_named: Vec<(&String, Named)> = peers.iter().map(|url| (url, named(url))).collect();
        // Every peer is asked at once, sharing one deadline, as [`fan_out::RECORDS`] reads them:
        // any LAN host can advertise up to [`crate::discover::MAX_CANDIDATES`] of them, and read
        // whole at once, each up to [`crate::membership_log::MAX_RECORDS_STREAM`], they could
        // have a pass hold up to a gigabyte of records nothing had verified. The deadline is read
        // here, on this thread, where a test sets it ([`servers_within`]).
        let fetched = match bounded::budget(pulls_by, servers_within(PROBE_DEADLINE)) {
            Ok(within) => {
                let ids = |url: &str| {
                    peer_named
                        .iter()
                        .find(|(peer, _)| peer.as_str() == url)
                        .map_or(&[][..], |(_, named)| named.ids.as_slice())
                };
                fan_out::reads_within(
                    fan_out::RECORDS,
                    peers,
                    within,
                    |url, left, limit| {
                        let sub = read_only.subscriber_for(url, sub_ref);
                        transport::fetch_records_within(url, ids(url), left, sub, bound, limit)
                    },
                    |url, read| match read? {
                        transport::Pulled::Fetched(answer) => Ok(PeerRecords::of(answer, judge)),
                        transport::Pulled::Larger => Err(transport::past_any_records_stream(url)),
                    },
                )
            }
            Err(elapsed) => peers.iter().map(|_| Some(Err(elapsed.clone()))).collect(),
        };
        peer_named
            .iter()
            .zip(fetched)
            .map(|((url, named), fetched)| {
                (
                    (*url).clone(),
                    named.heads > 0,
                    fetched.unwrap_or_else(|| Err(Error::new("peer record fetch panicked"))),
                )
            })
            .collect()
    };
    let (primary, peers, peer_batches) = if wait.is_none() {
        // Where nothing waits, every peer is asked too, and beside the primary rather than after
        // it: a primary that does not answer must not spend a bounded pass's whole deadline
        // (readmit's freshness read) before any peer is asked.
        std::thread::scope(|scope| {
            let primary = scope.spawn(fetch_primary);
            let peers = collect_peers();
            let peer_batches = fetch_peers(&peers);
            let primary = primary
                .join()
                .unwrap_or_else(|_| Err(Error::new("primary record fetch panicked")));
            (primary, peers, peer_batches)
        })
    } else {
        let primary = fetch_primary();
        // A successful long poll uses only the primary. Leave the next peer batch for a pass
        // that asks those peers instead of consuming it without a request.
        let peers = if primary.is_err() {
            collect_peers()
        } else {
            Vec::new()
        };
        let peer_batches = fetch_peers(&peers);
        (primary, peers, peer_batches)
    };

    // A server that states its log crossed and sent records may cross this Mac's
    // ([`crate::crossing`]): then, and once it crossed, a server that stated no bound counts as
    // one that failed, so nothing it sent is taken, noted or answered.
    let crossing = !crossed_before
        && (primary
            .as_ref()
            .is_ok_and(|answer| answer.bound.crossed() && answer.received.byte_len() > 0)
            || peer_batches.iter().any(|(_, _, res)| {
                res.as_ref()
                    .is_ok_and(|peer| peer.bound.crossed() && peer.records.any())
            }));
    let unstated_source = |url: &str| {
        Error::new(format!(
            "{}: this Mac's membership log crossed the bound a build before the fence holds, and \
             the server stated no records bound, so nothing it sent is taken",
            transport::describe(url)
        ))
    };
    let (primary, peer_batches) = if crossed_before || crossing {
        let primary = primary.and_then(|answer| match answer.bound {
            transport::ReplyBound::Unstated => Err(unstated_source(primary_url)),
            transport::ReplyBound::Unheard | transport::ReplyBound::Stated(_) => Ok(answer),
        });
        let peer_batches: Vec<PeerRecordsFetch> = peer_batches
            .into_iter()
            .map(|(url, named_heads, res)| {
                let res = res.and_then(|peer| match peer.bound {
                    transport::ReplyBound::Unstated => Err(unstated_source(&url)),
                    transport::ReplyBound::Unheard | transport::ReplyBound::Stated(_) => Ok(peer),
                });
                (url, named_heads, res)
            })
            .collect();
        (primary, peer_batches)
    } else {
        (primary, peer_batches)
    };
    // Keep the answering bounds until ingestion ends: merging uncrossed branches can cross
    // this Mac's log, making a previously compatible smaller server unable to follow it. A
    // mirror's is none of them: nothing posts a mirror records, so it falls behind by
    // construction, and a pass only a mirror answered never vouches that this Mac read a
    // removal (`readmit`'s freshness read fails closed then).
    let answering_bounds: Vec<_> = primary
        .as_ref()
        .ok()
        .and_then(|answer| answer.bound.stated())
        .into_iter()
        .chain(
            peer_batches
                .iter()
                .filter(|(url, _, _)| !read_only.holds(url))
                .filter_map(|(_, _, res)| res.as_ref().ok().and_then(|peer| peer.bound.stated())),
        )
        .collect();
    // A server that answered a pull naming a memo's heads left their closure out of its answer.
    let mut untaken = (primary.is_ok() && primary_named.heads > 0)
        || peer_batches
            .iter()
            .any(|(_, named_heads, res)| *named_heads && res.is_ok());
    // What each server that answered records sent, so the pass can note what the store did not
    // take ([`UNTAKEN`]): every return below that judged those records notes them first. A lock,
    // read or write that fails returns before any note, and the same records come again. Each
    // with whether the pass offered the store all of it.
    let mut sent: Vec<(String, Sent, bool)> = Vec::new();
    // The batch what `url` sent brings, once `sent` notes it.
    let mut offered =
        |url: &str, judged: RecordsJudged| -> Option<Vec<crate::membership_log::Record>> {
            sent.push((url.to_owned(), judged.stream, judged.whole));
            judged.batch
        };
    // Each batch, with whether a server that states its log crossed sent it.
    let mut verified_batches: Vec<(Vec<crate::membership_log::Record>, bool)> = Vec::new();
    // Each server that lacks this Mac's frontier, with the boot id its answer carried and what
    // it stated of its records.
    let mut push_urls: Vec<(String, Option<String>, transport::ReplyBound)> = Vec::new();
    let mut any_answered = false;
    let mut primary_err = None;
    let mut primary_refused = false;
    match primary {
        Ok(transport::ReceivedRecordsAnswer {
            received,
            needs,
            boot,
            bound,
        }) => {
            any_answered = true;
            if primary_named.heads > 0 {
                UNTAKEN.heard(&records_file, primary_url, needs, now);
            }
            if !needs {
                Reposted::satisfied(paths, primary_url);
            }
            if received.byte_len() == 0 {
                if needs {
                    push_urls.push((primary_url.clone(), boot, bound));
                }
            } else if let Some(batch) = offered(primary_url, judge(received)) {
                verified_batches.push((batch, bound.crossed()));
                if needs {
                    push_urls.push((primary_url.clone(), boot, bound));
                }
            } else {
                primary_refused = true;
            }
        }
        Err(err) => primary_err = Some(err),
    }
    for (url, named_heads, res) in peer_batches {
        let Ok(PeerRecords {
            records,
            needs,
            boot,
            bound,
        }) = res
        else {
            continue;
        };
        if named_heads {
            UNTAKEN.heard(&records_file, &url, needs, now);
        }
        if !needs {
            Reposted::satisfied(paths, &url);
        }
        let post = needs && !read_only.holds(&url);
        let judged = match records {
            Brought::Unjudged(received) if received.byte_len() == 0 => None,
            Brought::Unjudged(received) => Some(judge(received)),
            Brought::Judged(judged) => Some(judged),
        };
        match judged {
            None => {
                any_answered = true;
                if post {
                    push_urls.push((url, boot, bound));
                }
            }
            Some(judged) => {
                if let Some(batch) = offered(&url, judged) {
                    any_answered = true;
                    verified_batches.push((batch, bound.crossed()));
                    if post {
                        push_urls.push((url, boot, bound));
                    }
                }
            }
        }
    }
    // Notes what each server sent against `store`, the store after the pass, and says whether
    // the store did not take all of it.
    let note = |store: &crate::membership_log::Store| -> bool {
        let noted = Noted {
            store,
            named_from: &named_from,
            now,
            source: Some(primary_url),
        };
        for (url, stream, _) in &sent {
            UNTAKEN.note(records_file.clone(), url, stream, &noted);
        }
        sent.iter()
            .any(|(_, stream, whole)| !whole || stream.left_in(store))
    };
    if primary_refused && peers.is_empty() {
        note(&local_store);
        return Err(Error::new(
            "the vault server returned an invalid membership record stream",
        ));
    }
    if !any_answered && let Some(err) = primary_err {
        note(&local_store);
        return Err(err);
    }
    let lock = bounded::take(paths, deadline)?;
    let mut store = load_records(paths)?;
    // Pulls run outside the writer lock. Another writer can cross while they are in flight;
    // their earlier filtering and freshness judgment then describe the wrong side of the fence.
    // Discard that pass before ingest or a fresh admission, even if its response was empty.
    if !crossed_before && crate::crossing::crossed(&mark, &store)? {
        return Err(Error::new(
            "the membership log crossed while its records pull was in flight",
        ));
    }
    let mut retained_bound = records_bound(paths, &store)?;
    let added = if verified_batches.is_empty() {
        untaken |= note(&store);
        0
    } else {
        // MUTANT (`crate::crossing::Faults::mark_on_receipt`): the mark once a crossed log's
        // server sent records, before the store shows it takes any.
        #[cfg(test)]
        if crossing
            && verified_batches
                .iter()
                .any(|(_, from_crossed)| *from_crossed)
            && crate::crossing::faults(mark.path()).mark_on_receipt
        {
            mark.store()?;
        }
        let before = store.inventory();
        let rooted = store.log().is_some();
        let mut any_ingested = false;
        let mut last_ingest_err = None;
        // Whether the store took a batch a crossed log's server sent that brought it a record it
        // lacked: the store is this pass's own until `write_records`, so the mark goes there,
        // before the file holds such a record ([`write_taught_by_crossed`]).
        let mut taught_by_crossed = false;
        // What crossed logs' servers sent a Mac that has not crossed: taken after the rest, and
        // only where it grows the complete log.
        let mut from_crossed_logs = Vec::new();
        for (batch, from_crossed) in verified_batches {
            if from_crossed && !crossed_before {
                from_crossed_logs.push(batch);
                continue;
            }
            match store.ingest_records(batch) {
                Ok(brought) => {
                    any_ingested = true;
                    taught_by_crossed |= from_crossed && brought > 0;
                }
                Err(err) => last_ingest_err = Some(err),
            }
        }
        if !from_crossed_logs.is_empty() {
            // Into a copy, so a crossed log's batch that only brings records waiting on a parent
            // is not taken and stores no mark: a record waits for any key that ever held a seat
            // (`Log::insert`), a removed Mac's among them, with a parent that may never come, and
            // it would cross this Mac for good though it never completes. An honest server answers
            // every complete record past the ids this pull named, so its records wait here only on
            // records this store can never take (`membership_log::untaken`), and the pass notes
            // them untaken whether it holds them waiting or leaves them out.
            let complete = |store: &crate::membership_log::Store| {
                store
                    .log()
                    .map_or(0, crate::membership_log::Log::held_count)
            };
            let mut taught = store.clone();
            for batch in from_crossed_logs {
                match taught.ingest_records(batch) {
                    Ok(_) => any_ingested = true,
                    Err(err) => last_ingest_err = Some(err),
                }
            }
            let grew = complete(&taught) > complete(&store);
            // MUTANT (`crate::crossing::Faults::mark_on_waiting`): the copy taken, and the mark
            // stored, where it brought any record, a waiting one among them.
            #[cfg(test)]
            let grew = grew
                || (crate::crossing::faults(mark.path()).mark_on_waiting
                    && !taught.inventory().is_subset(&store.inventory()));
            if grew {
                store = taught;
                taught_by_crossed = true;
            }
        }
        if !any_ingested && let Some(err) = last_ingest_err {
            // Every ingest failed, so `store` is the file as this lock found it.
            note(&store);
            return Err(err);
        }
        // A store with no genesis keeps a pull only where it brings the genesis that roots it:
        // nothing before a genesis can be checked, any LAN host can answer this pull, and an
        // honest server sends such a store its log from the genesis (`Store::missing`). ALab's
        // store checks that genesis itself (by the compiled digest, or by the vault's keys);
        // another vault's checks none, and the first genesis it keeps binds it for good, so
        // here it is one the vault's key line signed, as one Commons offers must be
        // (`retain_records_for_commons`).
        let kept = match store.log() {
            None => false,
            Some(_) if rooted || vault_name == crate::ALAB_VAULT => true,
            Some(log) => log_entries::commons_log_matches_local_lineage(paths, log)?,
        };
        let added = if kept {
            store.inventory().difference(&before).count()
        } else {
            0
        };
        if added > 0 && taught_by_crossed {
            write_taught_by_crossed(paths, &mark, &store)?;
        } else if added > 0 {
            write_records(paths, &store)?;
        }
        // Against the store the lock holds, which is the file now: never a store read after the
        // lock, which another writer may have shrunk since. A store not kept is the file as the
        // lock found it, with no genesis, which no memo applies to, and it took nothing sent.
        if kept {
            untaken |= note(&store);
            retained_bound = records_bound(paths, &store)?;
        } else {
            untaken |= !sent.is_empty();
        }
        added
    };
    let stated = answering_bounds
        .into_iter()
        .any(|peer| retained_bound.can_exchange(peer));
    drop(lock);
    if deadline.is_none()
        && !push_urls.is_empty()
        && let Ok(store) = load_records(paths)
        && store.log().is_some()
        && let Ok(bound) = records_bound(paths, &store)
    {
        // Complete records only: a server keeps the records it holds waiting on a parent in
        // memory only and serves none of them (F23, rule 7), so they bought nothing on the wire.
        let frontier = sha256(&store.frontier().concat());
        let have = store.have();
        // Post only to compatible servers. A crossed Mac's query also makes older servers
        // refuse its body before reading it.
        let due: Vec<Due<'_>> = push_urls
            .iter()
            .filter(|(_, _, stated)| match stated.stated() {
                Some(peer) => bound.can_exchange(peer),
                None => !bound.crossed,
            })
            .filter_map(|(url, boot, _)| {
                let boot = boot.as_deref();
                let (past, posting) = match Reposted::due(paths, url, &frontier, boot)? {
                    Repost::Whole { boot_reset } => (None, Posting::Whole { boot_reset }),
                    Repost::Past(judged) => (Some(store.missing(&judged)), Posting::Past),
                };
                Some(Due {
                    url,
                    boot,
                    past,
                    posting,
                })
            })
            .collect();
        // Whole selections share this immutable snapshot. Do not collect it until one is due:
        // an unchanged frontier that every server judged needs no record owners or wire buffer.
        let whole = if due.iter().any(|due| due.past.is_none()) {
            store.missing(&[])
        } else {
            Vec::new()
        };
        // Every server due records is posted them at once ([`fan_out::post_record_selections`]), all by one
        // deadline: one that takes the post and never reads it holds this pass, and the publish
        // that ran it, for one exchange, not one each. A server due nothing past what it judged
        // is judged at once.
        let posts: Vec<(&str, &[crate::membership_log::Record])> = due
            .iter()
            .map(|due| (due.url, due.past.as_deref().unwrap_or(&whole)))
            .filter(|(_, body)| !body.is_empty())
            .collect();
        let by = posts_by.unwrap_or_else(fan_out::one_exchange);
        let mut answers = fan_out::post_record_selections(&posts, bound, by).into_iter();
        for due in &due {
            // `answers` holds one answer for each non-empty body, in order.
            if due.past.as_deref().unwrap_or(&whole).is_empty()
                || answers.next().is_some_and(|answer| answer.is_ok())
            {
                Reposted::posted(
                    paths,
                    due.url,
                    &frontier,
                    have.clone(),
                    due.posting,
                    due.boot,
                );
            }
        }
    }
    Ok(RecordsPass {
        added,
        untaken,
        unstated: !stated,
    })
}

/// Write `store`, which holds a record a crossed log's server taught this Mac
/// ([`crate::crossing`]): the mark first, so the records file never holds such a record without
/// it.
///
/// # Errors
/// The mark cannot be stored (the file is then left as it was), or the file cannot be written.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsCapacity",
        action = "MacPulls",
        project = "aterm_secrets::client::record_persistence_tests::records_capacity_mac"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "SwarmRecordsCapacity",
        action = "MacPullDies",
        project = "aterm_secrets::client::record_persistence_tests::records_capacity_mac"
    )
)]
fn write_taught_by_crossed(
    paths: &Paths,
    mark: &crate::crossing::Mark,
    store: &crate::membership_log::Store,
) -> crate::Result<()> {
    after_crossed_mark(mark, || write_records(paths, store))
}

/// Publish custody only after its source's durable crossed mark. The closure may also publish
/// the configuration selecting those records, so no new reader can observe them without it.
fn after_crossed_mark<T>(
    mark: &crate::crossing::Mark,
    publish: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<T> {
    #[cfg(test)]
    let faults = crate::crossing::faults(mark.path());
    // MUTANT (`crate::crossing::Faults::mark_after_take`): the file first, then the mark.
    #[cfg(test)]
    if faults.mark_after_take {
        let result = publish()?;
        if faults.dies_between {
            return Err(Error::new("a test's Mac died before its crossed mark"));
        }
        mark.store()?;
        return Ok(result);
    }
    mark.store()?;
    #[cfg(test)]
    if faults.dies_between {
        return Err(Error::new(
            "a test's Mac died between its crossed mark and its records",
        ));
    }
    publish()
}

/// How long a server that said it lacked this Mac's frontier, was posted the store, judged it
/// (took it or refused it, not a busy or failing answer: `transport::post_records_answered`), and
/// still says it lacks a frontier goes without the whole store again ([`Reposted`]). A frontier
/// that moves sooner is posted only what it holds past the one the server judged; this long
/// after, the whole store goes again, to a server that may since take it (a newer build, room
/// it lacked).
const REPOST_AGAIN: Duration = Duration::from_secs(60 * 60);

/// The stores this process posted to servers that said they lacked its frontier
/// (`X-Aterm-Records-Need`), keyed by records file and server URL: a server that judged such a
/// post and still lacks a frontier could not take all of it (a record its build refuses, a store
/// at its bound), and posting it the same records again, up to
/// [`crate::membership_log::MAX_RECORDS_STREAM`] each time, would not change that. It is posted
/// nothing more while the frontier stands, and once it moves only the records past what the
/// server judged (what this Mac minted or heard since), not the whole store on each record minted
/// anywhere in the swarm.
struct Reposted;

/// What a server that said it lacked this Mac's frontier is due ([`Reposted::due`]).
enum Repost {
    /// The whole store: it judged none of it, or none for [`REPOST_AGAIN`], or it restarted
    /// (`boot_reset`: a pass heard another boot id from it) and so forgot the records it held
    /// waiting on a parent.
    Whole { boot_reset: bool },
    /// The records past these ids, the store's [`crate::membership_log::Store::have`] when the
    /// server last judged a post of it ([`crate::membership_log::Store::missing`]).
    Past(Vec<crate::membership_log::RecordId>),
}

/// A server a records pass posts to ([`sync_records_until`]), and what it is due.
struct Due<'a> {
    url: &'a str,
    /// The boot id the pass heard from it.
    boot: Option<&'a str>,
    /// The records past what it last judged; `None` for the whole store.
    past: Option<Vec<crate::membership_log::Record>>,
    posting: Posting,
}

/// What a pass posted a server that judged it ([`Reposted::posted`]).
#[derive(Clone, Copy)]
enum Posting {
    /// The whole store, `boot_reset` when its boot id drew it ([`Repost::Whole`]).
    Whole { boot_reset: bool },
    /// The records past what it last judged.
    Past,
}

/// What [`Reposted`] keeps for one server: the frontier digest of the store it last judged, that
/// store's `have`, when it was last posted the whole store, the boot id the judging pass heard
/// from it ([`transport::RECORDS_BOOT_HEADER`]; `None` from a server before F23), and when a boot
/// id it had not heard last drew it a whole store.
struct PostJudged {
    frontier: [u8; 32],
    have: Vec<crate::membership_log::RecordId>,
    whole_at: Instant,
    boot: Option<String>,
    boot_reset_at: Option<Instant>,
}

/// [`Reposted`]'s table, for this process.
static REPOSTED: std::sync::Mutex<BTreeMap<(PathBuf, String), PostJudged>> =
    std::sync::Mutex::new(BTreeMap::new());

/// How far a test moved [`Reposted`]'s clock forward for each records file
/// ([`Reposted::advance`]).
#[cfg(test)]
static REPOSTED_SKEW: std::sync::Mutex<BTreeMap<PathBuf, Duration>> =
    std::sync::Mutex::new(BTreeMap::new());

/// What a server sent this Mac's records passes that its store could not take (a kind only a
/// newer build writes, a store at its bound, a record the log refuses), kept per records file and
/// server URL for this process: the next pull to that server names it first, so the server
/// answers only what is new past it rather than the same records on every pass
/// ([`crate::membership_log::untaken`]).
static UNTAKEN: crate::membership_log::untaken::Untaken<PathBuf> =
    crate::membership_log::untaken::Untaken::new();

impl Reposted {
    fn key(paths: &Paths, url: &str) -> (PathBuf, String) {
        (paths.records_file(), url.to_owned())
    }

    /// Now, as [`Reposted`] keeps time for the records file `file`: the monotonic clock, which a
    /// test moves forward for its own records file alone ([`Reposted::advance`]).
    fn now(file: &Path) -> Instant {
        let now = Instant::now();
        #[cfg(test)]
        if let Some(skew) = REPOSTED_SKEW
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(file)
        {
            return now + *skew;
        }
        #[cfg(not(test))]
        let _ = file;
        now
    }

    /// Move [`Reposted`]'s clock for `paths`' records file forward by `by`.
    #[cfg(test)]
    fn advance(paths: &Paths, by: Duration) {
        *REPOSTED_SKEW
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(paths.records_file())
            .or_default() += by;
    }

    /// What `url`, which answered this pass with boot id `boot`, is due of the store whose
    /// frontier digest is `frontier`: nothing when it judged that frontier within [`REPOST_AGAIN`]
    /// and has not restarted since. A boot id other than the one the judging pass heard is a
    /// restart (F23, rule 8), which forgot the records the server held waiting on a parent, some
    /// of them maybe this Mac's that a post past what it judged leaves out; it draws the whole
    /// store, unless a boot id already drew one from that server within [`REPOST_AGAIN`]: a host
    /// that changed its id on every reply would draw a whole store on every pass. A missing
    /// header matches a missing header, so a server before F23 keeps the rule it had.
    fn due(paths: &Paths, url: &str, frontier: &[u8; 32], boot: Option<&str>) -> Option<Repost> {
        let key = Self::key(paths, url);
        let now = Self::now(&key.0);
        match REPOSTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            None => Some(Repost::Whole { boot_reset: false }),
            Some(judged) if now.saturating_duration_since(judged.whole_at) >= REPOST_AGAIN => {
                Some(Repost::Whole { boot_reset: false })
            }
            Some(judged)
                if judged.boot.as_deref() != boot
                    && judged
                        .boot_reset_at
                        .is_none_or(|at| now.saturating_duration_since(at) >= REPOST_AGAIN) =>
            {
                Some(Repost::Whole { boot_reset: true })
            }
            Some(judged) if &judged.frontier != frontier => Some(Repost::Past(judged.have.clone())),
            Some(_) => None,
        }
    }

    /// `url`, whose answer this pass carried boot id `boot`, judged `posting` of the store whose
    /// frontier digest is `frontier` and whose `have` is `have`. A whole store posted while `url`
    /// answered a boot id other than the one its last judging pass heard serves that restart
    /// too, so it counts as the boot id's whole store: a host whose id changes on every reply
    /// draws one whole store per [`REPOST_AGAIN`] (the one its last whole post's age draws, which
    /// is also its boot id's), not that one and a boot id's beside it.
    fn posted(
        paths: &Paths,
        url: &str,
        frontier: &[u8; 32],
        have: Vec<crate::membership_log::RecordId>,
        posting: Posting,
        boot: Option<&str>,
    ) {
        let mut table = REPOSTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = Self::key(paths, url);
        let now = Self::now(&key.0);
        let before = table.get(&key);
        let whole_at = match (posting, before) {
            (Posting::Past, Some(judged)) => judged.whole_at,
            _ => now,
        };
        let boot_reset_at = match (posting, before) {
            (Posting::Whole { boot_reset: true }, _) => Some(now),
            (Posting::Whole { boot_reset: false }, Some(judged))
                if judged.boot.as_deref() != boot =>
            {
                Some(now)
            }
            (_, before) => before.and_then(|judged| judged.boot_reset_at),
        };
        table.insert(
            key,
            PostJudged {
                frontier: *frontier,
                have,
                whole_at,
                boot: boot.map(str::to_owned),
                boot_reset_at,
            },
        );
        // An entry outlives its whole post's `REPOST_AGAIN` by another: the whole post that age
        // draws then finds the boot id its last judging pass heard, though another server's pass
        // ran this first, and counts as the boot id's whole store too.
        table.retain(|(file, _), judged| {
            Self::now(file).saturating_duration_since(judged.whole_at) < REPOST_AGAIN * 2
        });
    }

    /// `url` answered a pull without saying it lacks a record named: it holds what was posted.
    fn satisfied(paths: &Paths, url: &str) {
        REPOSTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&Self::key(paths, url));
    }
}

fn subscriber_recipient(paths: &Paths) -> Option<String> {
    read_identity(paths)
        .ok()
        .flatten()
        .map(|identity| identity.recipient().to_bech32())
}

fn policy_peer_urls(paths: &Paths, config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(Some(entry)) = entry::read_reserved_entry(paths, config, crate::SWARM_POLICY_ENTRY)
        && let Ok(text) = std::str::from_utf8(entry.value().as_slice())
        && let Ok(policy) = crate::swarm_roster::SwarmPolicy::from_toml(text)
    {
        ledger_publish::push_policy_server_urls(&policy, &config.name, |u| out.push(u.to_owned()));
    }
    out
}

/// A subscriber's discovery source and independent record/envelope selection cursors.
type Found<'a> = &'a PeerDiscovery<'a>;

/// A [`Found`] that found nothing.
#[cfg(test)]
static FOUND_NONE: PeerDiscovery<'static> = PeerDiscovery::empty();

/// Additional publication servers: the compiled-in ALab URLs first, then the servers `found`
/// or a bounded browse supplies, highest advertised `seq` first
/// ([`crate::discover::candidate_urls`]). The publisher separately includes its trusted policy
/// and explicit URLs. A unit test dials no compiled-in URL and runs no browse.
fn live_alab_peer_urls(_paths: &Paths, config: &Config, found: Option<Found<'_>>) -> Vec<String> {
    let alab = !cfg!(test) && alab_vault(config);
    let discovered = match found {
        Some(found) => found.read(config),
        None if alab => crate::discover::browse_alab(PROBE_DEADLINE).unwrap_or_default(),
        None => Vec::new(),
    };
    let defaults: &[&str] = if alab { crate::ALAB_DEFAULT_URLS } else { &[] };
    crate::discover::candidate_urls(defaults, &discovered, &config.name)
}

/// The vault servers a pass of `config` reads beyond `primary_url` for `purpose`: the policy's
/// servers and, where `purpose` reads the vault ([`PeerUse::vault_read`]), the mirrors `source.toml` lists,
/// then `extra_peers` or what `found` discovers ([`PeerDiscovery::select`]). What any of them
/// serves counts only as far as it verifies under the one pin and floor
/// ([`peer_selection::select`]); what the pass writes goes to [`mirrors::ReadOnly::servers`].
fn collect_sync_peer_urls(
    paths: &Paths,
    config: &Config,
    primary_url: &str,
    extra_peers: &[String],
    found: Option<Found<'_>>,
    purpose: PeerUse,
) -> Vec<String> {
    if let Some(selected) = found.and_then(|source| source.preselected(config, primary_url)) {
        return selected;
    }
    let mut known = policy_peer_urls(paths, config);
    if purpose.vault_read() {
        known.extend(config.mirrors.iter().cloned());
    }
    let direct = PeerDiscovery::direct(None);
    found
        .unwrap_or(&direct)
        .select(paths, config, primary_url, &known, extra_peers, purpose)
}

struct SyncPeerHead {
    /// The peer that served it.
    url: String,
    seq: u64,
    digest: [u8; 32],
    parents: Vec<[u8; 32]>,
    envelope: Vec<u8>,
    network_body: Option<network_observation::Body>,
}

/// The head of the vault the peer at `url` served in `fetched`, where it verifies under `pin`,
/// with the network evidence `attempt` binds to its bytes.
fn sync_peer_head(
    url: &str,
    fetched: Fetched,
    attempt: Option<network_observation::Attempt>,
    config: &Config,
    pin: &[u8; 32],
) -> Option<SyncPeerHead> {
    let network_body = attempt.and_then(|attempt| attempt.received(&fetched));
    let Fetched::Body(body) = fetched else {
        return None;
    };
    let (envelope, _, _) =
        envelope::verify_for_reader(&body, &config.name, pin, &config.followed, None).ok()?;
    let digest = envelope.digest();
    Some(SyncPeerHead {
        url: url.to_owned(),
        seq: envelope.seq,
        digest,
        parents: envelope.parents,
        envelope: body,
        network_body,
    })
}

/// The verified heads every one of `urls` serves `subscriber`, read at once within `total`
/// ([`fan_out::envelopes_within`]), each read's lookup and connect within
/// [`transport::PROBE_CONNECT`]: in `urls`' order, without the peers that served none in time or
/// served one that does not verify. A mirror of `read_only`'s is not told the subscriber.
fn probe_peers_for_sync(
    paths: &Paths,
    urls: &[String],
    config: &Config,
    pin: &[u8; 32],
    total: Duration,
    subscriber: Option<&str>,
    read_only: &mirrors::ReadOnly,
) -> Vec<SyncPeerHead> {
    if urls.is_empty() {
        return Vec::new();
    }
    let attempt = network_observation::Attempt::start(paths, config, VaultNetworkSource::PeerUrl);
    fan_out::envelopes_within(
        urls,
        total,
        |url, left, limit| {
            let connect = Some(transport::PROBE_CONNECT.min(left));
            let subscriber = read_only.subscriber_for(url, subscriber);
            transport::get_within_as(url, left, connect, subscriber, limit)
        },
        |url, fetched| sync_peer_head(url, fetched, attempt, config, pin),
    )
    .into_iter()
    .flatten()
    .collect()
}

type HeadLineage = (u64, [u8; 32], Vec<[u8; 32]>);

fn heads_diverged(heads: &[HeadLineage]) -> bool {
    let Some(leader) = heads.iter().max_by_key(|(seq, digest, _)| (*seq, *digest)) else {
        return false;
    };
    heads.iter().any(|(seq, digest, _)| {
        *digest != leader.1
            && (*seq == leader.0 || leader.2.is_empty() || !leader.2.contains(digest))
    })
}

fn peer_subsumes_held(
    paths: &Paths,
    config: &Config,
    before: &State,
    probed: &SyncPeerHead,
    held_bytes: Option<&[u8]>,
) -> bool {
    if before.membership != Stored::In {
        return true;
    }
    if !probed.parents.is_empty()
        && before
            .digest
            .is_some_and(|held_digest| !probed.parents.contains(&held_digest))
    {
        return false;
    }
    let Ok(Some(identity)) = read_identity(paths) else {
        return false;
    };
    let Ok(probed_env) = envelope::parse(&probed.envelope) else {
        return false;
    };
    let Ok(seeded) = log_entries::trusted_seeded_log(paths, config, &probed_env) else {
        return false;
    };
    if !matches!(
        log_entries::log_base(seeded.as_ref().map(|(log, _)| log), &probed_env, &identity),
        Ok(base) if base != log_entries::LogBase::Behind
    ) {
        return false;
    }
    // A signed descendant may intentionally remove this reader. Requiring an
    // open locker first would suppress precisely those proven revocations.
    if before
        .digest
        .is_some_and(|held_digest| probed.parents.contains(&held_digest))
    {
        return true;
    }
    let Ok((Some(probed_entries), _)) =
        log_entries::open_entries_preferring_log(paths, config, &probed_env, &identity)
    else {
        return false;
    };
    let Some(held_bytes) = held_bytes else {
        // Sequence alone cannot show that an unparented peer preserves the
        // version already held. Recover its exact body before comparing it.
        return before.digest.is_none();
    };
    let Ok(held_env) = envelope::parse(held_bytes) else {
        return false;
    };
    let Ok((Some(held_entries), _)) =
        log_entries::open_entries_preferring_log(paths, config, &held_env, &identity)
    else {
        return false;
    };
    held_entries.iter().all(|held_entry| {
        let Some(probed_entry) = probed_entries.iter().find(|e| e.name == held_entry.name) else {
            return false;
        };
        match (&held_entry.audience, &probed_entry.audience) {
            (_, Audience::Everyone) => true,
            (Audience::Everyone, Audience::Hosts(_)) => false,
            (Audience::Hosts(held_hosts), Audience::Hosts(probed_hosts)) => {
                held_hosts.iter().all(|h| probed_hosts.contains(h))
            }
        }
    })
}

/// [`sync`], where a subscriber's pass of a vault at a path writes the files of a version it
/// holds again only as [`put_back_due`] rules, by `repaired`.
fn sync_repairing(
    paths: &Paths,
    wait: Option<Duration>,
    repaired: Option<&mut Repaired>,
) -> crate::Result<SyncOutcome> {
    sync_repairing_inner(paths, wait, repaired, &[], true, None, None)
}

#[cfg(test)]
thread_local! {
    /// The negative control of the vault pass's reach: while set, only a member whose vault is at
    /// a URL runs it, as before the exchange stopped signing.
    pub(crate) static URL_ONLY_PASS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// [`sync`] with `extra_peers` as its candidates, mirroring diverged servers where
/// `allow_mirror`; a subscriber's pass hands it the servers its browse found and where to
/// keep what the source itself said (`subscribing`, [`sync_found`]). The records posts of the
/// pass (its records pushes, a closed wrap gap's, a tended epoch's) all end by `posts_by` where
/// one is given; with none, each ends one exchange from when it starts. A mirror's posts keep
/// their own ([`log_entries::mirror_log_to_servers`]).
fn sync_repairing_inner(
    paths: &Paths,
    wait: Option<Duration>,
    mut repaired: Option<&mut Repaired>,
    extra_peers: &[String],
    allow_mirror: bool,
    subscribing: Option<(Found<'_>, &std::cell::Cell<SourceSaid>)>,
    posts_by: Option<Instant>,
) -> crate::Result<SyncOutcome> {
    let mut config = configured(paths)?;
    let mut before = load_state(paths)?;
    let browsed = subscribing.map(|(found, _)| found);
    let selected = (before.membership == Stored::In
        && !before.failed
        && before.pending.is_empty()
        && (source_bound(&config, &before) || log_only_source_bound(&config, &before)))
    .then(|| materialization_selection::current(paths, &before));
    let selection_due = selected.as_ref().is_some_and(|selected| {
        !selected.as_ref().is_ok_and(|current| {
            current
                .as_ref()
                .is_some_and(|current| Some(current.meaning) == before.materialization_selection)
        })
    });
    // A missing or unreadable cached selection needs incoming records/bytes, not an
    // unconditional local apply error before the network has its chance to heal it. Once
    // a usable selection enters canonical apply, all effect/ownership errors still stand.
    if selection_due
        && matches!(&selected, Some(Ok(Some(current))) if current.records == before.records_digest)
    {
        let repaired_locally = apply_until(
            paths,
            config.clone(),
            before.clone(),
            ApplyInput::CachedAtAppliedHeads,
            repaired.as_deref_mut(),
            None,
            true,
        )?;
        if !matches!(repaired_locally, SyncOutcome::Unchanged { .. }) {
            if let Some((_, said)) = subscribing {
                said.set(SourceSaid::Nothing);
            }
            return Ok(repaired_locally);
        }
        // The locked local attempt may have observed a newer applied version or frontier.
        // The ordinary fetch must use a fresh baseline rather than the scheduling reading.
        config = configured(paths)?;
        before = load_state(paths)?;
    }
    if matches!(config.source, Source::Url(_)) {
        let _ = sync_records_inner(paths, None, extra_peers, browsed, posts_by);
    }
    // Every log member's vault pass, a vault at a path included: its records come from Commons
    // exchanges, which sign nothing, so this is where its wrap gaps close and its re-keys mint.
    #[cfg(test)]
    let pass = !URL_ONLY_PASS.with(std::cell::Cell::get) || matches!(config.source, Source::Url(_));
    #[cfg(not(test))]
    let pass = true;
    if pass {
        remove::vault_pass(paths, posts_by);
    }
    let pending_records = log_entries::current_records_digest(paths, &config, &before)
        .is_some_and(|digest| Some(digest) != before.records_digest)
        || selection_due;
    let waited_on_primary =
        matches!(&config.source, Source::Url(_)) && wait.is_some() && !pending_records;
    let outcome = network_refresh::run(paths, &config, |refresh| {
        let subscriber = subscriber_recipient(paths);
        let attempt =
            network_observation::Attempt::start(paths, &config, VaultNetworkSource::ConfiguredUrl);
        let after = if waited_on_primary {
            long_poll_after(&config, &before)
        } else {
            None
        };
        let http204_request =
            http204_observation::Request::start(paths, &config, after, subscriber.as_deref());
        let asked = Instant::now();
        let observed = match (&config.source, wait) {
            (Source::Url(_), Some(wait)) if !pending_records => transport::fetch_as_observed(
                &config.source,
                after,
                wait.saturating_add(Duration::from_secs(10)),
                subscriber.as_deref(),
            ),
            _ => transport::fetch_as_observed(
                &config.source,
                None,
                Duration::from_secs(30),
                subscriber.as_deref(),
            ),
        };
        refresh.observed(&observed);
        let http204 = observed.as_ref().ok().and_then(|observed| observed.http204);
        let mut primary_result = observed.map(|observed| observed.fetched);
        let mut network_body = primary_result
            .as_ref()
            .ok()
            .and_then(|fetched| attempt.and_then(|attempt| attempt.received(fetched)));
        if let Some((_, said)) = subscribing {
            said.set(SourceSaid::of(primary_result.is_ok(), asked.elapsed()));
        }
        if let Some(request) = &http204_request {
            http204_observation::retain(paths, &config, request, http204.as_ref());
        }
        if waited_on_primary && matches!(primary_result, Ok(Fetched::Body(_))) {
            let _ = sync_records_inner(paths, None, extra_peers, browsed, posts_by);
        }
        // The source's body is news only as far as it verifies under the pin (the servers found are
        // read only under one): a body that claims a newer seq and does not verify (anyone on the
        // LAN can answer at the source's name or address) must not keep the servers found unread.
        let primary_advanced = match (&primary_result, config.signer.as_ref()) {
            (Ok(Fetched::Body(bytes)), Some(pin)) => {
                envelope::verify_for_reader(bytes, &config.name, pin, &config.followed, None)
                    .is_ok_and(|(envelope, pinned, _)| {
                        peer_selection::primary_advances(&before, &envelope, pinned)
                    })
            }
            _ => false,
        };
        // Select only when a probe, mirror or recovery needs peers, and reuse the same batch
        // throughout this pass. A primary advancing by itself must not skip a failover batch.
        let peers = std::cell::OnceCell::new();
        let peer_urls = || {
            peers.get_or_init(|| match (&config.source, config.signer.as_ref()) {
                (Source::Url(primary_url), Some(_)) => collect_sync_peer_urls(
                    paths,
                    &config,
                    primary_url,
                    extra_peers,
                    browsed,
                    PeerUse::Envelope,
                ),
                _ => Vec::new(),
            })
        };
        // The mirrors this Mac lists are read beside the servers and never written to: an
        // admin's heal and its log go to the servers alone.
        let read_only = std::cell::OnceCell::new();
        let read_only =
            || read_only.get_or_init(|| mirrors::ReadOnly::of(paths, &config, extra_peers));
        let mut max_peer_seq: Option<u64> = None;
        let is_local_admin = allow_mirror
            && admin_text_here(paths, &config, &before)
                .ok()
                .flatten()
                .is_some();
        if let (Source::Url(primary_url), Some(pin)) = (&config.source, config.signer.as_ref())
            && (!primary_advanced || (is_local_admin && wait.is_none()))
        {
            let peers = peer_urls();
            if !peers.is_empty() {
                let found = probe_peers_for_sync(
                    paths,
                    peers,
                    &config,
                    pin,
                    servers_within(PROBE_DEADLINE),
                    subscriber.as_deref(),
                    read_only(),
                );
                max_peer_seq = found.iter().map(|p| p.seq).max();
                if is_local_admin && !found.is_empty() {
                    // A mirror takes no admin publish, so its head is none an admin's heal can
                    // mend: only the servers' heads count.
                    let mut observed: Vec<HeadLineage> = found
                        .iter()
                        .filter(|p| !read_only().holds(&p.url))
                        .map(|p| (p.seq, p.digest, p.parents.clone()))
                        .collect();
                    match &primary_result {
                        Ok(Fetched::Body(bytes)) => {
                            if let Ok((env, _, _)) = envelope::verify_for_reader(
                                bytes,
                                &config.name,
                                pin,
                                &config.followed,
                                None,
                            ) {
                                observed.push((env.seq, env.digest(), env.parents));
                            }
                        }
                        Ok(Fetched::NoNewer(Some(seq)))
                            if *seq == before.seq && before.digest.is_some() =>
                        {
                            observed.push((*seq, before.digest.unwrap(), Vec::new()));
                        }
                        _ => {}
                    }
                    if heads_diverged(&observed) {
                        let mirror_urls = read_only().servers(primary_url, peers);
                        if log_entries::mirror_log_to_servers(paths, &mirror_urls)?.is_some() {
                            refresh.delegated();
                            return sync_repairing_inner(
                                paths,
                                None,
                                repaired,
                                extra_peers,
                                false,
                                subscribing,
                                posts_by,
                            );
                        }
                    }
                }
                if let Some(peer) = peer_selection::select(
                    paths,
                    &config,
                    &before,
                    &primary_result,
                    found,
                    true,
                    None,
                )? {
                    network_body = peer.network_body;
                    primary_result = Ok(Fetched::Body(peer.envelope));
                }
            }
        }
        // After a heal the higher-seq side's version takes every server over, so no two heads differ
        // for `heads_diverged` to flag, and a subscriber whose source advanced probes no peer: an
        // admin whose log moved since the version it applied mirrors the merged log, which publishes
        // only where the log changes the version, so the lower-seq side's edits reach the envelope a
        // 0.145 Mac reads with nobody acting.
        if is_local_admin
            && let Source::Url(primary_url) = &config.source
            && log_entries::current_records_digest(paths, &config, &before)
                .is_some_and(|digest| Some(digest) != before.records_digest)
        {
            let mirror_urls = read_only().servers(primary_url, peer_urls());
            if let Ok(Some(_)) = log_entries::mirror_log_to_servers(paths, &mirror_urls) {
                refresh.delegated();
                return sync_repairing_inner(
                    paths,
                    None,
                    repaired,
                    extra_peers,
                    false,
                    subscribing,
                    posts_by,
                );
            }
        }
        // An approval this Mac made on the lower-seq side of a partition, which every server
        // dropped at the heal for the other side's higher version, comes back by this Mac's own
        // mirror (`own_head`). Records that healed before the version did leave the log unmoved,
        // so the mirror above never ran for it. Its errors leave the pass to apply as before.
        let cached;
        let served: Option<&[u8]> = match &primary_result {
            Ok(Fetched::Body(bytes)) => Some(bytes.as_slice()),
            Ok(Fetched::NoNewer(_)) => {
                cached = held_copy(paths, &config, &before);
                cached.as_deref()
            }
            _ => None,
        };
        if is_local_admin
            && let Source::Url(primary_url) = &config.source
            && let Some(served) = served
            && own_head::due(
                paths,
                &config,
                served,
                log_entries::current_records_digest(paths, &config, &before).as_ref(),
            )
        {
            let mirror_urls = read_only().servers(primary_url, peer_urls());
            if let Ok(Some(_)) = log_entries::mirror_log_to_servers(paths, &mirror_urls) {
                refresh.delegated();
                return sync_repairing_inner(
                    paths,
                    None,
                    repaired,
                    extra_peers,
                    false,
                    subscribing,
                    posts_by,
                );
            }
            // A carry that could not complete (a head whose records no server has, every server
            // refusing) waits until its own version, the served one or the records move.
            own_head::tried(
                paths,
                served,
                log_entries::current_records_digest(paths, &config, &before).as_ref(),
            );
        }
        if network_body.is_some() {
            refresh.peer_response();
        }
        match apply_response_until(
            paths,
            config.clone(),
            before,
            primary_result.map(|fetched| network_observation::Received {
                fetched,
                body: network_body,
            }),
            repaired.as_deref_mut(),
            None,
            wait.is_none(),
        ) {
            Ok(outcome) => Ok(outcome),
            Err(err)
                if err.kind() == crate::ErrorKind::NotStored
                    && let (Source::Url(primary_url), Some(pin)) =
                        (&config.source, config.signer.as_ref())
                    && !peer_urls().is_empty() =>
            {
                let peers = peer_urls();
                let current = load_state(paths)?;
                if allow_mirror
                    && admin_text_here(paths, &config, &current)
                        .ok()
                        .flatten()
                        .is_some()
                {
                    let mirror_urls = read_only().servers(primary_url, peers);
                    if log_entries::mirror_log_to_servers(paths, &mirror_urls)?.is_some() {
                        refresh.delegated();
                        return sync_repairing_inner(
                            paths,
                            None,
                            repaired,
                            extra_peers,
                            false,
                            subscribing,
                            posts_by,
                        );
                    }
                }
                let verified_peer_seq = max_peer_seq.or_else(|| {
                    probe_peers_for_sync(
                        paths,
                        peers,
                        &config,
                        pin,
                        servers_within(PROBE_DEADLINE),
                        subscriber.as_deref(),
                        read_only(),
                    )
                    .into_iter()
                    .map(|p| p.seq)
                    .max()
                });
                if verified_peer_seq.is_some_and(|seq| seq >= current.seq) {
                    return Ok(SyncOutcome::Unchanged { seq: current.seq });
                }
                Err(err)
            }
            Err(err) => Err(err),
        }
    });
    if matches!(
        outcome,
        Ok(SyncOutcome::Applied { .. } | SyncOutcome::Unchanged { .. })
    ) {
        // This normal sync is authorized to verify its registered entry. Holder
        // probes never take this path or open mailbox plaintext before custody.
        let _ = mailbox_index::backfill_after_sync(paths);
    }
    outcome
}

/// [`sync`] as one try of a bounded wait (an approval's): a long-poll whose whole request ends
/// within `total` ([`transport::fetch_within`]), as the server holds one for its own time, not
/// the asker's; one that runs out of `total` heard nothing new ([`SyncOutcome::Unchanged`]).
/// A vault at a path is read as [`sync`] reads it.
pub fn sync_within(paths: &Paths, total: Duration) -> crate::Result<SyncOutcome> {
    let config = configured(paths)?;
    let before = load_state(paths)?;
    network_refresh::run(paths, &config, |refresh| {
        let subscriber = subscriber_recipient(paths);
        let attempt =
            network_observation::Attempt::start(paths, &config, VaultNetworkSource::ConfiguredUrl);
        let after = long_poll_after(&config, &before);
        let http204_request =
            http204_observation::Request::start(paths, &config, after, subscriber.as_deref());
        let observed = match &config.source {
            Source::Url(url) => {
                match transport::fetch_within_as_observed(url, after, total, subscriber.as_deref())
                {
                    Err(error) if error.kind() == crate::ErrorKind::TimedOut => {
                        return Ok(SyncOutcome::Unchanged { seq: before.seq });
                    }
                    fetched => fetched,
                }
            }
            Source::Path(_) => transport::fetch_as_observed(
                &config.source,
                None,
                Duration::from_secs(30),
                subscriber.as_deref(),
            ),
        };
        refresh.observed(&observed);
        let observed = observed?;
        if let Some(request) = &http204_request {
            http204_observation::retain(paths, &config, request, observed.http204.as_ref());
        }
        let fetched = observed.fetched;
        let body = attempt.and_then(|attempt| attempt.received(&fetched));
        apply_fetched(
            paths,
            config.clone(),
            before,
            network_observation::Received { fetched, body },
        )
    })
}

/// Confirm an admitted join through its selected source within `total`, retaining the same
/// caller authority across the HTTP wait and locked materialization. Broader peer repair
/// remains the subscriber's work; this does not grant ordinary maintenance new authority.
/// `current` must be read-only and acquire neither the vault lock nor a lock that waits for it.
///
/// # Errors
/// As [`sync_within`], or the caller's authority is withdrawn before a subsequent effect.
pub fn sync_within_admitted(
    paths: &Paths,
    total: Duration,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<SyncOutcome> {
    sync_join_approval(paths, Some(total), ExpectedConfig::AdmittedAny(current))
}

/// Fetch this admitted setup's selected source immediately and materialize under its current
/// authority. No long-poll or independent maintenance pass borrows the setup's permission.
/// `current` has the same read-only, lock-free contract as [`sync_within_admitted`].
///
/// # Errors
/// As [`sync_within_admitted`].
pub fn sync_admitted(
    paths: &Paths,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<SyncOutcome> {
    current()?;
    let config = configured(paths)?;
    let before = load_state(paths)?;
    let attempt =
        network_observation::Attempt::start(paths, &config, VaultNetworkSource::ConfiguredUrl);
    let observed = transport::fetch_as_observed(
        &config.source,
        None,
        servers_within(Duration::from_secs(30)),
        subscriber_recipient(paths).as_deref(),
    );
    current()?;
    let fetched = observed?.fetched;
    let body = attempt.and_then(|attempt| attempt.received(&fetched));
    apply_until_admitted(
        paths,
        config,
        before,
        ApplyInput::Response(Ok(network_observation::Received { fetched, body })),
        None,
        None,
        (false, current),
    )
}

/// The seq a long-poll waits past (`?after=`): the one this Mac holds, once it is bound to
/// the source and has nothing of that version left to write.
fn long_poll_after(config: &Config, before: &State) -> Option<u64> {
    (before.membership != Stored::None
        && !pin_gives_way(config)
        && source_bound(config, before)
        && (!matches!(before.membership, Stored::In | Stored::Plain)
            || (!before.failed && before.pending.is_empty())))
    .then_some(before.seq)
}

/// Apply a test's supplied response through the ordinary sync admission and locked readback.
#[cfg(test)]
fn sync_with(paths: &Paths, bytes: Vec<u8>) -> crate::Result<SyncOutcome> {
    let config = configured(paths)?;
    let before = load_state(paths)?;
    apply_fetched(paths, config, before, Fetched::Body(bytes))
}

fn apply_fetched(
    paths: &Paths,
    config: Config,
    before: State,
    fetched: impl Into<network_observation::Received>,
) -> crate::Result<SyncOutcome> {
    apply_fetched_repairing(paths, config, before, fetched, None)
}

/// [`apply_fetched`], where a subscriber's pass of a vault at a path holds `repaired` to rule on
/// writing the files of a version it holds again ([`put_back_due`]).
fn apply_fetched_repairing(
    paths: &Paths,
    config: Config,
    before: State,
    fetched: impl Into<network_observation::Received>,
    repaired: Option<&mut Repaired>,
) -> crate::Result<SyncOutcome> {
    apply_fetched_until(paths, config, before, fetched, repaired, None)
}

fn apply_fetched_until(
    paths: &Paths,
    config: Config,
    before: State,
    fetched: impl Into<network_observation::Received>,
    repaired: Option<&mut Repaired>,
    deadline: Option<Instant>,
) -> crate::Result<SyncOutcome> {
    apply_response_until(
        paths,
        config,
        before,
        Ok(fetched.into()),
        repaired,
        deadline,
        false,
    )
}

/// Cache decisions use the current locked state, never a pre-fetch snapshot.
fn apply_response_until(
    paths: &Paths,
    config: Config,
    before: State,
    received: crate::Result<network_observation::Received>,
    repaired: Option<&mut Repaired>,
    deadline: Option<Instant>,
    repair_requested: bool,
) -> crate::Result<SyncOutcome> {
    apply_until(
        paths,
        config,
        before,
        ApplyInput::Response(received),
        repaired,
        deadline,
        repair_requested,
    )
}

enum ApplyInput {
    Response(crate::Result<network_observation::Received>),
    Cached,
    /// Reinterpret the applied version only while its actual selected frontier still holds.
    CachedAtAppliedHeads,
}

fn apply_until(
    paths: &Paths,
    config: Config,
    before: State,
    input: ApplyInput,
    repaired: Option<&mut Repaired>,
    deadline: Option<Instant>,
    repair_requested: bool,
) -> crate::Result<SyncOutcome> {
    apply_until_admitted(
        paths,
        config,
        before,
        input,
        repaired,
        deadline,
        (repair_requested, &|| Ok(())),
    )
}

fn apply_until_admitted(
    paths: &Paths,
    mut config: Config,
    before: State,
    input: ApplyInput,
    repaired: Option<&mut Repaired>,
    deadline: Option<Instant>,
    (repair_requested, current): (bool, &(dyn Fn() -> crate::Result<()> + Sync)),
) -> crate::Result<SyncOutcome> {
    let lock = bounded::take(paths, deadline)?;
    current()?;
    if self::config(paths)?.as_ref() != Some(&config) {
        return Ok(SyncOutcome::Unchanged { seq: before.seq });
    }
    let requested_config = config.clone();
    let mut state = load_state(paths)?;
    let at_applied_heads = matches!(&input, ApplyInput::CachedAtAppliedHeads);
    if at_applied_heads {
        let version = |state: &State| {
            (
                state.seq,
                state.digest,
                state.records_digest,
                state.source_identity,
                state.membership,
            )
        };
        if version(&state) != version(&before)
            || state.membership != Stored::In
            || state.failed
            || !state.pending.is_empty()
            || !materialization_selection::current(paths, &state)
                .ok()
                .flatten()
                .is_some_and(|current| current.records == state.records_digest)
        {
            // No effect has started. New records or an unavailable cached selection belong
            // to the normal fetch, not to a local replay that would hide its newer envelope.
            return Ok(SyncOutcome::Unchanged { seq: state.seq });
        }
    }
    if matches!(
        &input,
        ApplyInput::Cached | ApplyInput::CachedAtAppliedHeads | ApplyInput::Response(Err(_))
    ) && let Some(configured) = peer_selection::configured_log_recovery(paths, &config, &state)
    {
        if at_applied_heads
            && Some(log_entries::records_heads_digest(&configured.log)) != state.records_digest
        {
            return Ok(SyncOutcome::Unchanged { seq: state.seq });
        }
        current()?;
        return apply_configured_log(paths, &config, &mut state, configured);
    }
    let network_observation::Received {
        mut fetched,
        mut body,
    } = match input {
        ApplyInput::Response(Ok(received)) => received,
        ApplyInput::Cached | ApplyInput::CachedAtAppliedHeads => {
            let Some(copy) = peer_selection::cached_materialization(
                paths,
                &config,
                &state,
                repair_requested || long_poll_after(&config, &state).is_none(),
                false,
                None,
                &mut None,
            )
            .or_else(|| peer_selection::cached_log_progress(paths, &config, &state, &mut None)) else {
                return Ok(SyncOutcome::Unchanged { seq: state.seq });
            };
            Fetched::Body(copy).into()
        }
        ApplyInput::Response(Err(error)) => {
            // Local repair and downloaded log progress do not require a live
            // endpoint. Only the current locked cache can supply either, and
            // neither is evidence of a successful network response.
            if let Some(copy) = peer_selection::cached_materialization(
                paths,
                &config,
                &state,
                repair_requested || long_poll_after(&config, &state).is_none(),
                false,
                None,
                &mut None,
            ) {
                Fetched::Body(copy).into()
            } else if let Some(copy) =
                peer_selection::cached_log_progress(paths, &config, &state, &mut None)
            {
                Fetched::Body(copy).into()
            } else {
                return Err(error);
            }
        }
    };
    if match &fetched {
        Fetched::Missing | Fetched::NoNewer(_) => true,
        Fetched::Body(bytes) => envelope::parse(bytes)
            .is_ok_and(|body| body.name == config.name && body.seq < state.seq),
    } && let Some(copy) = peer_selection::cached_log_progress(paths, &config, &state, &mut body)
    {
        // Local record progress must not depend on repairing an unavailable or older
        // envelope server. The normal verifier below still checks the cached head and log.
        fetched = Fetched::Body(copy);
        body = None;
    }
    let mut bytes = match fetched {
        Fetched::NoNewer(served) => {
            let heard = heard_now(state.heard);
            if heard != state.heard {
                state.heard = heard;
                current()?;
                save_state(paths, &state)?;
            }
            let older = served.filter(|served| {
                *served < state.seq && (state.seq, state.digest) == (before.seq, before.digest)
            });
            if deadline.is_none()
                && let Some((served, copy)) = older
                    .and_then(|served| Some((served, publishable_copy(paths, &config, &state)?)))
            {
                drop(lock);
                current()?;
                transport::publish(&config.source, &copy).map_err(|error| {
                    Error::of_kind(
                        error.kind(),
                        format!(
                            "{} holds seq {served} of {}, older than this machine's seq {}, and putting this machine's copy back failed: {error}",
                            place(&config.source),
                            config.name,
                            state.seq
                        ),
                    )
                })?;
                return Ok(SyncOutcome::PutBack {
                    seq: state.seq,
                    served: Some(served),
                });
            }
            if let Some(copy) = peer_selection::cached_materialization(
                paths,
                &config,
                &state,
                repair_requested || long_poll_after(&config, &state).is_none(),
                deadline.is_none() && (state.seq, state.digest) == (before.seq, before.digest),
                served,
                &mut body,
            ) {
                copy
            } else if let Some(copy) =
                peer_selection::cached_log_progress(paths, &config, &state, &mut body)
            {
                // A log admission can activate a pending reader. Reading that
                // progress does not require permission to publish this body.
                body = None;
                copy
            } else if state.digest.is_none()
                && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing
                && let Some(configured) = log_entries::trusted_configured_log(paths, &config)?
            {
                current()?;
                return apply_configured_log(paths, &config, &mut state, configured);
            } else {
                return Ok(SyncOutcome::Unchanged { seq: state.seq });
            }
        }
        Fetched::Missing => {
            if state.digest.is_none()
                && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing
                && let Some(configured) = log_entries::trusted_configured_log(paths, &config)?
            {
                current()?;
                return apply_configured_log(paths, &config, &mut state, configured);
            }
            let copy = match config.source {
                Source::Url(_) => publishable_copy(paths, &config, &state),
                Source::Path(_) => None,
            };
            let Some(copy) = copy else {
                return Err(missing(&config));
            };
            if deadline.is_some() {
                return Ok(SyncOutcome::Unchanged { seq: state.seq });
            }
            drop(lock);
            current()?;
            transport::publish(&config.source, &copy).map_err(|error| {
                Error::of_kind(
                    error.kind(),
                    format!(
                        "{} holds no vault named {}, and putting this machine's copy back failed: {error}",
                        place(&config.source),
                        config.name
                    ),
                )
            })?;
            return Ok(SyncOutcome::PutBack {
                seq: state.seq,
                served: None,
            });
        }
        Fetched::Body(bytes) => bytes,
    };
    if config.signer.is_some()
        && state.seq > before.seq
        && envelope::parse(&bytes)
            .is_ok_and(|parsed| parsed.name == config.name && parsed.seq < state.seq)
    {
        // An in-flight response can be overtaken by another local apply. Its
        // bytes supply no current-version evidence; recover only from the
        // current cache while still honoring this caller's repair policy.
        if let Some(copy) = peer_selection::cached_materialization(
            paths,
            &config,
            &state,
            repair_requested || long_poll_after(&config, &state).is_none(),
            false,
            None,
            &mut body,
        ) {
            bytes = copy;
        } else if let Some(copy) =
            peer_selection::cached_log_progress(paths, &config, &state, &mut body)
        {
            bytes = copy;
            body = None;
        } else {
            return Ok(SyncOutcome::Unchanged { seq: state.seq });
        }
    }
    let Some(pin) = config.signer else {
        let Source::Path(path) = &config.source else {
            return Err(Error::new("source.toml: a URL source needs a signer"));
        };
        if is_envelope(&bytes) {
            return Err(Error::new(
                "the file at the path is now a signed vault: run aterm secrets join again to pin it",
            ));
        }
        let entries = plain_entries(&config, path)?;
        // The text is the version: the same text with the same files not as written is
        // put back once, not every minute.
        let version = (
            sha256(&[source_identity(&config).as_slice(), bytes.as_slice()].concat()),
            0,
        );
        let settled = state.membership == Stored::Plain
            && source_bound(&config, &state)
            && !state.failed
            && state.pending.is_empty()
            && state.own.is_some()
            && state.materialization_selection
                == Some(materialization_selection::Meaning::of(&entries));
        if settled
            && repaired.is_some_and(|repaired| !put_back_due(paths, &state, version, repaired))
        {
            state.heard = heard_now(state.heard);
            current()?;
            save_state(paths, &state)?;
            return Ok(SyncOutcome::Unchanged { seq: 0 });
        }
        current()?;
        let report = apply_entries(paths, &mut state, &entries)?;
        state.membership = Stored::Plain;
        state.failed = apply_failed(&report);
        state.source_identity = (!state.failed).then(|| source_identity(&config));
        state.heard = heard_now(state.heard);
        current()?;
        save_state(paths, &state)?;
        if quiet(&report) {
            return Ok(SyncOutcome::Unchanged { seq: 0 });
        }
        return Ok(SyncOutcome::Applied { seq: 0, report });
    };
    let held = floor(paths, &state, &config.name, &config);
    let verified =
        envelope::verify_for_reader(&bytes, &config.name, &pin, &config.followed, held.as_ref());
    let (envelope, pinned, verdict) = match verified {
        Ok(verified) => verified,
        Err(_) if at_applied_heads => return Ok(SyncOutcome::Unchanged { seq: state.seq }),
        Err(error) => {
            // The source went back to a version older than this Mac's (restored, or put back
            // by a Mac that slept, maybe one signed before a rotation): a pass that did not
            // long-poll hears it as a body, not a `204` naming the older seq, and puts this
            // Mac's copy back as that one would. Its name and seq are read as the `204`'s are,
            // unverified: what is put back is this Mac's own copy, verified under its pin
            // ([`held_copy`]), which the server takes only as a newer version of its chain.
            let older = match config.source {
                Source::Url(_) if (state.seq, state.digest) == (before.seq, before.digest) => {
                    envelope::parse(&bytes)
                        .ok()
                        .filter(|served| served.name == config.name && served.seq < state.seq)
                        .map(|served| served.seq)
                }
                _ => None,
            };
            let Some((served, copy)) =
                older.and_then(|served| Some((served, publishable_copy(paths, &config, &state)?)))
            else {
                return Err(error);
            };
            if deadline.is_some() {
                return Ok(SyncOutcome::Unchanged { seq: state.seq });
            }
            drop(lock);
            current()?;
            transport::publish(&config.source, &copy).map_err(|failed| {
                Error::of_kind(
                    failed.kind(),
                    format!(
                        "{} holds seq {served} of {}, older than this machine's seq {}, and putting this machine's copy back failed: {failed}",
                        place(&config.source),
                        config.name,
                        state.seq
                    ),
                )
            })?;
            return Ok(SyncOutcome::PutBack {
                seq: state.seq,
                served: Some(served),
            });
        }
    };
    if at_applied_heads && (verdict != Verdict::Unchanged || !matches!(pinned, Pin::Same)) {
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    if !at_applied_heads {
        state.heard = heard_now(state.heard);
    }
    // A server serves the version a publish of this Mac left `[everyone]` writes pending on: they
    // wait for the new key from now, whatever became of that publish's reply. Best effort: a
    // pending set that does not land stays pending for the next read of that version.
    if !at_applied_heads
        && log_entries::waiting_pending(paths)
        && let Ok(Some(identity)) = read_identity(paths)
    {
        current()?;
        let _ = log_entries::land_waiting_held(
            paths,
            &identity,
            &log_entries::stored_versions(&envelope),
        );
    }
    let mut next_config = config.clone();
    if let Pin::Rotated(new) | Pin::Replaced(new) = pinned {
        next_config.signer = Some(new);
        next_config.followed = envelope.reached_from();
    }
    if matches!(pinned, Pin::Rotated(_)) {
        current()?;
        retained_history::acknowledge_for(
            paths,
            &load_records_for(&Reads::new(paths), &next_config)?,
            &next_config,
        )?;
    }
    let seeded_log = if matches!(pinned, Pin::Replaced(_)) {
        log_entries::replacement_seeded_log(paths, &next_config, &envelope)?
    } else {
        log_entries::trusted_seeded_log(paths, &next_config, &envelope)?
    };
    let current_records_digest = seeded_log.as_ref().map(|(_, d)| *d);
    // Record intake has its own custody. Check the source actually retained for apply,
    // not just the earlier scheduling or locked observation, before any effects. All
    // subsequent entry selection uses this owned log; this path never adopts a later draft.
    if at_applied_heads {
        if current_records_digest != state.records_digest {
            return Ok(SyncOutcome::Unchanged { seq: state.seq });
        }
        state.heard = heard_now(state.heard);
    }
    if !matches!(pinned, Pin::Replaced(_))
        && peer_selection::awaiting_applied_log(&state, current_records_digest)
    {
        // A verified envelope can predate values already obtained from the log,
        // even when its envelope sequence advances. Log loss is not an edit.
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    if verdict == Verdict::Unchanged
        && matches!(pinned, Pin::Same)
        && state.records_digest == current_records_digest
        && state.membership == Stored::In
        && source_bound(&config, &state)
        && state.own.is_some()
        && (files_intact(paths, &state)
            || (!state.failed
                && state.pending.is_empty()
                && cache_intact(paths, &state)
                && materialization_selection_current(paths, &state)
                && repaired.is_some_and(|repaired| {
                    !put_back_due(
                        paths,
                        &state,
                        (source_identity(&config), state.seq),
                        repaired,
                    )
                })))
    {
        current()?;
        network_observation::record(
            paths,
            &lock,
            &requested_config,
            &config,
            &mut state,
            &envelope,
            body,
        );
        save_state(paths, &state)?;
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    let identity = read_identity(paths)?
        .ok_or_else(|| Error::new("this machine has no key: run aterm secrets join"))?;
    // A log that lacks records this version's entries were written at is behind it: this Mac
    // keeps what it holds until they arrive (a changed records digest applies the version again)
    // rather than write the older log's values, or the envelope's over its own newer ones.
    let mut base = log_entries::log_base(
        seeded_log.as_ref().map(|(log, _)| log),
        &envelope,
        &identity,
    )?;
    if at_applied_heads && base == log_entries::LogBase::Behind {
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    let mut seeded_log = seeded_log;
    // A version this Mac published whose records it held back when the publish was cut short
    // after a server stored it: taken in here, the next records pass sends them to every server
    // that lacks them, and every other Mac's log then reads the version too.
    current()?;
    if base == log_entries::LogBase::Behind
        && log_entries::adopt_held_draft(paths, &envelope, &identity)?
    {
        seeded_log = log_entries::trusted_seeded_log(paths, &next_config, &envelope)?;
        base = log_entries::log_base(
            seeded_log.as_ref().map(|(log, _)| log),
            &envelope,
            &identity,
        )?;
    }
    if base == log_entries::LogBase::Behind {
        current()?;
        save_state(paths, &state)?;
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    if let Pin::Replaced(_) = pinned {
        current()?;
        retained_history::acknowledge_replacement(
            paths,
            &load_records_for(&Reads::new(paths), &next_config)?,
            &next_config,
        )?;
        state.seq = 0;
        state.digest = None;
        state.records_digest = None;
    }
    if let Pin::Rotated(_) | Pin::Replaced(_) = pinned {
        current()?;
        prepare_materialization(paths, &mut state)?;
        config = next_config;
        save_config(paths, &config)?;
    }
    let (opened, applied_records_digest) = match seeded_log {
        Some((log, digest))
            if log.removes(&identity.recipient())
                || log_entries::recipient_denied_by_roster_or_log(
                    None,
                    &log,
                    &identity.recipient(),
                )
                || log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &identity.recipient(),
                ) =>
        {
            (None, Some(digest))
        }
        Some((log, digest))
            if base == log_entries::LogBase::Covers && log_entries::reads_log(&log, &identity) =>
        {
            (log.open_entries(&identity)?, Some(digest))
        }
        // The version's writer had no seat in the log (or it predates stamps): its lockers are
        // the version, read until a writer whose log holds it stamps one.
        Some((_, digest)) => (open_lockers(&envelope, &identity)?, Some(digest)),
        None => (open_lockers(&envelope, &identity)?, None),
    };
    // The version this Mac's saved state already names is the one its cache was written with,
    // before that state was saved: an apply of it again (the membership log moved under the
    // same envelope, which `cached_log_progress` answers with the cache's own bytes) finds
    // those bytes in place and leaves the file as it is. Any other version, a replaced line
    // (whose state names none) and a cache that does not hold these bytes are written.
    let cached_version = state.digest == Some(envelope.digest());
    state.seq = envelope.seq;
    state.digest = Some(envelope.digest());
    state.records_digest = applied_records_digest;
    current()?;
    if cached_version {
        fsx::write_atomic_if_changed(&paths.cache_file(), envelope.raw(), 0o600)?;
    } else {
        fsx::write_atomic(&paths.cache_file(), envelope.raw(), 0o600)?;
    }
    let Some(entries) = opened else {
        state.membership = match state.membership {
            Stored::In | Stored::Revoked => Stored::Revoked,
            _ => Stored::Pending,
        };
        state.source_identity = Some(source_identity(&config));
        current()?;
        save_state(paths, &state)?;
        return Ok(SyncOutcome::NotIn);
    };
    current()?;
    let report = apply_entries(paths, &mut state, &entries)?;
    if state.membership != Stored::In
        || (handover::holds_allowed(paths) && log_entries::addressed_to(&envelope, &identity)?)
    {
        // In now: the Allow a member handed it has done its work. A Mac the membership log let
        // in holds it until an admin's approver names it in `[machines]`, which its locker shows.
        current()?;
        handover::forget_allowed(paths);
    }
    state.membership = Stored::In;
    state.failed = apply_failed(&report);
    state.source_identity = Some(source_identity(&config));
    current()?;
    network_observation::record(
        paths,
        &lock,
        &requested_config,
        &config,
        &mut state,
        &envelope,
        body,
    );
    save_state(paths, &state)?;
    let _ = mailbox_index::record_applied(paths, &config, &state, &entries);
    Ok(SyncOutcome::Applied {
        seq: envelope.seq,
        report,
    })
}

fn apply_configured_log(
    paths: &Paths,
    config: &Config,
    state: &mut State,
    configured: log_entries::ConfiguredLog,
) -> crate::Result<SyncOutcome> {
    let digest = log_entries::records_heads_digest(&configured.log);
    let identity = read_identity(paths)?
        .ok_or_else(|| Error::new("this machine has no key: run aterm secrets join"))?;
    let opened = log_entries::configured_entries(&configured.log, &identity)?;
    if state.records_digest == Some(digest)
        && state.membership == Stored::In
        && log_only_source_bound(config, state)
        && state.own.is_some()
        && opened.as_ref().is_some_and(|entries| {
            state.materialization_selection == Some(materialization_selection::Meaning::of(entries))
        })
        && local_files_intact(paths, state)
    {
        save_state(paths, state)?;
        return Ok(SyncOutcome::Unchanged { seq: state.seq });
    }
    state.records_digest = Some(digest);
    let Some(entries) = opened else {
        state.membership = match state.membership {
            Stored::In | Stored::Revoked => Stored::Revoked,
            _ => Stored::Pending,
        };
        state.source_identity = Some(source_identity(config));
        save_state(paths, state)?;
        return Ok(SyncOutcome::NotIn);
    };
    let report = apply_entries(paths, state, &entries)?;
    state.membership = Stored::In;
    state.failed = apply_failed(&report);
    state.source_identity = Some(source_identity(config));
    save_state(paths, state)?;
    let _ = mailbox_index::record_applied(paths, config, state, &entries);
    Ok(SyncOutcome::Applied {
        seq: state.seq,
        report,
    })
}

/// Long-poll the source to apply new versions and run the admin approver loop.
pub fn watch(paths: &Paths, log: &(dyn Fn(&str) + Sync)) -> crate::Result<()> {
    beside_the_approver(
        |stop| approver_loop(paths, log, stop),
        || subscriber_loop(paths, log),
    )
}

/// Run `subscriber` beside `approver`, on a thread of its own, and set the stop `approver` is given
/// however `subscriber` ends, a panic included: the scope joins the approver before it returns or
/// re-raises, so a stop left unset would hold `watch` for good.
fn beside_the_approver<R>(
    approver: impl FnOnce(&aterm_uds::stop::Stop) + Send,
    subscriber: impl FnOnce() -> R,
) -> R {
    /// Sets its stop when dropped, the unwinding of a panic included.
    struct SetOnDrop<'a>(&'a aterm_uds::stop::Stop);
    impl Drop for SetOnDrop<'_> {
        fn drop(&mut self) {
            self.0.set();
        }
    }
    let stop = aterm_uds::stop::Stop::new();
    std::thread::scope(|scope| {
        scope.spawn(|| approver(&stop));
        let _ending = SetOnDrop(&stop);
        subscriber()
    })
}

/// Sleep `total`, ending at once when `stop` is set, or early once `moved` says so (looked at
/// every `look`).
fn nap_unless(
    stop: &aterm_uds::stop::Stop,
    total: Duration,
    look: Duration,
    moved: &dyn Fn() -> bool,
) {
    let deadline = Instant::now() + total.min(Duration::from_secs(365 * 24 * 60 * 60));
    while !stop.is_set() && !moved() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stop.sleep(left.min(look)) {
            return;
        }
    }
}

/// How often a subscriber, or the approver beside it, that `[swarm] enabled = false` holds looks
/// at `aterm.toml` again: one `stat` ([`crate::swarm::Switch`]).
const SWARM_LOOK: Duration = Duration::from_secs(1);

/// Whether `[swarm] enabled` lets `who` (this subscriber, the approver beside it) work now
/// (`switch`, [`crate::swarm::Switch`]; `docs/DESIGN-alab-swarm-2026-10-27.md` §9: the switch
/// sets `aterm secrets agent off` among the rest). An off switch holds it, and so does one that
/// cannot be read, as it holds all automatic vault work (`aterm init`'s Secrets stage, the
/// subscriber's mend). Each hold, and its end, is said once in `log` (`held` keeps whether one
/// stands).
fn swarm_lets(
    switch: &mut crate::swarm::Switch,
    held: &mut bool,
    who: &str,
    log: &dyn Fn(&str),
) -> bool {
    match switch.enabled() {
        Ok(true) => {
            if std::mem::take(held) {
                log(&format!(
                    "sharing is on again ([swarm] enabled): {who} works again"
                ));
            }
            true
        }
        off => {
            if !std::mem::replace(held, true) {
                log(&match off {
                    Ok(_) => format!(
                        "sharing is off ([swarm] enabled = false): {who} asks, reads and applies \
                         nothing until it is on"
                    ),
                    Err(error) => format!(
                        "cannot read [swarm] in aterm.toml ({error}): {who} asks, reads and \
                         applies nothing until it reads"
                    ),
                });
            }
            false
        }
    }
}

/// How often an approver's wait for a version this Mac applies looks at its seq
/// ([`approver_nap`]).
const NAP_LOOK: Duration = Duration::from_millis(100);
/// The longest approver nap that watches this Mac's seq ([`approver_nap`]).
const WATCHED_NAP: Duration = Duration::from_secs(2);

/// An approver's nap of `pause`. A short one, the wait for this Mac's subscriber to apply a
/// version the queue moved with, ends once this Mac's seq (`seq`) moves, looked at every
/// [`NAP_LOOK`]: that version may admit a Mac, or end one's asking. A longer one is an approver
/// resting (this Mac is no admin, the usual case on every member Mac, or a pass failed): it reads
/// nothing and sleeps on `stop` in one wait, which its setting ends at once (it once woke each
/// second to look at a flag set only when the subscriber ends).
fn approver_nap(stop: &aterm_uds::stop::Stop, pause: Duration, seq: &dyn Fn() -> Option<u64>) {
    if pause > WATCHED_NAP {
        stop.sleep(pause);
        return;
    }
    let held = seq();
    nap_unless(stop, pause, NAP_LOOK, &|| seq() != held);
}

const FIRST_RETRY: Duration = Duration::from_secs(5);
const LONGEST_RETRY: Duration = Duration::from_secs(300);

const LONGEST_ERROR_RETRY: Duration = Duration::from_secs(60);
/// How often a wait looks at the network where the kernel tells it of no change
/// ([`crate::network::RouteChanges`]).
const MOVED_STEP: Duration = Duration::from_secs(5);

/// Wait `total` unless the network moves first (`moved`, looked at after each `wait`, which
/// waits at most the time it is given and says how long it took): whether it moved.
fn nap_unless_moved(
    total: Duration,
    moved: &mut dyn FnMut() -> bool,
    wait: &mut dyn FnMut(Duration) -> Duration,
) -> bool {
    let mut left = total;
    while !left.is_zero() {
        left = left.saturating_sub(wait(left));
        if moved() {
            return true;
        }
    }
    false
}

/// [`nap_unless_moved`]'s wait on `hearing`'s bell: until a source rings it, the kernel's word of
/// a change to the routes, addresses or links included where `hearing` hears them (else a look
/// every [`MOVED_STEP`]). It keeps in `woke` a ring that ended it, and says how long it took.
fn wait_for_a_ring(
    hearing: &Hearing<'_>,
    woke: &std::cell::Cell<Option<crate::wake::Wake>>,
    most: Duration,
) -> Duration {
    let started = Instant::now();
    let step = if hearing.routes {
        most
    } else {
        most.min(MOVED_STEP)
    };
    match hearing.bell.nap(step) {
        crate::wake::Wake::Timer => {}
        why => woke.set(Some(why)),
    }
    started.elapsed()
}

/// How long a subscriber away from the ALab LAN waits after a pass that failed: the vault's
/// source and its servers answer on the ALab LAN, so asking every minute only spends dials; a
/// new network or a vault server heard ends the wait at once ([`nap_after_failing`]).
const AWAY_ERROR_RETRY: Duration = Duration::from_secs(15 * 60);

/// The wait after a pass that failed, from the last (`backoff`): 5 s once this Mac's network
/// `moved`; [`AWAY_ERROR_RETRY`] while `away` from the ALab LAN (`aterm_netplace::routes::paced`);
/// else twice the last, up to [`LONGEST_ERROR_RETRY`].
fn retry_after(backoff: Duration, moved: bool, away: bool) -> Duration {
    if moved {
        Duration::from_secs(5)
    } else if away {
        AWAY_ERROR_RETRY
    } else {
        backoff.saturating_mul(2).min(LONGEST_ERROR_RETRY)
    }
}

/// The subscriber's words for a pass that failed with `text`, `away` from the ALab LAN or not.
fn failed_line(text: &str, away: bool) -> String {
    if away {
        format!(
            "{text}; this Mac is off the ALab LAN, so it asks again every {} min, at once on a \
             new network or a vault server heard",
            AWAY_ERROR_RETRY.as_secs() / 60
        )
    } else {
        format!(
            "{text}; retrying with backoff up to 1 min, at once on a new network or a vault server heard"
        )
    }
}

/// How one subscriber pass went, beyond its outcome ([`pause_after`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pass {
    /// What its source itself said.
    source: SourceSaid,
    /// Whether it moved this Mac's seq (a version applied, or one that leaves it out).
    moved: bool,
}

/// What a subscriber pass's source itself said, whatever the servers found gave
/// ([`sync_found`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceSaid {
    /// It answered after at least [`HELD`]: a long-poll it held.
    Held,
    /// It answered sooner.
    AtOnce,
    /// It did not answer, however long that took (a `.local` name nobody answers fails only
    /// once its lookup gives up): what the pass read came from the servers found, if any.
    Nothing,
}

impl SourceSaid {
    /// What a source said that answered (`answered`), or not, after `took`.
    fn of(answered: bool, took: Duration) -> Self {
        match (answered, took >= HELD) {
            (false, _) => Self::Nothing,
            (true, true) => Self::Held,
            (true, false) => Self::AtOnce,
        }
    }
}

/// A source that answered after at least this long held a long-poll, not one it answered at
/// once ([`SourceSaid`]).
const HELD: Duration = Duration::from_secs(1);

/// Whether a pass that went well is paced as a failing one: its source said nothing and the
/// servers found held nothing new (F5's failover while the source is away). It is asked again on
/// the error backoff, a vault server heard or a route change ending the wait early, not at once
/// and not every few seconds.
fn failed_over_idle(pass: Pass) -> bool {
    pass.source == SourceSaid::Nothing && !pass.moved
}

/// A local apply can need convergence while the source is still unavailable.
/// Keep its recovery trigger for the first pass that actually hears the source.
fn converge_after_pass(
    due: &mut crate::converge::Due,
    outcome: &SyncOutcome,
    now_ms: u64,
    routes: crate::converge::Routes,
    last: crate::converge::Last,
    source: SourceSaid,
) -> bool {
    if source == SourceSaid::Nothing {
        due.after_unreachable_pass(outcome, now_ms, routes, last)
    } else {
        due.after_pass(outcome, now_ms, routes, last)
    }
}

/// The pause after a pass that ended with `outcome`, from its start. A pass that moved this
/// Mac's seq, or whose source held it, is followed by the next at once: that one is held until
/// something changes. Only one answered at once with nothing new pauses, so a source that
/// holds nothing is not asked in a loop; one whose source said nothing is paced as a failing
/// pass besides ([`failed_over_idle`]).
fn pause_after(
    source: &Source,
    outcome: &SyncOutcome,
    failing: &mut Duration,
    pass: Pass,
) -> Duration {
    let pause = match (source, outcome) {
        (Source::Path(_), _) => Duration::from_secs(60),
        (_, SyncOutcome::Applied { report, .. }) if report.failed.is_empty() => Duration::ZERO,
        (_, SyncOutcome::Applied { .. } | SyncOutcome::PutBack { served: None, .. }) => {
            let pause = *failing;
            *failing = failing.saturating_mul(2).min(LONGEST_RETRY);
            return pause;
        }
        _ if pass.moved || pass.source == SourceSaid::Held => Duration::ZERO,
        (_, SyncOutcome::NotIn) => Duration::from_secs(30),
        (_, SyncOutcome::Unchanged { .. } | SyncOutcome::PutBack { .. }) => Duration::from_secs(10),
    };
    *failing = FIRST_RETRY;
    pause
}

/// The entries a pass refused, named after its counts ([`materialize::Report::refused`]); empty
/// where it refused none.
fn refused_words(report: &Report) -> String {
    if report.refused.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = report
        .refused
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    format!(
        "; not written, as no Mac writes these names as files (an AI CLI's login, a home path aterm does not name, another spelling of a held or consumed name, or two spellings of one file): {}",
        names.join(", ")
    )
}

fn pass_line(
    paths: &Paths,
    config: &Config,
    outcome: &SyncOutcome,
    repeated: &mut String,
) -> Option<String> {
    let (line, again) = match outcome {
        SyncOutcome::Applied { seq, report } => (
            Some(format!(
                "applied seq {seq}: {} written, {} unchanged, {} removed, {} moved aside, {} failed{}",
                report.written.len(),
                report.unchanged.len(),
                report.removed.len(),
                report.moved_aside.len(),
                report.failed.len(),
                refused_words(report)
            )),
            !report.failed.is_empty(),
        ),
        SyncOutcome::Unchanged { .. } => (None, false),
        SyncOutcome::PutBack {
            seq,
            served: Some(served),
        } => (
            Some(format!(
                "{} held seq {served} of {}, older than this machine's seq {seq}: put this machine's copy back",
                place(&config.source),
                config.name
            )),
            false,
        ),
        SyncOutcome::PutBack { seq, served: None } => (
            Some(format!(
                "{} held no vault named {}: put this machine's copy back (seq {seq})",
                place(&config.source),
                config.name
            )),
            true,
        ),
        SyncOutcome::NotIn => (
            load_state(paths)
                .is_ok_and(|s| s.membership == Stored::Revoked)
                .then(|| "this machine is no longer in the vault".to_string()),
            false,
        ),
    };
    let last = std::mem::take(repeated);
    if let (true, Some(line)) = (again, &line) {
        repeated.clone_from(line);
    }
    line.filter(|line| *line != last)
}

/// The last repair a subscriber made: of which version (source and seq, or for a plain file at a
/// path the digest of its source and text, and 0), and the files that were not as written then.
type Repaired = Option<(([u8; 32], u64), BTreeSet<String>)>;

/// Re-acknowledge the raw, verified membership log when the applied digest alone prevents any
/// read. The explicit repair retains the prior state before changing only that digest, and never
/// accepts an unverified store or rewrites identity, membership, proof or vault contents.
/// Its state-lock wait shares the caller's deadline; a busy writer grants no repair authority.
pub(crate) fn repair_legacy_frontier(paths: &Paths, deadline: Instant) -> crate::Result<bool> {
    let Some(_config) = config(paths)? else {
        return Ok(false);
    };
    let _lock = bounded::take(paths, Some(deadline))?;
    let configured = load_records(paths)
        .map_err(|error| Error::new(format!("verified local membership log failed: {error}")))?;
    let log = configured
        .log()
        .ok_or_else(|| Error::new("the local membership log cannot be verified"))?;
    let digest = log_entries::records_heads_digest(log);
    let mut state = load_state(paths)
        .map_err(|error| Error::new(format!("applied membership state failed: {error}")))?;
    if state.records_digest == Some(digest) {
        return Ok(false);
    }
    let before = fsx::read_private(&paths.state_file(), MAX_STATE)?
        .ok_or_else(|| Error::new("the applied membership state is unavailable"))?;
    aterm_journal::write_private_durable(
        &paths.dir,
        "state.before-frontier-repair",
        before.as_slice(),
    )?;
    state.records_digest = Some(digest);
    save_state(paths, &state)?;
    Ok(true)
}

/// Whether this pass repairs the files a version wrote, rather than long-polling: when they are
/// not as written, or one an entry left this Mac's own is gone ([`unsettled`]), once per
/// version and set of such files. Another file changed since (or one put back that changed
/// again with another) is repaired again; the same set is not, so a file a program on this Mac
/// keeps rewriting is put back once, not every pass.
fn repair_due(paths: &Paths, repaired: &mut Repaired) -> bool {
    let Ok(state) = load_state(paths) else {
        return false;
    };
    let Some(source) = state.source_identity else {
        return false;
    };
    if state.membership != Stored::In || state.failed || !state.pending.is_empty() {
        return false;
    }
    let version = (source, state.seq);
    if !cache_intact(paths, &state) || !materialization_selection_current(paths, &state) {
        *repaired = Some((version, BTreeSet::new()));
        return true;
    }
    // A state with no record of the entries this Mac holds as its own is applied once to
    // write one, as the files are then not known to be settled.
    let changed = unsettled(paths, &state);
    let due = (!changed.is_empty() || state.own.is_none())
        && repaired
            .as_ref()
            .is_none_or(|(last, files)| *last != version || *files != changed);
    if due {
        *repaired = Some((version, changed));
    }
    due
}

/// Whether a subscriber's pass of a vault at a path, which reads it every minute in place of a
/// long-poll, writes the files of `version` (its source and seq, or a plain file's text) again:
/// the first time it meets that version, then as [`repair_due`] rules for a URL, once per set of
/// files not as written ([`unsettled`]), so a file a program on this Mac keeps rewriting is put
/// back once, not every minute.
fn put_back_due(
    paths: &Paths,
    state: &State,
    version: ([u8; 32], u64),
    repaired: &mut Repaired,
) -> bool {
    let changed = unsettled(paths, state);
    let due = match repaired.as_ref() {
        Some((last, files)) if *last == version => !changed.is_empty() && *files != changed,
        _ => true,
    };
    if due {
        *repaired = Some((version, changed));
    }
    due
}

/// Start the converge pass's runner for the subscriber (`crate::converge::start_requested`, `--trigger
/// subscriber`) without waiting for it: the pass runs `aterm fleet sync`'s work among its items,
/// and a dial of the fleet's issuer must never hold up this long-poll (or, on an admin Mac, the
/// approver beside it).
#[cfg(not(test))]
fn start_converge(
    exe: &Path,
    request: crate::converge::RepositoryRequest,
    log: &dyn Fn(&str),
) -> Option<aterm_uds::spawnfd::Child> {
    crate::converge::start_requested(exe, crate::converge::SUBSCRIBER, request)
        .map_err(|error| log(&format!("could not start the converge pass: {error}")))
        .ok()
}

/// A test build's live subscriber starts no runner: the test binary is no aterm. A test hands
/// [`subscriber_loop_until`] a [`Converging`] of its own.
#[cfg(test)]
fn start_converge(
    _exe: &Path,
    _request: crate::converge::RepositoryRequest,
    _log: &dyn Fn(&str),
) -> Option<aterm_uds::spawnfd::Child> {
    None
}

/// How the subscriber's loop starts the converge pass's runner, and reads the routes its due
/// check compares ([`crate::converge::Due`]): [`start_converge`] and
/// [`crate::server::route_sources`], or a test's own.
struct Converging<'a> {
    /// Start the runner of the program at this path, without waiting for it.
    start: &'a (
            dyn Fn(&Path, crate::converge::RepositoryRequest) -> Option<aterm_uds::spawnfd::Child>
                + Sync
        ),
    /// This Mac's routes.
    routes: &'a (dyn Fn() -> crate::converge::Routes + Sync),
}

/// What the subscriber's loop hears of the network (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §3.5.2): the vault servers its standing `_alab-vault._tcp` browse found, which a pass whose
/// source fails or holds nothing newer reads ([`sync_found`]), and the bell its nap after an
/// error waits on, which this process's route changes and that browse ring ([`crate::wake`]).
/// [`subscriber_loop`]'s are this Mac's; a test hands its own to `subscriber_loop_until`.
struct Hearing<'a> {
    found: Found<'a>,
    bell: &'a crate::wake::Bell,
    /// Whether this Mac's route changes ring `bell` ([`crate::wake::hear_routes`]). Where they
    /// do not, the nap looks at the routes every [`MOVED_STEP`].
    routes: bool,
    /// How it asks ALab's members on the LAN to show this Mac's request while it waits to be
    /// let in ([`ask_members_if_due`]): how many heard it.
    ask: AskMembers<'a>,
    /// Shared with the independent route listener: one backoff and one member ask at a time.
    members: std::sync::Mutex<MembersAsked>,
    /// What a round `[swarm] enabled` holds does with the LAN uses that run by themselves between
    /// rounds: the live loop's drops its standing `_alab-vault._tcp` browse
    /// ([`StandingBrowse::stop`]), which the first pass of the ALab vault once sharing is on
    /// starts afresh. A test's does nothing, or counts.
    hold: &'a dyn Fn(),
    /// Whether this Mac is away from the ALab LAN now (`aterm_netplace::routes::paced`): the nap
    /// after a failed pass is then [`AWAY_ERROR_RETRY`] ([`retry_after`]). It gates nothing.
    away: &'a dyn Fn() -> bool,
}

/// How a waiting Mac asks ALab's members on the LAN to show its request (F10's `ask`): how many
/// heard it.
type AskMembers<'a> = &'a dyn Fn(&Paths) -> crate::Result<usize>;

/// A passive, owned native readiness source. Packets only wake an ordinary member
/// ask; this source provides no peer identity, proof or admission authority.
pub trait MemberArrivals: Send {
    /// Refresh attachments after activation, a route change or a broker arrival.
    /// A healthy source is not refreshed on the observer's periodic looks.
    /// # Errors
    /// No permitted attachment could be opened or retained.
    fn refresh(&mut self) -> std::io::Result<()>;
    /// At most eight descriptors, borrowed until the listener's wait ends.
    fn fds(&self) -> Vec<std::os::fd::BorrowedFd<'_>>;
    /// Drain a bounded ready batch, coalescing duplicate untrusted hints.
    /// # Errors
    /// An attached socket could not be read or its custody was withdrawn.
    fn poll(&mut self) -> std::io::Result<bool>;
}

struct MemberCallbacks {
    ask: fn(&Paths) -> crate::Result<usize>,
    arrivals: fn(&Paths) -> std::io::Result<Box<dyn MemberArrivals>>,
}

/// The ask and its passive native readiness source share one registration.
static ASK_MEMBERS: std::sync::OnceLock<MemberCallbacks> = std::sync::OnceLock::new();

/// Ask ALab's members on the LAN with `ask` (`aterm members join`'s, over the fleet port's
/// `ask` op, which aterm-secrets cannot dial itself) while this Mac waits to be let into the
/// ALab vault ([`ask_members_if_due`]). Its owned passive source is retained only after
/// ordinary asking established that wait. The first call wins.
pub fn on_ask_members(
    ask: fn(&Paths) -> crate::Result<usize>,
    arrivals: fn(&Paths) -> std::io::Result<Box<dyn MemberArrivals>>,
) {
    let _ = ASK_MEMBERS.set(MemberCallbacks { ask, arrivals });
}

/// How long a Mac waiting to be let into ALab waits at least before asking its members again,
/// after `asks` asks in a row: 20 seconds, then 1, 2, 4 and 8 minutes, then every 15. An ask goes
/// at the top of a subscriber round or when its existing readiness worker sees that this
/// deadline is due. The worker waits for the deadline itself, told when a round moves it
/// ([`MembersAsked::telling`]), so the vault's roughly 25-second long poll cannot postpone a due
/// ask. A member on the home ALab LAN lets
/// the Mac in on its first ask (`client::readmit`); a member busy with a vault pass, or a
/// connection its listener's watchdog cut, can answer the next. A member keeps one row per
/// asking Mac (`members::desk`), so an ask again shows nothing new there.
fn ask_members_pause(asks: u32) -> Duration {
    const SECONDS: [u64; 6] = [20, 60, 120, 240, 480, 900];
    let at = usize::try_from(asks.saturating_sub(1))
        .map_or(SECONDS.len() - 1, |at| at.min(SECONDS.len() - 1));
    Duration::from_secs(SECONDS[at])
}

/// When the subscriber last asked ALab's members, how many times in a row, on what, and what it
/// last said of it.
struct MembersAsked {
    at: Option<Instant>,
    said: String,
    /// The asks in a row since this Mac began waiting, or last started over
    /// ([`ask_members_pause`]).
    asks: u32,
    /// What the last ask went out on: this Mac's network ([`crate::network::default_route`]), the
    /// last one known (a read that finds none, or a part not read yet, keeps it), and its join
    /// request's time (`State::requested`). Another network, or a request a person's Join posted
    /// again, starts the pauses over.
    on: Option<(Option<String>, u64)>,
    /// How this Mac's network is read: [`crate::network::default_route`], or a test's.
    network: fn() -> Option<String>,
    /// Positive asking custody only, at the exact inputs the ordinary predicate read.
    /// It paces a passive hint source, never a proof or an admission effect.
    waiting: Option<vault_memo::InputStamps>,
    /// Rung when the subscriber's round moved what the readiness worker reads of this asking
    /// ([`Self::telling`]): the worker reads it again at a ring ([`member_routes::Rings::members`]).
    rung: Option<std::sync::Arc<aterm_uds::ready::Doorbell>>,
}

impl Default for MembersAsked {
    fn default() -> Self {
        Self {
            at: None,
            said: String::new(),
            asks: 0,
            on: None,
            network: crate::network::default_route,
            waiting: None,
            rung: None,
        }
    }
}

impl MembersAsked {
    /// Whether an ask went out that the wait still stands on (the readiness worker's asking
    /// begins here).
    fn asking(&self) -> bool {
        self.at.is_some() && self.on.is_some()
    }

    fn ended(&mut self) {
        self.at = None;
        self.asks = 0;
        self.on = None;
        self.waiting = None;
    }

    /// No prior ordinary ask means no native receiver and no private read. While
    /// asking, only changed inputs re-evaluate the same ordinary waiting predicate.
    /// A later intent is first observed by an ordinary subscriber round; reopening
    /// then owes a fresh ask to cover readiness announced before the receiver armed.
    fn still_asking(&mut self, paths: &Paths) -> bool {
        if self.at.is_none() || self.on.is_none() {
            self.waiting = None;
            return false;
        }
        let Some(before) = member_waiting_inputs(paths) else {
            self.waiting = None;
            return false;
        };
        if self.waiting.as_ref() == Some(&before) {
            return true;
        }
        self.waiting = None;
        if member_waiting(paths).is_none() {
            self.ended();
            return false;
        }
        if Some(&before) == member_waiting_inputs(paths).as_ref() {
            self.waiting = Some(before);
            true
        } else {
            false
        }
    }

    /// What the readiness worker reads of this asking: whether it asks, and when its next ask is
    /// due by its pause.
    fn heard(&self) -> (bool, Option<Instant>) {
        (
            self.asking(),
            self.at.map(|at| at + ask_members_pause(self.asks)),
        )
    }

    /// Make `change`, ringing [`Self::rung`] when it moved what the worker reads ([`Self::heard`]):
    /// a wait begun or ended, an ask made, or pauses a person's Join or another network started
    /// over.
    fn telling(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.heard();
        change(self);
        if self.heard() != before
            && let Some(rung) = &self.rung
        {
            rung.ring();
        }
    }

    /// Keep the join request's time this subscriber's own ask again moved it to (`requested`,
    /// read after [`ask_again_if_due`]), so that move is not taken for a person's Join.
    fn asked_again(&mut self, requested: Option<u64>) {
        if let (Some((_, kept)), Some(requested)) = (&mut self.on, requested) {
            *kept = requested;
        }
    }
}

fn member_waiting_inputs(paths: &Paths) -> Option<vault_memo::InputStamps> {
    vault_memo::stamp_inputs(paths, |paths| {
        let mut inputs = vault_memo::inputs(paths)?;
        inputs.extend([
            handover::allowed_file(paths),
            paths.dir.join(alab_join::FILE),
        ]);
        Some(inputs)
    })
}

struct MemberWaiting {
    requested: u64,
    bootstrap: bool,
}

/// The one eligibility predicate for ordinary asking and its passive native hints.
fn member_waiting(paths: &Paths) -> Option<MemberWaiting> {
    let config = self::config(paths)
        .ok()
        .flatten()
        .filter(|config| matches!(config.source, Source::Url(_)) && alab_vault(config));
    let state = load_state(paths).ok();
    let bootstrap = alab_join::pending(paths);
    // A seatless recipient asks with the same keys until a seat binds it.
    let waiting = config.as_ref().is_some_and(|config| {
        state
            .as_ref()
            .is_some_and(|state| state.membership == Stored::Pending)
            && (!holds_allowed(paths)
                || log_entries::trusted_configured_log(paths, config)
                    .is_ok_and(|log| log.is_some()))
    }) || seat_ask::asks_for_seat(paths)
        || bootstrap;
    state.filter(|_| waiting).map(|state| MemberWaiting {
        requested: state.requested,
        bootstrap,
    })
}

/// Ask ALab's members on the LAN to show this Mac's request with `ask` while it waits to be let
/// into the ALab vault at a URL (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.6 step 3: System
/// State's join posts only to the vault's queue, which no member's desk reads): at once as the
/// wait begins, then after each [`ask_members_pause`], and at once again where this Mac moved to
/// another network or a person's Join posted its request again (the subscriber's own hourly ask
/// again, and its reset of a request a pass failed with, are not a person's). Each member keeps a
/// heard request in memory only, so asking again is what shows it on a member that slept, was
/// off the LAN, or restarted its window when this Mac first asked. What it heard is said in `log`
/// when it changes.
///
/// A Mac holding a member's `Allowed` (F10's files) waits on an admin's approver and asks
/// nobody, while its vault has no membership log, so a member's Allow ends the asking; once it
/// holds a trusted one (F12b), any member can let it in by the log (F15), so it asks the members
/// again rather than wait on an approver that may never run.
fn ask_members_if_due(
    paths: &Paths,
    asked: &mut MembersAsked,
    ask: AskMembers<'_>,
    log: &dyn Fn(&str),
) {
    ask_members_if_due_with_native(paths, asked, ask, &alab_join::background_native, log);
}

fn ask_members_if_due_with_native(
    paths: &Paths,
    asked: &mut MembersAsked,
    ask: AskMembers<'_>,
    native: &dyn Fn(&Paths) -> crate::Result<NativeAdmission>,
    log: &dyn Fn(&str),
) {
    let Some(waiting) = member_waiting(paths) else {
        // A wait that begins again starts its pauses over.
        asked.ended();
        return;
    };
    // A read with no default route (the Wi-Fi not joined yet after a wake), or one whose gateway
    // ARP has not resolved, is no move: the last route known stands.
    let network = (asked.network)()
        .filter(|route| crate::network::route_known(route))
        .or_else(|| asked.on.as_ref().and_then(|(was, _)| was.clone()));
    let moved = asked.on.as_ref().is_some_and(|(was, requested)| {
        (network.is_some() && network != *was)
            || (waiting.requested != 0 && waiting.requested != *requested)
    });
    if moved {
        asked.at = None;
        asked.asks = 0;
    }
    if asked
        .at
        .is_some_and(|at| at.elapsed() < ask_members_pause(asked.asks))
    {
        return;
    }
    asked.at = Some(Instant::now());
    asked.asks = asked.asks.saturating_add(1);
    asked.on = Some((network, waiting.requested));
    let said = if waiting.bootstrap {
        match native(paths) {
            Ok(NativeAdmission::Admitted) => "native bootstrap admitted this Mac".to_owned(),
            Ok(NativeAdmission::Uncertain) => {
                "native bootstrap waits for its proof to be confirmed".to_owned()
            }
            Ok(NativeAdmission::None) => {
                "native bootstrap heard no admitting member on this network".to_owned()
            }
            Err(why) => format!("native bootstrap waits: {why}"),
        }
    } else {
        match ask(paths) {
            Ok(0) => {
                "asked ALab's members to let this Mac in: none answers on this network".to_owned()
            }
            Ok(heard) => format!(
                "asked ALab's members to let this Mac in: {heard} answered; a member on this Mac's home ALab LAN lets it in by itself"
            ),
            Err(error) => format!("asking ALab's members to let this Mac in failed: {error}"),
        }
    };
    if said != asked.said {
        log(&said);
        asked.said = said;
    }
}

/// The subscriber's projection as its readiness worker serves it.
struct Projecting<'a> {
    projection: &'a projection::Projection<'a>,
    bell: &'a crate::wake::Bell,
    /// When the inputs' upkeep is next owed, as its last run said.
    upkeep: std::sync::Mutex<Option<Instant>>,
    /// The sharing switch has a file no watch hears: a look is owed at every turn.
    unheard: bool,
}

impl member_routes::Projected for Projecting<'_> {
    fn event(&self, event: member_routes::ProjectionEvent) {
        match event {
            member_routes::ProjectionEvent::Maintenance => {
                let at = self.projection.maintain();
                // A runner that ended is reaped here, as the HTTP round reaps it, so its exit
                // owes no further look whatever the next observation admits.
                self.projection.reap();
                *self
                    .upkeep
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = at;
            }
            member_routes::ProjectionEvent::Admitted => self.projection.admitted(),
            member_routes::ProjectionEvent::Observe => self.projection.poll(),
            member_routes::ProjectionEvent::Recovered => self.projection.recovered(),
            member_routes::ProjectionEvent::Ended => self.bell.ring(crate::wake::Wake::Listener),
            member_routes::ProjectionEvent::BrokerReady => {
                self.bell.ring(crate::wake::Wake::Broker);
            }
        }
    }

    fn next_look(&self) -> Option<Instant> {
        if self.unheard {
            return Some(Instant::now());
        }
        let upkeep = *self
            .upkeep
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        upkeep.into_iter().chain(self.projection.next_look()).min()
    }

    fn wakes(&self) -> Vec<std::sync::Arc<std::os::fd::OwnedFd>> {
        self.projection.wakes()
    }
}

fn subscriber_loop(paths: &Paths, log: &(dyn Fn(&str) + Sync)) -> crate::Result<()> {
    let start = |exe: &Path, request| start_converge(exe, request, log);
    // Without a doorbell (this process is out of descriptors) the worker looks at its own pace.
    let doorbell = || {
        aterm_uds::ready::Doorbell::new()
            .ok()
            .map(std::sync::Arc::new)
    };
    let (inputs_rung, members_rung, projection_rung) = (doorbell(), doorbell(), doorbell());
    // The sharing switch holds the projection and the worker's member discovery, and a hold
    // calls no observer that could watch it, so the lane watches it: its change owes a look.
    // Where that watch cannot start, a look is owed at every turn instead.
    let switch = crate::swarm::config_in_home(&paths.home).map(|path| {
        let rung = inputs_rung.clone()?;
        let watch = aterm_uds::file_watch::FileWatch::platform(
            "sharing-switch",
            Box::new(move |_| rung.ring()),
        )
        .ok()?;
        watch.watch(0, path);
        Some(watch)
    });
    let converging = Converging {
        start: &start,
        routes: &crate::server::route_sources,
    };
    let bell = std::sync::Arc::new(crate::wake::Bell::default());
    let routes = crate::wake::hear_routes(&bell);
    let standing = StandingBrowse::default();
    let found = PeerDiscovery::watching(&bell, &standing);
    let ask = |paths: &Paths| match ASK_MEMBERS.get() {
        Some(callbacks) => (callbacks.ask)(paths),
        None => Err(Error::new("this program cannot reach ALab's members")),
    };
    let hold = || standing.stop();
    let away = || {
        #[cfg(unix)]
        return aterm_types::dirs::state_dir()
            .is_some_and(|state| aterm_netplace::routes::paced_now(&state));
        #[cfg(not(unix))]
        false
    };
    let hearing = Hearing {
        found: &found,
        bell: &bell,
        routes,
        ask: &ask,
        members: std::sync::Mutex::new(MembersAsked {
            rung: members_rung.clone(),
            ..MembersAsked::default()
        }),
        hold: &hold,
        away: &away,
    };
    let projection = projection::Projection::observing(
        paths,
        &converging,
        convergence_inputs(paths, ConvergenceRole::Subscriber, inputs_rung.clone()),
    )
    .logging(log)
    .ringing(projection_rung.clone());
    let run = |maintain: &mut dyn FnMut() -> member_routes::Status| {
        subscriber_loop_projecting(paths, log, &|| false, &hearing, &projection, maintain)
    };
    member_routes::while_hearing(
        paths,
        &hearing.members,
        &ask,
        log,
        &Projecting {
            projection: &projection,
            bell: &bell,
            upkeep: std::sync::Mutex::new(None),
            unheard: matches!(switch, Some(None)),
        },
        member_routes::Rings {
            inputs: inputs_rung.as_deref(),
            members: members_rung.as_deref(),
            projection: projection_rung.as_deref(),
        },
        member_routes::NativeChanges::new(),
        run,
    )
}

/// The subscriber's standing vault browse ([`crate::discover::Watch::start_look`]: of
/// `_alab-vault._tcp`, or a lab's vault service, through the LAN broker): started the
/// first time a pass of the ALab vault asks ([`browsed`]), dropped while
/// `[swarm] enabled` holds the rounds ([`Hearing::hold`]), and started afresh by the first pass
/// once sharing is on again.
#[derive(Default)]
struct StandingBrowse {
    watch: std::sync::Mutex<Option<std::sync::Arc<crate::discover::Watch>>>,
}

impl StandingBrowse {
    /// The browse running now, `start`ed where none does.
    fn watch_or_start(
        &self,
        start: impl FnOnce() -> crate::discover::Watch,
    ) -> std::sync::Arc<crate::discover::Watch> {
        std::sync::Arc::clone(
            self.watch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert_with(|| std::sync::Arc::new(start())),
        )
    }

    /// End the browse running now, if one does (dropping it kills its child).
    fn stop(&self) {
        drop(
            self.watch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
    }

    /// Whether a browse runs now.
    #[cfg(test)]
    fn running(&self) -> bool {
        self.watch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
}

/// The ALab vault servers this Mac's `standing` browse found for `config`'s vault, which starts
/// the first time a pass of the ALab vault asks (and again after `[swarm]` held the rounds) and
/// then rings `bell` ([`Hearing`]); none for another vault. While it has heard none yet (it has
/// just started, or this Mac is alone), a bounded browse's.
fn browsed(
    config: &Config,
    bell: &std::sync::Arc<crate::wake::Bell>,
    standing: &StandingBrowse,
) -> Vec<crate::discover::DiscoveredVault> {
    if !alab_vault(config) {
        return Vec::new();
    }
    // Every vault server it hears counts, this Mac's own included: the server here is one more
    // place this subscriber may read.
    let watch = standing.watch_or_start(|| crate::discover::Watch::start_look(bell, &[]));
    if watch.heard().is_empty() {
        return crate::discover::browse_alab(PROBE_DEADLINE).unwrap_or_default();
    }
    watch.found(PROBE_DEADLINE)
}

/// [`subscriber_loop`], ending once `stop` says so (a test's end) at the top of a round or
/// before the pause or nap that ends one, its converge runner started and its routes read
/// through `converging`, its passes reading the vault servers `hearing` found, and its naps
/// after an error ended by what `hearing` hears.
#[cfg(test)]
fn subscriber_loop_until(
    paths: &Paths,
    log: &(dyn Fn(&str) + Sync),
    stop: &dyn Fn() -> bool,
    converging: &Converging<'_>,
    hearing: &Hearing<'_>,
) -> crate::Result<()> {
    let projection = projection::Projection::new(paths, converging).logging(log);
    subscriber_loop_projecting(paths, log, stop, hearing, &projection, &mut || {
        member_routes::Status {
            listening: false,
            retry: None,
        }
    })
}

fn subscriber_loop_projecting(
    paths: &Paths,
    log: &(dyn Fn(&str) + Sync),
    stop: &dyn Fn() -> bool,
    hearing: &Hearing<'_>,
    projection: &projection::Projection<'_>,
    maintain: &mut dyn FnMut() -> member_routes::Status,
) -> crate::Result<()> {
    let mut backoff = Duration::from_secs(5);
    let mut failing = FIRST_RETRY;
    let mut repaired = None;
    let mut last_error = String::new();
    let mut repeated = String::new();
    if projection.replaced() {
        log("this program was replaced on disk; exiting so the service starts the new one");
        return Ok(());
    }
    let mut notary = release::NotaryRounds::default();
    // `[swarm] enabled = false` holds every round: no ask, long-poll, apply or converge pass
    // until it is on again, looked at each [`SWARM_LOOK`] at the cost of one `stat`.
    if projection.allowed() {
        projection.start();
    } else {
        projection.failed();
    }
    loop {
        if stop() {
            return Ok(());
        }
        // Arm before reading the listener's level: a notice sent after this clear
        // must survive to the nap, even if its handle is not finished just yet.
        hearing.bell.clear();
        // Every round, healthy or not, reaps a runner that ended (how it ended is kept for the
        // next healthy round's due check): one started while the vault cannot be reached is no
        // zombie for as long as that lasts.
        let listener = maintain();
        projection.maintain();
        projection.reap();
        if projection.replaced() {
            log("this program was replaced on disk; exiting so the service starts the new one");
            return Ok(());
        }
        if !projection.allowed() {
            projection.failed();
            (hearing.hold)();
            std::thread::sleep(SWARM_LOOK);
            continue;
        }
        // A refused or ended listener is retained for retry at ordinary round boundaries.
        // Keep observing here meanwhile, under the same fresh sharing gate. This fallback
        // follows the network round; the healthy listener remains independent of its HTTP.
        if !listener.listening {
            projection.poll();
        } else {
            projection.resume();
        }
        // Whether or not the vault's source answers: the members on this LAN can let this Mac
        // in while it cannot be reached. An ask that began or ended the wait rings the readiness
        // worker, whose asking follows it.
        {
            hearing
                .members
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .telling(|members| ask_members_if_due(paths, members, hearing.ask, log));
        }
        // A verdict kept before this round's long-poll begins does not end it (only a new one
        // does), so it is read first: one that came while this subscriber started, or paused.
        heed_verdict(paths, log);
        let progress = || {
            load_state(paths)
                .ok()
                .map(|state| (state.seq, state.records_digest))
        };
        let before = progress();
        let routes = crate::server::route_sources();
        let started = Instant::now();
        let said = std::cell::Cell::new(SourceSaid::AtOnce);
        match subscriber_pass_saying(paths, &mut repaired, hearing.found, &said) {
            Ok((config, outcome)) => {
                let _ = crate::service_health::after_round(paths);
                let after = progress();
                let pass = Pass {
                    source: said.get(),
                    moved: after.map(|(s, _)| s) > before.map(|(s, _)| s)
                        || after.map(|(_, r)| r) != before.map(|(_, r)| r),
                };
                last_error.clear();
                if let Some(line) = pass_line(paths, &config, &outcome, &mut repeated) {
                    log(&line);
                }
                // Every healthy round preflights using file reads, so tools installed later
                // are noticed. A release host with the tools handles a changed entry once,
                // keeping a profile that already authenticates.
                notary.after_round(paths, &config, log);
                // A version applied, or (its own route read) moved routes or the recheck.
                projection.after_pass(&outcome, pass.source);
                let requested = || load_state(paths).ok().map(|state| state.requested);
                let before = requested();
                ask_again_if_due(paths, &config, log);
                let after = requested();
                if after != before {
                    hearing
                        .members
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .asked_again(after);
                }
                // A verdict that ended the long-poll.
                heed_verdict(paths, log);
                if stop() {
                    return Ok(());
                }
                let pause = pause_after(&config.source, &outcome, &mut failing, pass);
                if failed_over_idle(pass) {
                    // The source is away and the servers found hold nothing newer: asked
                    // again as after an error, never in a loop.
                    let moved = nap_after_failing(hearing, routes, listener.limit(backoff), log);
                    backoff = retry_after(backoff, moved, (hearing.away)());
                } else {
                    backoff = Duration::from_secs(5);
                    pause_on_bell(
                        hearing.bell,
                        listener.limit(pause.saturating_sub(started.elapsed())),
                    );
                }
            }
            Err(error) => {
                let line = failed_line(&error.to_string(), (hearing.away)());
                if line != last_error {
                    log(&line);
                    last_error = line;
                }
                // The first healthy pass after this one starts a converge runner.
                projection.failed();
                if stop() {
                    return Ok(());
                }
                let moved = nap_after_failing(hearing, routes, listener.limit(backoff), log);
                backoff = retry_after(backoff, moved, (hearing.away)());
            }
        }
    }
}

/// Keep healthy batching's original deadline, while an ended readiness owner can
/// request upkeep immediately. Other queued hints neither end nor renew this pause.
fn pause_on_bell(bell: &crate::wake::Bell, pause: Duration) -> crate::wake::Wake {
    let deadline = Instant::now() + pause;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return crate::wake::Wake::Timer;
        }
        if let why @ (crate::wake::Wake::Timer
        | crate::wake::Wake::Listener
        | crate::wake::Wake::Stop) = bell.nap(left)
        {
            return why;
        }
    }
}

/// A subscriber's nap of `backoff` after a pass that failed, or read only the servers found: a
/// broker arrival or vault server the browse heard ends it, and so does a route change once this Mac's routes
/// moved from `routes`, where they were as the pass began. Whether one ended it, said in `log`.
fn nap_after_failing(
    hearing: &Hearing<'_>,
    routes: [Option<std::net::IpAddr>; 3],
    backoff: Duration,
    log: &(dyn Fn(&str) + Sync),
) -> bool {
    nap_after_failing_with(
        hearing,
        backoff,
        || crate::server::route_sources() != routes,
        log,
    )
}

fn nap_after_failing_with(
    hearing: &Hearing<'_>,
    backoff: Duration,
    mut routes_moved: impl FnMut() -> bool,
    log: &(dyn Fn(&str) + Sync),
) -> bool {
    let woke = std::cell::Cell::new(None);
    let moved = nap_unless_moved(
        backoff,
        &mut || {
            matches!(
                woke.get(),
                Some(
                    crate::wake::Wake::Browse
                        | crate::wake::Wake::Broker
                        | crate::wake::Wake::Listener
                )
            ) || routes_moved()
        },
        &mut |most| wait_for_a_ring(hearing, &woke, most),
    );
    if moved {
        let why = woke.get().unwrap_or(crate::wake::Wake::Route);
        log(&format!("asking again now: woken by {}", why.words()));
    }
    moved
}

/// Read an admin Mac's verdict on this waiting machine's join password when one is due
/// ([`verdict_due`]), and say a refusal once.
fn heed_verdict(paths: &Paths, log: &dyn Fn(&str)) {
    if verdict_due(paths) && read_verdict(paths).unwrap_or(false) {
        log(
            "an admin Mac refused this Mac's join password: retype it (System State's Secrets row, or aterm secrets join)",
        );
    }
}

/// Whether this machine waits with a proof of the join password that no verdict refused yet
/// ([`read_verdict`]).
fn verdict_due(paths: &Paths) -> bool {
    load_state(paths).is_ok_and(|state| {
        state.membership == Stored::Pending && state.proof.is_some() && !refused_now(&state)
    })
}

/// [`subscriber_pass_saying`], where what the source said is not wanted.
#[cfg(test)]
fn subscriber_pass(
    paths: &Paths,
    repaired: &mut Repaired,
    found: Found<'_>,
) -> crate::Result<(Config, SyncOutcome)> {
    subscriber_pass_saying(
        paths,
        repaired,
        found,
        &std::cell::Cell::new(SourceSaid::AtOnce),
    )
}

/// One subscriber pass: a long-poll of the source (a read every minute of a vault at a path),
/// which gives way to the vault servers `found` where the source fails or holds nothing newer
/// ([`sync_found`]). It keeps in `said` what a source at a URL itself said.
fn subscriber_pass_saying(
    paths: &Paths,
    repaired: &mut Repaired,
    found: Found<'_>,
    said: &std::cell::Cell<SourceSaid>,
) -> crate::Result<(Config, SyncOutcome)> {
    creation::resume(paths)?;
    alab_join::resume(paths)?;
    let config = config(paths)?
        .ok_or_else(|| Error::new("no vault configured; waiting for aterm secrets join"))?;
    let before = repaired.clone();
    let synced = match config.source {
        Source::Url(_) => {
            let wait = (!repair_due(paths, repaired)).then_some(LONG_POLL);
            sync_found(paths, wait, found, said)
        }
        // Read every minute, not long-polled: the pass itself rules on putting files back.
        Source::Path(_) => sync_repairing(paths, None, Some(&mut *repaired)),
    };
    match synced {
        Ok(outcome) => {
            // A put-back wrote no file here: the repair this pass was for is still due.
            if matches!(outcome, SyncOutcome::PutBack { .. }) {
                *repaired = before;
            }
            Ok((config, outcome))
        }
        Err(error) => {
            *repaired = before;
            let _ = change_request(paths, &config, |state| {
                let due = state.membership == Stored::Pending && state.requested != 0;
                if due {
                    state.requested = 0;
                }
                due
            });
            Err(error)
        }
    }
}

/// How long a subscriber's long-poll waits for the server's answer, before the margin
/// [`sync`] adds: as long as a server holds one by default, so a request a sleeping Mac lost
/// ends within about that long of its waking.
const LONG_POLL: Duration = crate::server::DEFAULT_HOLD;

/// How often a machine still waiting asks again ([`ask_again_if_due`]).
const REASK: u64 = 60 * 60;

/// How long a join has, from saving the vault, to post its own request ([`in_flight`]).
const JOIN_GRACE: u64 = 5 * 60;

/// The `requested` a join records when it saves the vault, before it asks for the password
/// and posts: dated so the re-ask comes due [`JOIN_GRACE`] later. A subscriber asking sooner,
/// without the join's proof, would make the join's own request a change the server refuses for
/// a while; a join cut off before it posts is asked for after the grace, not an hour on.
fn in_flight() -> u64 {
    now().saturating_sub(REASK - JOIN_GRACE)
}

fn ask_again_if_due(paths: &Paths, config: &Config, log: &dyn Fn(&str)) {
    let due = || -> crate::Result<Option<(Config, Option<String>)>> {
        let _lock = StateLock::take(paths)?;
        let Some(current) = self::config(paths)?
            .filter(|current| (&current.source, &current.name) == (&config.source, &config.name))
        else {
            return Ok(None);
        };
        // A lab identity asks ALab's vault nothing by itself (A12, [`request_join_until`]): it
        // is passed over here, not refused and logged each pass.
        if alab_vault(&current) && refuse_lab_identity(paths).is_err() {
            return Ok(None);
        }
        let state = load_state(paths)?;
        // A Mac the membership log let in asks too while it holds its `Allowed` (F15): an
        // admin's approver still names it in `[machines]` and the fleet, and a member's one
        // post of the `Allowed` can miss.
        let waiting = state.membership == Stored::Pending
            || (state.membership == Stored::In && handover::holds_allowed(paths));
        Ok((waiting && now().saturating_sub(state.requested) >= REASK)
            .then_some((current, state.proof)))
    };
    let Ok(Some((current, proof))) = due() else {
        return;
    };
    match request_join(paths, &current, proof, Asker::Again) {
        Ok(code) => log(&format!("asked to join again (code {code})")),
        Err(error) => log(&format!("join request failed: {error}")),
    }
}

/// Automatic join-approval policy configured in `[join]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// `password = …`: requests whose proof matches it.
    Password,
    /// `open = yes`, or its open note ([`text::OPEN_NOTE`]): no join password. A request is let
    /// in by a member's Allow, and otherwise waits for an admin's approval by code, on every
    /// vault, the pinned ALab vault included: its asking alone admits nothing. A Mac on the home
    /// ALab LAN is let in by a member when it proves itself, whatever `[join]` says.
    Open,
}

impl Approval {
    /// Automatic approval mode configured in `doc`, if any.
    pub fn of(doc: &Document) -> Option<Approval> {
        if doc.open() {
            return Some(Approval::Open);
        }
        doc.join_password().map(|_| Approval::Password)
    }

    const fn reads(self, request: &Request) -> bool {
        match self {
            Approval::Password => request.proof.is_some(),
            Approval::Open => true,
        }
    }
}

/// Whether this machine is an admin of the recorded vault (`Some`), and the automatic approval
/// its `[join]` line sets there, if any.
fn admin_here(
    paths: &Paths,
    config: &Config,
    state: &State,
) -> crate::Result<Option<Option<Approval>>> {
    Ok(admin_text_here(paths, config, state)?.map(|opened| Approval::of(&opened.doc)))
}

/// The admin text of the recorded vault from the local cache, when this machine is its admin.
fn admin_text_here(
    paths: &Paths,
    config: &Config,
    state: &State,
) -> crate::Result<Option<admin_keys::AdminText>> {
    let Some(pin) = config.signer else {
        return Ok(None);
    };
    let cached = chain_cache::held(&paths.cache_file())?
        .ok_or_else(|| Error::new("no cached vault to check approval"))?;
    let (envelope, _, _) =
        envelope::verify_for_reader(cached.bytes(), &config.name, &pin, &config.followed, None)?;
    if envelope.seq != state.seq || Some(envelope.digest()) != state.digest {
        return Err(Error::new("the cached vault is not the recorded version"));
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let Some(locker) = envelope.admin_locker() else {
        return Ok(None);
    };
    let plain = match age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes()) {
        Ok(plain) => plain,
        Err(age::AgeError::NoMatch) => return Ok(None),
        Err(error) => return Err(Error::new(format!("admin locker: {error}"))),
    };
    let opened = parse_admin(&plain)?;
    opened.admissions.validate(&opened.doc, envelope.seq)?;
    if admin_removed_in_log(paths, config, &envelope, &identity, &opened.doc) {
        return Ok(None);
    }
    Ok(Some(opened))
}

pub(super) fn admin_removed_in_log(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    identity: &Identity,
    doc: &Document,
) -> bool {
    admin_removed_in_log_with_lock(paths, config, envelope, identity, doc, None)
}

fn admin_removed_in_log_with_lock(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    identity: &Identity,
    doc: &Document,
    lock: Option<&StateLock>,
) -> bool {
    let log = match log_entries::trusted_seeded_log(paths, config, envelope) {
        Ok(Some((log, _))) => log,
        Ok(None) => return false,
        Err(_) => return true,
    };
    let current = match lock {
        Some(lock) => log_entries::local_swarm_roster_for_log_held(paths, &log, lock),
        None => log_entries::local_swarm_roster_for_log(paths, &log),
    };
    let Ok(mut roster) = current else {
        return true;
    };
    if let Ok(Some(doc_roster)) = crate::swarm_roster::SwarmRoster::from_document(doc) {
        match &mut roster {
            Some(r) => {
                r.deny_labels.extend(doc_roster.deny_labels);
                r.deny_nodes.extend(doc_roster.deny_nodes);
                for m in doc_roster.members {
                    if !r
                        .members
                        .iter()
                        .any(|existing| existing.recipient == m.recipient)
                    {
                        r.members.push(m);
                    }
                }
                r.canonicalize();
                let _ = roster::lift_restored_from_roster(&log, r);
            }
            None => {
                let mut r = doc_roster;
                if let Ok(Some(denied)) = swarm_denied_nodes(paths) {
                    r.deny_nodes.extend(denied);
                }
                roster::overlay_log_membership(&log, None, &mut r);
                roster = Some(r);
            }
        }
    }
    let is_out = |r: &Recipient, machine_name: Option<&str>| {
        log.recipient_is_removed(r)
            || (log_entries::log_base(Some(&log), envelope, identity)
                .ok()
                .is_some_and(|b| b != log_entries::LogBase::Behind)
                && log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    r,
                ))
            || roster.as_ref().is_some_and(|ros| {
                log_entries::recipient_denied_by_roster_or_log(Some(ros), &log, r)
                    || machine_name.is_some_and(|name| ros.is_label_denied(name))
            })
    };
    let my_rec = identity.recipient();
    let my_name = doc
        .machines()
        .into_iter()
        .find(|m| Recipient::parse(&m.recipient).ok().as_ref() == Some(&my_rec))
        .map(|m| m.name)
        .or_else(|| (!config.host.trim().is_empty()).then(|| config.host.clone()));
    if !is_out(&my_rec, my_name.as_deref()) {
        return false;
    }
    let admins: BTreeSet<String> = doc.admins().into_iter().collect();
    doc.machines().into_iter().any(|m| {
        admins.contains(&m.name)
            && Recipient::parse(&m.recipient)
                .ok()
                .is_some_and(|r| r != my_rec && !is_out(&r, Some(&m.name)))
    })
}

/// Why the approver in process `pid` cannot read this Mac's recorded admin locker: its program
/// is a release older than the first that reads the locker's format. An approver from before
/// [`approver_stuck`]'s record existed says nothing of itself; `None` where it reads the locker,
/// this Mac is no admin, or nothing can be told.
pub fn approver_cannot_read(paths: &Paths, pid: u32) -> Option<String> {
    let _lock = StateLock::observe(paths).ok()??;
    let config = config(paths).ok()??;
    let state = load_state(paths).ok()?;
    let custody = admin_text_here(paths, &config, &state).ok()??.custody;
    locker_skew::unreadable_by(pid, custody)
}

struct ApprovalCheck {
    version: ([u8; 32], u64),
    digest: Option<[u8; 32]>,
    at: Instant,
    admin: Option<Option<Approval>>,
}

fn approval_here(
    paths: &Paths,
    checked: &mut Option<ApprovalCheck>,
) -> crate::Result<Option<(Config, u64, Approval)>> {
    Ok(admin_check(paths, checked)?
        .and_then(|(config, seq, approval)| approval.map(|approval| (config, seq, approval))))
}

/// The recorded vault, its version and its automatic approval, when this machine is its admin.
fn admin_check(
    paths: &Paths,
    checked: &mut Option<ApprovalCheck>,
) -> crate::Result<Option<(Config, u64, Option<Approval>)>> {
    let Some(_lock) = StateLock::observe(paths)? else {
        if config(paths)?.is_none() {
            return Ok(None);
        }
        return Err(Error::new(
            "vault state is being changed; check approval again",
        ));
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let state = load_state(paths)?;
    let seq = state.seq;
    let version = (source_identity(&config), seq);
    if state
        .source_identity
        .is_some_and(|source| source != version.0)
    {
        *checked = None;
        return Err(Error::new("the recorded vault belongs to another source"));
    }
    if checked.as_ref().is_none_or(|held| {
        held.version != version
            || held.digest != state.digest
            || held.at.elapsed() >= Duration::from_secs(300)
    }) {
        *checked = None;
        let admin = admin_here(paths, &config, &state)?;
        *checked = Some(ApprovalCheck {
            version,
            digest: state.digest,
            at: Instant::now(),
            admin,
        });
    }
    Ok(checked
        .as_ref()
        .and_then(|held| held.admin)
        .map(|approval| (config, seq, approval)))
}

/// Whether this Mac administers its vault, and the admin locker format this build reads there,
/// from the verified local copy alone ([`mend_held`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AdminLocker {
    /// This Mac is no admin of a signed vault: no vault, a plain text file, a member's key.
    None,
    /// This build opens the admin locker of the version this Mac holds, in this format.
    Reads(&'static str),
    /// Not known now, and why: this Mac's vault state is being changed (its lock is held), or
    /// its state, copy or locker cannot be read.
    Unreadable(String),
}

/// Whether this machine can open its configured vault's administrative locker.
/// Reads only the authenticated local copy; it fetches nothing and prints no values.
///
/// # Errors
/// A concurrent apply or unreadable state prevents an authoritative answer.
pub fn can_administer(paths: &Paths) -> crate::Result<bool> {
    match admin_locker_here(paths) {
        AdminLocker::None => Ok(false),
        AdminLocker::Reads(_) => Ok(true),
        AdminLocker::Unreadable(why) => Err(Error::new(why)),
    }
}

fn admin_locker_here(paths: &Paths) -> AdminLocker {
    let unreadable = |why: &dyn std::fmt::Display| AdminLocker::Unreadable(why.to_string());
    let lock = match StateLock::observe(paths) {
        Ok(lock) => lock,
        Err(error) => return unreadable(&error),
    };
    let Some(_lock) = lock else {
        // A lock that is there but held is a sync or a publish under way, never "no vault"
        // (as `observe_commons` reads it).
        return match fsx::kind(&paths.lock_file()) {
            Ok(fsx::Kind::Missing) => match config(paths) {
                Ok(None) => AdminLocker::None,
                Ok(Some(_)) => unreadable(&"this Mac's vault state has no lock file"),
                Err(error) => unreadable(&error),
            },
            Ok(_) => unreadable(&"this Mac's vault state is being changed"),
            Err(error) => unreadable(&error),
        };
    };
    let config = match config(paths) {
        Ok(Some(config)) => config,
        Ok(None) => return AdminLocker::None,
        Err(error) => return unreadable(&error),
    };
    let state = match load_state(paths) {
        Ok(state) => state,
        Err(error) => return unreadable(&error),
    };
    match admin_text_here(paths, &config, &state) {
        Ok(Some(opened)) => AdminLocker::Reads(opened.custody),
        Ok(None) => AdminLocker::None,
        Err(error) => unreadable(&error),
    }
}

/// What the window's mend did with a vault job held on another aterm ([`mend_held`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldMend {
    /// Nothing is held: the job is absent, runs this aterm, is switched off, or is in a shape
    /// `agent on` or `server on` never writes ([`crate::service::held`]), or a person changed it
    /// while the mend judged it (an `on`, an `off`, a pin; [`crate::service::move_held`]), or,
    /// for the server, the system LaunchDaemon serves this Mac's vaults in its place.
    NotHeld,
    /// Moved onto this aterm, or (the ALab subscriber) installed on it where its job was missing:
    /// the install's line, and why.
    Moved(String, HeldMoved),
    /// Left on the program it holds, and why.
    Kept(HeldKept),
    /// Not judged now, and why: this Mac's vault state is being changed, or cannot be read. The
    /// next mend reads it again.
    Unread(String),
}

/// Why a vault job runs this aterm now ([`HeldMend::Moved`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldMoved {
    /// An ALab server older than [`steward::FLUID_SERVER_MIN_RELEASE`], whose store holds the ALab
    /// vault (`DESIGN-fluid-swarm` §4.3 step 1).
    FluidServer,
    /// An unpinned ALab server or subscriber held on an older release or another copy of this
    /// one: from [`ALAB_MANAGED_JOB_RELEASE`] ALab's jobs follow the installed release, so an old
    /// but healthy job cannot keep behavior the installed release dropped (0.148's and 0.149's
    /// approver admitting a plain ask from the home ALab LAN among it).
    ManagedRelease,
    /// On the ALab vault's admin Mac, an approver older than [`ALLOWED_APPROVER_MIN_RELEASE`],
    /// which admits by `open` and drops a request a member Allowed.
    OpenApprover,
    /// A program older than the first release that reads the admin locker this Mac holds.
    Locker,
    /// Nothing was held: the ALab subscriber's job was missing, and [`mend_subscriber`]
    /// installed it.
    Missing,
    /// The exact owned same-path subscriber still had the old process clamp.
    Scheduling,
}

impl HeldMoved {
    /// Why the job runs this aterm now, as a clause for the window's log.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::FluidServer => {
                "it ran an ALab server older than 0.146, which neither pulls from the other \
                 servers nor hands the alab-secrets name on"
            }
            Self::ManagedRelease => {
                "it ran an older copy of aterm, and ALab's jobs follow the installed release"
            }
            Self::OpenApprover => {
                "it ran an ALab approver older than 0.146, which admits every Mac that asks and \
                 drops each request a member Allowed"
            }
            Self::Locker => "it ran an aterm that cannot read the vault's admin locker",
            Self::Missing => "its job was missing",
            Self::Scheduling => "its owned subscriber needed the current thread scheduling policy",
        }
    }
}

/// Why a held job stays where it is ([`HeldMend::Kept`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldKept {
    /// A person pinned it there on purpose (`agent on --program`, `server on --program`,
    /// [`crate::service::pin_path`]).
    Pinned,
    /// This aterm is a development build, which shares its number with the release before it
    /// and may not read what that writes.
    Development,
    /// This Mac administers no vault, and no managed ALab update applies to this job.
    NoAdmin,
    /// The held program reads the vault's admin locker, so the hold still does what it may have
    /// been for.
    Reads,
    /// The held program's release cannot be read, and why.
    Unknown(String),
}

/// Move this Mac's `job` off a held program (one an admin pinned its approver and server to
/// with `--program`, docs/DESIGN-secrets-2026-10-20.md §9) onto this aterm, the installed one the
/// window's mend runs ([`crate::service::program_for_service`]), with the arguments it had.
/// From 0.147, unpinned ALab jobs follow the installed release, including a held copy of the
/// same core version, and never move backwards. A subscriber needs no admin key for this
/// refresh; a server must hold the verified ALab vault and serve it successfully after moving.
/// Other jobs move only where the hold can no longer do what it was for: the held program is a release older than the
/// first that reads the admin locker this Mac holds (`locker_skew::first_reader` of its
/// format), so as an approver it lets no machine in, while this aterm, a release build, opens
/// that locker. A job a person pinned on purpose is never moved ([`HeldKept::Pinned`]), nor one
/// on a Mac that administers no vault, nor one whose program reads the locker (a snapshot held
/// back from a newer release that misbehaves, as 0.125 was from 0.138 to 0.141), nor one while
/// this Mac's vault state is being changed. The move reads the job again under its decision lock
/// and moves it only while it is still the job judged here, so a person's `off`, `on` or pin
/// meanwhile stands. Its plist changed, launchd boots the job out (its running process ends) and
/// starts it on this aterm; one that does not start there is put back on the program it held
/// ([`crate::service::move_held`]). A held `< 0.146.0` ALab server moves as the converge step
/// moves it (`DESIGN-fluid-swarm` §4.3 step 1): only where its store holds the ALab vault this
/// build verifies, counted moved once it is seen serving that vault at its own address, and put
/// back otherwise ([`crate::service::move_held_serving`]). On the ALab vault's admin Mac, a held
/// approver older than [`ALLOWED_APPROVER_MIN_RELEASE`] moves too, however well it reads the
/// locker: it admits by `open` every Mac that asks, and drops each request a member Allowed
/// (an `allowed=` line it cannot parse), so a Mac let in by Allow and the phrase would wait on
/// it for good (`DESIGN-fluid-swarm` §4.3 step 1).
/// # Errors
/// This build's release is unreadable, or a move was due and this aterm cannot be named for a
/// job (a build, a copy that goes away), the job did not start on it, or the job's decision
/// lock, the subscriber's off record or the system LaunchDaemon's state (for the server) could
/// not be read.
pub fn mend_held(paths: &Paths, job: crate::service::Job) -> crate::Result<HeldMend> {
    let program = crate::service::program_for_service();
    let Some(held) = crate::service::held(&paths.home, job, program.as_deref().ok()) else {
        return Ok(HeldMend::NotHeld);
    };
    mend_held_with(
        paths,
        &locker_skew::own()?,
        &held,
        program.as_deref().ok(),
        &locker_skew::version_of,
        &mut |watch| {
            let program = program
                .as_deref()
                .map_err(|error| Error::new(error.to_string()))?;
            match watch {
                Some(url) => {
                    crate::service::move_held_serving(&paths.home, &held, program, &|| {
                        steward::serves_alab(url, steward::SERVES_WITHIN)
                    })
                }
                None => crate::service::move_held(&paths.home, &held, program),
            }
        },
    )
}

/// The first release whose approver admits only a request that carries a member's `Allowed`
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10): it reads ` allowed=` lines and admits nothing
/// by `open`.
const ALLOWED_APPROVER_MIN_RELEASE: locker_skew::Version = (0, 146, 0);

/// The release that keeps managed ALab jobs on the installed release, so an old but
/// healthy server or approver cannot keep behavior the installed release dropped.
const ALAB_MANAGED_JOB_RELEASE: locker_skew::Version = (0, 147, 0);

fn held_alab_release_due(own: &locker_skew::Own, held: locker_skew::Version) -> bool {
    // Build metadata is not an ordering: adopt this installed release even from a held
    // copy of the same core version. Once moved, `service::held` sees the same path and
    // returns None, including after reinstallation. Never move a newer core backwards.
    own.release && own.version >= ALAB_MANAGED_JOB_RELEASE && held <= own.version
}

/// [`mend_held`] of `held`, this build being `own`, `release_of` reading a program's release
/// and `move_it` moving the job onto this aterm (`None`: a person changed the job meanwhile,
/// [`crate::service::move_held`]); it is given the URL a held ALab server must be seen serving
/// the ALab vault at before the move counts, or `None` for a move that waits on no answer.
fn mend_held_with(
    paths: &Paths,
    own: &locker_skew::Own,
    held: &crate::service::Held,
    destination: Option<&Path>,
    release_of: &dyn Fn(&Path) -> Result<locker_skew::Version, String>,
    move_it: &mut dyn FnMut(Option<&str>) -> crate::Result<Option<String>>,
) -> crate::Result<HeldMend> {
    let kept = |why| Ok(HeldMend::Kept(why));
    let moved = |why: HeldMoved| move |line: String| HeldMend::Moved(line, why);
    let mut checked_move = |watch| {
        let program =
            destination.ok_or_else(|| Error::new("no lasting service destination is available"))?;
        crate::service::require_program_release(program, own.version, release_of)?;
        move_it(watch)
    };
    if held.pinned() {
        return kept(HeldKept::Pinned);
    }
    if !own.release {
        return kept(HeldKept::Development);
    }
    if held.job == crate::service::Job::Server
        && own.version >= steward::FLUID_SERVER_MIN_RELEASE
        && (config(paths)
            .ok()
            .flatten()
            .as_ref()
            .is_some_and(alab_vault)
            || held
                .args
                .windows(2)
                .any(|w| w[0] == "--advertise" && w[1] == crate::alab_advertised()))
        && let Some((store, url)) = crate::cli::served_by(paths, &held.args)
        && crate::server::holds_alab(&store)
    {
        match release_of(&held.program) {
            Err(why) => return kept(HeldKept::Unknown(why)),
            Ok(version)
                if version < steward::FLUID_SERVER_MIN_RELEASE
                    || held_alab_release_due(own, version) =>
            {
                let why = if version < steward::FLUID_SERVER_MIN_RELEASE {
                    HeldMoved::FluidServer
                } else {
                    HeldMoved::ManagedRelease
                };
                return Ok(checked_move(Some(url.as_str()))?.map_or(HeldMend::NotHeld, moved(why)));
            }
            Ok(_) => {}
        }
    }
    // Members and pending joiners need the new subscriber too. This precedes the admin
    // locker check, which remains the narrower legacy/personal-vault repair below.
    if held.job == crate::service::Job::Subscriber
        && own.version >= ALAB_MANAGED_JOB_RELEASE
        && config(paths)
            .ok()
            .flatten()
            .as_ref()
            .is_some_and(alab_vault)
    {
        match release_of(&held.program) {
            Err(why) => return kept(HeldKept::Unknown(why)),
            Ok(version) if held_alab_release_due(own, version) => {
                return Ok(
                    checked_move(None)?.map_or(HeldMend::NotHeld, moved(HeldMoved::ManagedRelease))
                );
            }
            Ok(_) => {}
        }
    }
    let custody = match admin_locker_here(paths) {
        AdminLocker::None => return kept(HeldKept::NoAdmin),
        AdminLocker::Unreadable(why) => return Ok(HeldMend::Unread(why)),
        AdminLocker::Reads(custody) => custody,
    };
    if held.job == crate::service::Job::Subscriber
        && own.version >= ALLOWED_APPROVER_MIN_RELEASE
        && config(paths)
            .ok()
            .flatten()
            .as_ref()
            .is_some_and(alab_vault)
    {
        match release_of(&held.program) {
            Err(why) => return kept(HeldKept::Unknown(why)),
            Ok(version) if version < ALLOWED_APPROVER_MIN_RELEASE => {
                return Ok(
                    checked_move(None)?.map_or(HeldMend::NotHeld, moved(HeldMoved::OpenApprover))
                );
            }
            Ok(_) => {}
        }
    }
    let Some(need) = locker_skew::first_reader(custody, own.version) else {
        return kept(HeldKept::Reads);
    };
    match release_of(&held.program) {
        Err(why) => kept(HeldKept::Unknown(why)),
        Ok(version) if version >= need => kept(HeldKept::Reads),
        Ok(_) => Ok(checked_move(None)?.map_or(HeldMend::NotHeld, moved(HeldMoved::Locker))),
    }
}

/// Automatic approval mode active on this machine for a URL-backed vault, read from local state.
pub fn approving_here(paths: &Paths) -> crate::Result<Option<Approval>> {
    Ok(approval_here(paths, &mut None)?
        .filter(|(config, _, _)| matches!(config.source, Source::Url(_)))
        .map(|(_, _, approval)| approval))
}

fn approver_loop(paths: &Paths, log: &(dyn Fn(&str) + Sync), stop: &aterm_uds::stop::Stop) {
    approver_loop_napping(paths, log, stop, &mut |pause| {
        approver_nap(stop, pause, &|| {
            load_state(paths).map(|state| state.seq).ok()
        });
    });
}

/// [`approver_loop`], each pause between two passes taken by `nap`.
fn approver_loop_napping(
    paths: &Paths,
    log: &(dyn Fn(&str) + Sync),
    stop: &aterm_uds::stop::Stop,
    nap: &mut dyn FnMut(Duration),
) {
    let mut approver = Approver::resumed(paths);
    if let Err(error) = clear_dead_stuck(paths) {
        log(&format!("could not clear old approver records: {error}"));
    }
    let mut swarm = crate::swarm::Switch::new(&paths.home);
    let mut held = false;
    while !stop.is_set() {
        if !swarm_lets(&mut swarm, &mut held, "the approver", log) {
            approver.pause_account_upkeep();
            stop.sleep(SWARM_LOOK);
            continue;
        }
        let pause = approver_pass(paths, log, &mut approver);
        if !pause.is_zero() {
            nap(pause);
        }
    }
}

/// How long an approver's long-poll of the join queue waits for the server's answer, past
/// [`transport::FETCH_MARGIN`]: as long as the server holds one by default.
const JOINS_WAIT: Duration = crate::server::DEFAULT_HOLD;

/// The join queue as an approver last heard it: where, the version the server named (`None`:
/// a server that names none, polled), its lines, and the address of the server that answered
/// (`None`: not known), which a long-poll continues from only.
#[derive(Clone, Default)]
struct Heard {
    url: String,
    version: Option<u64>,
    lines: Vec<String>,
    from: Option<std::net::IpAddr>,
}

/// A fair selection from the configured queue and verified policy mirrors. A version only
/// continues a long-poll at the URL and server address that supplied it; a fresh listing is
/// reported as moved. Empty queues keep their current listing to continue long-polling.
fn hear_joins(
    paths: &Paths,
    config: &Config,
    heard: &mut Heard,
) -> crate::Result<(Vec<String>, bool)> {
    join_queues::hear(paths, config, heard, JOINS_WAIT)
}

/// What a judged approver pass read: the vault version, the requests, and those of them the
/// vault answered as they stand ([`still_answered`]).
type Listed = (
    u64,
    std::collections::BTreeSet<Judged>,
    std::collections::BTreeSet<Answered>,
);

/// The requests in `pending` the vault answered as they stand ([`Answered`]). One whose time the
/// server moved (a Mac let go that asks again as it asked before) drops out, which starts one
/// pass; a request judged anew stays out, so its hourly re-asks start none, and neither do two
/// requests under one key and name.
fn still_answered(
    pending: &[Request],
    answered: &std::collections::BTreeSet<Answered>,
) -> std::collections::BTreeSet<Answered> {
    pending
        .iter()
        .filter_map(Answered::of)
        .filter(|answer| answered.iter().any(|held| held.matches(answer)))
        .collect()
}

/// The requests in `pending` from keys on `code-only` lines now (`named`) that the record did not
/// know on one when its last judged pass of the queue read the vault, or knew but that were named
/// anew since (`known`: the rest, [`stayed_named`]), as answered, save those this approver showed
/// as asking at the version just before (`shown`). An approver records a request as answered only
/// when it read it while its key was in the vault, so one that never did (a new admin Mac, the
/// first run of a build with the record, or one that was off, asleep or no admin while another
/// admin let that Mac in and go) would take the request that let the Mac in, still queued after a
/// version let it go, for an ask, and show a person a Mac nobody runs, whose Approve lets it back
/// in. A `code-only` line names a key a version let go (or one held as the vault opened), so the
/// request it has standing when the line appears is most often no ask. One that is (a Mac held as
/// the vault opened while this approver did not read the version before, or one let go that already
/// asked again) is shown once the server moves its time: a changed request at once, an unchanged
/// one hourly. A key that stayed named stays as it was: a Mac held and shown is still shown after a
/// restart. A Mac let go that asks again, with the renewed join password, before this approver
/// reads the version that let it go is held so too, and let in once its time moves: a proof alone
/// never says when it was made, since an admin may set an earlier password again.
fn code_only_standing(
    named: &std::collections::BTreeSet<String>,
    known: &std::collections::BTreeSet<String>,
    shown: &[Asking],
    pending: &[Request],
) -> Vec<Answered> {
    pending
        .iter()
        .filter(|request| named.contains(&request.recipient) && !known.contains(&request.recipient))
        .filter(|request| {
            !shown
                .iter()
                .any(|asking| asking.recipient == request.recipient)
        })
        .filter_map(Answered::of)
        .collect()
}

/// The keys of `known`, those on `code-only` lines at this approver's last judged pass of the
/// queue (version `judged`), that stayed named through `doc`, version `seq`; the rest were named
/// anew since, and [`code_only_standing`] seeds their standing requests. It rests on three
/// premises:
/// - One version cannot both let a key in and let it go, so at `judged` itself and the version
///   just after it every key known stayed named.
/// - A key let in and let go since is named anew by the version that let it go, which notes it
///   at its own version ([`note_code_only`]); a key that stayed keeps the note it had, or none,
///   so a key the run notes nothing for has been on its line since before the run began.
/// - The run note, through `seq` and begun no later than `judged + 2`, says every version from
///   there on kept the notes. The version just after `judged` alone may be an aterm's before
///   them: it can only have let a key in, and the version that let it go again noted it. A run
///   begun later leaves two versions or more it does not cover, which could have let a key in
///   and let it go.
///
/// Anything else (no version judged, a version read below the one judged, a run that does not
/// reach `seq` or begins later, or a key noted after `judged`) is named anew: its standing
/// request is taken for answered until the server moves its time, which a Mac still asking has
/// it do within an hour or two. A note only ever takes a request for answered, so none lets a
/// Mac in.
fn stayed_named(
    doc: &Document,
    seq: u64,
    judged: Option<u64>,
    known: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    let Some(judged) = judged.filter(|&judged| judged <= seq) else {
        return std::collections::BTreeSet::new();
    };
    if seq <= judged.saturating_add(1) {
        return known.clone();
    }
    match doc.code_only_noted() {
        Some((from, through)) if through == seq && from <= judged.saturating_add(2) => {
            let named_in = doc.code_only_named_in();
            known
                .iter()
                .filter(|key| named_in.get(*key).is_none_or(|&version| version <= judged))
                .cloned()
                .collect()
        }
        _ => std::collections::BTreeSet::new(),
    }
}

/// What the approver keeps from one pass to the next.
#[derive(Default)]
struct Approver {
    settled: Settled,
    checked: Option<ApprovalCheck>,
    release_migration: release_migration::Rounds,
    /// Writes the ALab vault's `open = yes` line as its open note ([`open_migration`]).
    open_migration: open_migration::Rounds,
    /// One executable-owned task for this watch, dropped before its approver exits.
    account_upkeep: Option<Box<dyn AccountUpkeep>>,
    account_upkeep_at: Option<Instant>,
    /// What the last judged pass read ([`Listed`]).
    listed: Option<Listed>,
    /// The last successful asking judgment, retained while its display is withdrawn.
    told: Option<Vec<Asking>>,
    /// The UI record was withdrawn after a failed observation; `told` still
    /// retains the previous judgment used by the admission policy.
    asking_withdrawn: bool,
    /// Whether the last pass met a failure, other than a failure to read on a Mac last read as a
    /// member, or a first refused publish ([`Approver::refused`]), which leave it as it was.
    failing: bool,
    /// Whether the last pass's publish was refused before the vault stored it
    /// ([`crate::ErrorKind::NotStored`]) and said nothing: one refused again is reported.
    refused: bool,
    /// The failure this approver last reported, in its log and in [`approver_stuck`]'s record.
    reported: Option<String>,
    /// Whether this approver's record outlived a removal that failed, and waits for another.
    uncleared: bool,
    /// Whether the record of the failure reported last failed to be written, and waits for
    /// another try.
    unrecorded: bool,
    /// Whether the last pass that read this Mac's role found it no admin.
    member: bool,
    /// The join requests the vault answered that are still queued ([`Answered`]), kept in
    /// their record ([`write_answered`]) so an approver started again knows them too.
    answered: std::collections::BTreeSet<Answered>,
    /// The join queue this approver's record of answered requests was begun on ([`queue_of`]):
    /// a judged pass of another sets it.
    begun: Option<String>,
    /// The keys on `code-only` lines in the version this approver's last judged pass of
    /// `begun`'s queue read, kept in its record: a key a later version names that this does not
    /// hold, or one named anew since ([`stayed_named`]), was let go (or held) since, and its
    /// standing request is seeded as answered ([`code_only_standing`]).
    seen: std::collections::BTreeSet<String>,
    /// The vault version that pass read, kept in its record: [`stayed_named`] tells from it
    /// which of `seen` stayed named since.
    judged: Option<u64>,
    /// Whether `answered` differs from its record, which a failed write left behind.
    answered_unwritten: bool,
    /// The join queue as last heard ([`hear_joins`]).
    heard: Heard,
    /// The key the vault's join password stretches to ([`JoinKeys`]).
    keys: JoinKeys,
}

/// Account maintenance owned by one automatic watch, never shared between watch lifetimes.
/// `step` and `pause` must not wait on a worker or network request. The implementation owns
/// any background work and cancels and joins it in `Drop`, before the watch can finish,
/// including when its subscriber or approver unwinds.
pub trait AccountUpkeep: Send {
    /// Offer work or drain an owned result under freshly opened local admin authority.
    /// The deadline bounds this callback, not an independently owned fetch.
    /// # Errors
    /// The application cannot complete its bounded maintenance step.
    fn step(&mut self, paths: &Paths, admin: &Admin, deadline: Instant) -> crate::Result<usize>;

    /// Retire pending work after a hold or lost authority, without joining a worker.
    fn pause(&mut self) {}
}

type MakeAccountUpkeep = fn() -> Box<dyn AccountUpkeep>;
static ACCOUNT_UPKEEP: std::sync::OnceLock<MakeAccountUpkeep> = std::sync::OnceLock::new();

/// Install the executable's account application maintenance. The normal
/// approver constructs it once per watch and invokes it only after opening current local
/// root authority. A member retains no application authority by constructing the object.
pub fn on_account_upkeep(make: MakeAccountUpkeep) {
    let _ = ACCOUNT_UPKEEP.set(make);
}

fn account_upkeep(
    paths: &Paths,
    config: &Config,
    seq: u64,
    upkeep: &mut Option<Box<dyn AccountUpkeep>>,
    at: &mut Option<Instant>,
    log: &dyn Fn(&str),
) {
    let Some(upkeep) = upkeep.as_mut() else {
        return;
    };
    if crate::service::subscriber_turned_off(&paths.home) {
        upkeep.pause();
        *at = None;
        return;
    }
    if at.is_some_and(|at| at.elapsed() < Duration::from_secs(2)) {
        return;
    }
    *at = Some(Instant::now());
    let deadline = Instant::now() + Duration::from_secs(2);
    let opened = enrollment::admin_cached_until(paths, deadline).and_then(|admin| {
        (admin.config == *config && admin.seq == seq)
            .then_some(admin)
            .ok_or_else(|| Error::new("account upkeep authority changed"))
    });
    let result = match opened {
        Ok(admin) if !crate::service::subscriber_turned_off(&paths.home) => {
            upkeep.step(paths, &admin, deadline)
        }
        Ok(_) => {
            upkeep.pause();
            *at = None;
            Ok(0)
        }
        Err(error) => {
            upkeep.pause();
            *at = None;
            Err(error)
        }
    };
    if let Err(error) = result {
        log(&format!("shared account upkeep: {error}"));
    }
}

/// The key a vault's join password stretches to, kept from one approver pass to the next while
/// the vault and its password stay: stretching it ([`join::ITERATIONS`] of PBKDF2) is the costly
/// part of judging, and the approver judges at each change to the join queue, which any host on
/// the LAN can make by asking to join.
#[derive(Default)]
struct JoinKeys {
    held: Option<(String, SecretBytes, join::Key)>,
}

impl JoinKeys {
    /// The key `password` stretches to for `vault`, stretched only when either changed.
    fn of(&mut self, vault: &str, password: &SecretBytes) -> &join::Key {
        if !matches!(&self.held, Some((held, kept, _)) if held == vault && kept == password) {
            self.held = None;
        }
        let (_, _, key) = self.held.get_or_insert_with(|| {
            (
                vault.to_owned(),
                SecretBytes::from(password.as_slice()),
                join::Key::derive(vault, password.as_slice()),
            )
        });
        key
    }
}

impl Approver {
    /// An approver that knows the join requests its earlier runs found answered.
    fn resumed(paths: &Paths) -> Approver {
        let record = read_answered(paths);
        Approver {
            answered: record.answered,
            begun: record.begun,
            seen: record.seen,
            judged: record.judged,
            account_upkeep: ACCOUNT_UPKEEP.get().map(|make| make()),
            ..Approver::default()
        }
    }

    fn pause_account_upkeep(&mut self) {
        if let Some(upkeep) = &mut self.account_upkeep {
            upkeep.pause();
        }
        self.account_upkeep_at = None;
    }

    /// Keep `answered`, read from the join queue `queue` ([`queue_of`]) at version `judged`,
    /// whose `code-only` lines name `seen`, in its record when any of them changed, or when the
    /// last write failed.
    fn keep_answered(
        &mut self,
        paths: &Paths,
        log: &dyn Fn(&str),
        answered: std::collections::BTreeSet<Answered>,
        queue: String,
        seen: std::collections::BTreeSet<String>,
        judged: u64,
    ) {
        if self.answered == answered
            && self.begun.as_deref() == Some(queue.as_str())
            && self.seen == seen
            && self.judged == Some(judged)
            && !self.answered_unwritten
        {
            return;
        }
        self.answered = answered;
        self.begun = Some(queue);
        self.seen = seen;
        self.judged = Some(judged);
        self.answered_unwritten = match write_answered(
            paths,
            &self.answered,
            self.begun.as_deref(),
            self.judged,
            &self.seen,
        ) {
            Ok(()) => false,
            Err(error) => {
                log(&format!(
                    "could not record the join requests the vault answered: {error}"
                ));
                true
            }
        };
    }

    /// Keep `asking` in the record an admin Mac's aterm reads ([`asking`]) when it changed
    /// or a failed observation withdrew its previous display.
    fn tell(&mut self, paths: &Paths, log: &dyn Fn(&str), asking: Vec<Asking>) {
        if !self.asking_withdrawn && self.told.as_ref() == Some(&asking) {
            return;
        }
        match write_asking(paths, &asking) {
            Ok(()) => {
                self.told = Some(asking);
                self.asking_withdrawn = false;
            }
            Err(error) => log(&format!(
                "could not record the Macs asking to join: {error}"
            )),
        }
    }

    fn withdraw_asking(&mut self, paths: &Paths, log: &dyn Fn(&str)) {
        self.asking_withdrawn = true;
        if let Err(error) = write_asking(paths, &[]) {
            log(&format!("could not withdraw stale join actions: {error}"));
        }
    }

    /// Note `text`, a failure this pass met. It is reported, in the log and in the record
    /// `status` reads ([`approver_stuck`]), when the pass before failed too (or the caller set
    /// `failing` first, for a failure that must not wait), and then once until a failure that
    /// differs in more than its numbers comes: a pass that meets a sync mid-write goes
    /// unreported, and an approver that stays stuck says why.
    fn trouble(&mut self, paths: &Paths, log: &dyn Fn(&str), text: String) {
        self.withdraw_asking(paths, log);
        let shape =
            |text: &str| -> String { text.chars().filter(|c| !c.is_ascii_digit()).collect() };
        let fresh = self.failing && self.reported.as_deref().map(shape) != Some(shape(&text));
        if fresh {
            log(&format!("not approving: {text}"));
        }
        if fresh || self.unrecorded {
            match write_stuck(paths, Some(&text)) {
                Ok(()) => self.unrecorded = false,
                Err(error) => {
                    if fresh || !self.unrecorded {
                        log(&format!(
                            "could not record why the approver is stuck: {error}"
                        ));
                    }
                    self.unrecorded = true;
                }
            }
        }
        if fresh {
            self.reported = Some(text);
        }
        self.failing = true;
        self.refused = false;
    }

    /// Note a pass that met no failure, withdrawing a reported one with the line `said`.
    fn untroubled(&mut self, paths: &Paths, log: &dyn Fn(&str), said: &str) {
        self.failing = false;
        self.refused = false;
        self.unrecorded = false;
        let withdrawn = self.reported.take().is_some();
        if withdrawn {
            log(said);
        }
        if withdrawn || self.uncleared {
            match write_stuck(paths, None) {
                Ok(()) => self.uncleared = false,
                Err(error) => {
                    if !self.uncleared {
                        log(&format!("could not clear the approver's record: {error}"));
                    }
                    self.uncleared = true;
                }
            }
        }
    }
}

/// The approver records' names: `approver-stuck.<pid>`, one per process, so an approver writes
/// only its own and removes no live approver's (its own, and at its start those of processes
/// that are gone, [`clear_dead_stuck`]), and `status` reads the one of the subscriber it names.
const STUCK_PREFIX: &str = "approver-stuck.";

/// The longest failure text the approver's record keeps.
const STUCK_MOST: usize = 512;

/// Record `stuck`, why this process's approver stopped approving, where `status` reads it
/// ([`approver_stuck`]); the record gone when `stuck` is `None`.
fn write_stuck(paths: &Paths, stuck: Option<&str>) -> crate::Result<()> {
    let path = paths.stuck_file(std::process::id());
    let Some(text) = stuck else {
        return remove_stuck(&path);
    };
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(STUCK_MOST)
        .collect();
    fsx::ensure_private_dir(&paths.dir)?;
    fsx::write_atomic(&path, format!("{text}\n").as_bytes(), 0o600)
}

fn remove_stuck(path: &Path) -> crate::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
    }
}

/// Remove the records of approvers that are gone, and one an earlier process with this pid left.
fn clear_dead_stuck(paths: &Paths) -> crate::Result<()> {
    match fsx::kind(&paths.dir)? {
        fsx::Kind::Missing => return Ok(()),
        fsx::Kind::Dir => {}
        fsx::Kind::Symlink => {
            return Err(Error::new(format!(
                "{}: is a symlink; refusing to keep secrets through it",
                paths.dir.display()
            )));
        }
        fsx::Kind::File | fsx::Kind::Other => {
            return Err(Error::new(format!(
                "{}: not a directory",
                paths.dir.display()
            )));
        }
    }
    let entries = match std::fs::read_dir(&paths.dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::new(format!("{}: {error}", paths.dir.display()))),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix(STUCK_PREFIX))
            .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() || !aterm_uds::process::pid_alive(pid) {
            remove_stuck(&entry.path())?;
        }
    }
    Ok(())
}

/// Why the approver in process `pid` last said it stopped approving: from the local record
/// alone, `None` where it reported nothing or approves again.
pub fn approver_stuck(paths: &Paths, pid: u32) -> Option<String> {
    let bytes = fsx::read_private(&paths.stuck_file(pid), 4 << 10).ok()??;
    let text = String::from_utf8_lossy(bytes.as_slice());
    let why = text.lines().next()?;
    (!why.is_empty()).then(|| why.to_owned())
}

fn approver_pass(paths: &Paths, log: &dyn Fn(&str), approver: &mut Approver) -> Duration {
    let (config, seq, approval) = match admin_check(paths, &mut approver.checked) {
        Ok(Some(held)) => {
            approver.member = false;
            held
        }
        Ok(None) => {
            approver.pause_account_upkeep();
            approver.member = true;
            approver.listed = None;
            approver.tell(paths, log, Vec::new());
            approver.untroubled(
                paths,
                log,
                "not approving: this Mac is no admin of a vault now",
            );
            return Duration::from_secs(30);
        }
        // A Mac last read as a member is no approver: its failures to read are not reported.
        Err(_) if approver.member => {
            approver.pause_account_upkeep();
            return Duration::from_secs(2);
        }
        Err(error) => {
            approver.pause_account_upkeep();
            approver.trouble(paths, log, error.to_string());
            return Duration::from_secs(2);
        }
    };
    if approver.release_migration.step(paths, &config, seq, log)
        || approver.open_migration.step(paths, &config, seq, log)
    {
        approver.pause_account_upkeep();
        return Duration::ZERO;
    }
    account_upkeep(
        paths,
        &config,
        seq,
        &mut approver.account_upkeep,
        &mut approver.account_upkeep_at,
        log,
    );
    let (pending, moved) = match hear_joins(paths, &config, &mut approver.heard) {
        Ok((lines, moved)) => (requests_in(lines, &[]), moved),
        Err(error) => {
            approver.trouble(
                paths,
                log,
                format!("could not read the machines asking to join: {error}"),
            );
            return Duration::from_secs(10);
        }
    };
    let by = match approval {
        Some(Approval::Password) => "by the join password",
        Some(Approval::Open) | None => ALLOWED_BY,
    };
    let judged: std::collections::BTreeSet<Judged> = pending.iter().map(Judged::of).collect();
    let listed = (
        seq,
        judged.clone(),
        still_answered(&pending, &approver.answered),
    );
    let mut queue_config = config.clone();
    if !approver.heard.url.is_empty() {
        queue_config.source = Source::Url(approver.heard.url.clone());
    }
    let queue = queue_of(&queue_config);
    let due = approval
        .is_some_and(|approval| approver.settled.due(&queue_config, seq, &pending, approval));
    // A request that came or went, a version that let a Mac in, or a request the vault answered
    // whose time moved (a Mac let go asking again) is read again too, so the record of the Macs
    // an admin approves by hand follows the queue and the vault.
    let judging = approver.asking_withdrawn
        || due
        || approver.listed.as_ref() != Some(&listed)
        || approver.begun.as_deref() != Some(queue.as_str());
    if judging {
        let mut publishing = false;
        let same_queue = approver.begun.as_deref() == Some(queue.as_str());
        let mut answered = if same_queue || approver.begun.is_none() {
            approver.answered.clone()
        } else {
            BTreeSet::new()
        };
        // What this Mac's own approvals by code recorded ([`record_approved`]).
        answered.extend(read_approved(paths));
        // Keys named before, and the version the pass that knew them read, are known only on
        // the queue the record was begun on.
        let (known, last_judged) = if same_queue {
            (approver.seen.clone(), approver.judged)
        } else {
            (std::collections::BTreeSet::new(), None)
        };
        // A Mac this approver showed at the version just before this one was held as its line
        // appeared, since one version cannot both let a Mac in and let it go; across more
        // versions it may have come and gone since, and is seeded like any key named anew.
        let shown = match &approver.listed {
            Some((last, ..)) if same_queue && seq <= last.saturating_add(1) => {
                approver.told.clone().unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let mut seen = std::collections::BTreeSet::new();
        let queue_url = approver.heard.url.clone();
        let keys = &mut approver.keys;
        let done = admin_open(paths).and_then(|admin| {
            if admin.config != config || admin.seq != seq {
                return Err(Error::new("the vault changed while checking approval"));
            }
            seen = admin.published_doc.code_only().into_iter().collect();
            let known = stayed_named(&admin.published_doc, seq, last_judged, &known);
            answered.extend(code_only_standing(&seen, &known, &shown, &pending));
            auto_approve_listed(
                paths,
                &admin,
                &pending,
                &mut publishing,
                &mut answered,
                (keys, Some(&queue_url)),
            )
        });
        match done {
            Ok(mut done) => {
                approver.keep_answered(paths, log, answered, queue, seen, seq);
                // Read against the record this pass left, so the next pass rests.
                approver.listed = Some((seq, judged, still_answered(&pending, &approver.answered)));
                approver.tell(paths, log, std::mem::take(&mut done.asking));
                for line in approved_lines(&done, by) {
                    log(&line);
                }
                for line in seat_lines(&done) {
                    log(&line);
                }
                let refused: Vec<&str> = done
                    .judged
                    .iter()
                    .filter(|judged| {
                        judged.proof.is_some() && done.refused.contains(&judged.recipient)
                    })
                    .map(|judged| judged.host.as_str())
                    .collect();
                if !refused.is_empty() {
                    log(&format!("refused {} {by}", refused.join(", ")));
                }
                approver.settled.settle(done.judged);
            }
            Err(error) => {
                // A failed publish may still have landed (a reply lost after the server took
                // it): say so from the first pass, unlike a failure to read. One the vault
                // refused before storing it met another admin's publish and cannot have landed:
                // after a pass that did not fail it says nothing, leaves the approver as it was
                // and reads that version again soon; refused again, it is reported.
                let refused = error.kind() == crate::ErrorKind::NotStored;
                if refused && !approver.refused && !approver.failing {
                    approver.withdraw_asking(paths, log);
                    approver.refused = true;
                    return Duration::from_secs(2);
                }
                approver.failing |= publishing || refused;
                approver.trouble(paths, log, format!("approving {by} failed: {error}"));
                return Duration::from_secs(30);
            }
        }
    }
    approver.untroubled(paths, log, "approving again");
    match approver.heard.version {
        // A server that names no version of its queue holds no long-poll of it: one that an
        // admin approves only by code is read every 10 s, as often as its notices need.
        None if approval.is_some() => Duration::from_secs(2),
        None => Duration::from_secs(10),
        // A queue that moved with nothing to judge: the server's new version, which this Mac's
        // subscriber applies within moments ([`approver_loop`] naps until it does).
        Some(_) if moved && !judging => Duration::from_secs(2),
        Some(_) => Duration::ZERO,
    }
}

/// Join request key tuple evaluated by [`auto_approve`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Judged {
    /// The machine's key.
    pub recipient: String,
    /// The name it asked for.
    pub host: String,
    /// Its proof of the join password, if it sent one.
    pub proof: Option<String>,
    /// The member's Allow it carried, if any.
    pub allowed: Option<String>,
}

impl Judged {
    fn of(request: &Request) -> Judged {
        Judged {
            recipient: request.recipient.clone(),
            host: request.host.clone(),
            proof: request.proof.clone(),
            allowed: request.allowed.clone(),
        }
    }
}

/// Join requests already judged at the current vault version.
#[derive(Debug, Default)]
pub struct Settled {
    version: Option<([u8; 32], u64)>,
    judged: std::collections::BTreeSet<Judged>,
}

impl Settled {
    /// Whether a request in `listed` relevant to `approval` is unjudged at `(config, seq)`.
    pub fn due(
        &mut self,
        config: &Config,
        seq: u64,
        listed: &[Request],
        approval: Approval,
    ) -> bool {
        let version = (source_identity(config), seq);
        if self.version != Some(version) {
            self.judged.clear();
            self.version = Some(version);
        }
        let still: std::collections::BTreeSet<Judged> = listed.iter().map(Judged::of).collect();
        self.judged.retain(|judged| still.contains(judged));
        listed
            .iter()
            .filter(|request| approval.reads(request))
            .any(|request| !self.judged.contains(&Judged::of(request)))
    }

    /// Record requests judged at the current version.
    pub fn settle(&mut self, judged: Vec<Judged>) {
        self.judged.extend(judged);
    }
}

/// How the approver's log names a machine a member's Allow let in.
const ALLOWED_BY: &str = "by a member's Allow";

/// The approver's log lines for the machines `done` added: those a member's Allow let in apart
/// from the rest, which `by` names (the join password).
fn approved_lines(done: &AutoApproved, by: &str) -> Vec<String> {
    let (allowed, rest): (Vec<&str>, Vec<&str>) = done
        .approved
        .iter()
        .map(String::as_str)
        .partition(|host| done.allowed.iter().any(|allowed| allowed == host));
    [(allowed, ALLOWED_BY), (rest, by)]
        .into_iter()
        .filter(|(hosts, _)| !hosts.is_empty())
        .map(|(hosts, by)| format!("approved {} {by}", hosts.join(", ")))
        .collect()
}

/// The approver's log lines for each new machine `done` let in under the `m<N>` label of a
/// standing seat (`client::continuity`): a new seat beside that one, on the record.
fn seat_lines(done: &AutoApproved) -> Vec<String> {
    done.beside
        .iter()
        .map(|(host, label)| {
            format!(
                "approved {host} as a new seat beside {label}'s: nothing proved it is that \
                 seat's Mac"
            )
        })
        .collect()
}

/// What [`auto_approve`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoApproved {
    /// The machine names added.
    pub approved: Vec<String>,
    /// The machine names in `approved` that a member's Allow let in.
    pub allowed: Vec<String>,
    /// The keys whose proof did not match the password.
    pub refused: Vec<String>,
    /// Keys on an open vault's `code-only` list left for manual code approval.
    pub code_only: Vec<String>,
    /// Every request read and judged in this pass.
    pub judged: Vec<Judged>,
    /// The requests from machines not in the vault that this pass left for an admin to approve
    /// by hand: no valid password or member's Allow, or a `code-only` key.
    pub asking: Vec<Asking>,
    /// The machine names in `approved` that asked under the `m<N>` label of a standing seat, each
    /// with that seat's label: a new seat beside it (nothing the approver reads proves the Mac is
    /// that seat's; only re-admission by proof moves a seat), which `aterm members list` names as
    /// a [`Split`] once the log seats it.
    pub beside: Vec<(String, String)>,
    /// The version published, when any machine was added.
    pub seq: Option<u64>,
}

/// Approve and publish all queued requests eligible under the vault's `[join]` policy.
pub fn auto_approve(paths: &Paths) -> crate::Result<AutoApproved> {
    let admin = admin_open(paths)?;
    auto_approve_opened(
        paths,
        &admin,
        &mut false,
        &mut read_answered(paths).answered,
    )
}

/// [`auto_approve_listed`] of the requests queued at `admin`'s source now.
fn auto_approve_opened(
    paths: &Paths,
    admin: &Admin,
    publishing: &mut bool,
    answered: &mut std::collections::BTreeSet<Answered>,
) -> crate::Result<AutoApproved> {
    let (pending, queue_url) = requests_from(paths, &admin.config)?;
    auto_approve_listed(
        paths,
        admin,
        &pending,
        publishing,
        answered,
        (&mut JoinKeys::default(), Some(&queue_url)),
    )
}

/// The current Commons roster this admin Mac reads, whose writer keys may sign an Allow, and
/// the network it binds; `None` when it holds none, which admits no Allow.
fn allow_roster(
    paths: &Paths,
) -> Option<(
    crate::commons::journal::Trust,
    Vec<crate::commons::roster::Member>,
)> {
    let bootstrap = crate::commons::bootstrap::Bootstrap::cached(paths).ok()??;
    let access = bootstrap.access().ok()??;
    Some((bootstrap.trust(), access.members().to_vec()))
}

/// Whether the Allow `text` admits `request`: it verifies, a member of `roster` signed it, and
/// it names the request's key and name in the roster's network.
fn allowed_by_roster(
    roster: Option<&(
        crate::commons::journal::Trust,
        Vec<crate::commons::roster::Member>,
    )>,
    text: &str,
    request: &Request,
) -> bool {
    let Some((trust, members)) = roster else {
        return false;
    };
    crate::members::Allowed::from_text(text).is_ok_and(|allowed| {
        allowed.admits(
            members,
            &trust.network,
            &trust.authority,
            &request.recipient,
            &request.host,
        )
    })
}

/// [`auto_approve`] with `admin` opened, of the requests `pending` at the selected queue;
/// `publishing` is set once a publish is attempted that may have landed: any failed one but one
/// the vault refused before storing it ([`crate::ErrorKind::NotStored`]). A request in
/// `answered` from a key no longer in the vault is judged and left alone: no approval, no
/// refusal, no ask. On success `answered` holds the requests the vault has answered now: those
/// of keys in the vault (this pass's approvals included), and those it held before that are
/// still queued. The join password's key comes from `keys`, and `queue_url` is the selected
/// queue's, where refusal verdicts are posted. No request is let in by its asking alone, on any
/// vault, the pinned ALab vault included: a Mac on the home ALab LAN is let in by a member when
/// it proves itself (`crate::client::readmit`), and its plain ask waits for a person here.
fn auto_approve_listed(
    paths: &Paths,
    admin: &Admin,
    pending: &[Request],
    publishing: &mut bool,
    answered: &mut std::collections::BTreeSet<Answered>,
    (keys, queue_url): (&mut JoinKeys, Option<&str>),
) -> crate::Result<AutoApproved> {
    let mut done = AutoApproved::default();
    let policy = Approval::of(&admin.published_doc);
    let name = &admin.config.name;
    let key = admin
        .published_doc
        .join_password()
        .filter(|_| policy == Some(Approval::Password))
        .map(|password| keys.of(name, &password));
    let mut members: Vec<String> = admin
        .published_doc
        .machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .collect();
    let code_only = admin.published_doc.code_only();
    let mut doc = admin.published_doc.clone();
    let mut admissions = Vec::new();
    let mut refusals = Vec::new();
    let mut kept = std::collections::BTreeSet::new();
    let mut newly_named = Vec::new();
    let mut roster = None;
    let mut seat_log = None;
    // A Mac a member let in through the membership log is in `[machines]` already (the log's
    // overlay names it), so the approver never approves it. While it still asks with its
    // `Allowed`, the version served does not address it yet: the publish below names it there,
    // so its locker arrives and it stops asking. Registry registration supplies its owner label.
    let mut log_labels = None;
    let mut unaddressed = false;
    for request in pending.iter().cloned() {
        let answer = Answered::of(&request);
        if members.contains(&request.recipient) {
            kept.extend(answer);
            let log_member = |labels: &mut Option<(BTreeMap<String, String>, Vec<String>)>| -> crate::Result<bool> {
                if let Some((names, _)) = labels {
                    return Ok(names.contains_key(&request.recipient));
                }
                let read = roster::log_labels(paths)?;
                let member = read.0.contains_key(&request.recipient);
                *labels = Some(read);
                Ok(member)
            };
            if request.allowed.is_some()
                && !admin.served_machines.contains(&request.recipient)
                && log_member(&mut log_labels)?
            {
                unaddressed = true;
            }
        } else if let Some(answer) =
            answer.filter(|answer| answered.iter().any(|held| held.matches(answer)))
        {
            // Queued while its key was in the vault, and left so by a version that let it go.
            kept.insert(answer);
            done.judged.push(Judged::of(&request));
            continue;
        }
        // A member's Allow admits whatever `[join]` says, but never a key the vault let go
        // (`code-only`) or one the membership log removed and did not restore: an Allow kept by
        // a Mac later removed cannot bring it back, whichever member signed the removal. A
        // removal another member signs reaches this admin's log, never its `code-only`; read
        // only that list, the approver let the removed key back in from a request it had queued
        // before, the next version left it out, and the two went on for good.
        let allowed = !members.contains(&request.recipient)
            && !code_only.contains(&request.recipient)
            && request.allowed.as_deref().is_some_and(|text| {
                allowed_by_roster(
                    roster.get_or_insert_with(|| allow_roster(paths)).as_ref(),
                    text,
                    &request,
                )
            })
            && !Recipient::parse(&request.recipient).is_ok_and(|recipient| {
                seat_log
                    .get_or_insert_with(|| continuity::trusted_log(paths))
                    .as_ref()
                    .is_some_and(|log| log.recipient_is_removed(&recipient))
            });
        let approval = policy.filter(|approval| approval.reads(&request));
        if !allowed && approval.is_none() {
            if !members.contains(&request.recipient) {
                done.asking.push(Asking::of(&request));
            }
            continue;
        }
        let judged = Judged::of(&request);
        if !members.contains(&request.recipient) {
            let method = if allowed {
                Some(AdmissionMethod::Manual)
            } else if approval == Some(Approval::Password)
                && key.zip(request.proof.as_ref()).is_some_and(|(key, proof)| {
                    key.verify(name, &request.recipient, &request.host, proof)
                })
            {
                Some(AdmissionMethod::Password)
            } else {
                None
            };
            if let Some(method) = method {
                // The seat the request asks under, by its host's `m<N>`: a name never takes a
                // seat (only a proof moves one, `readmit`), so a Mac let in here under a standing
                // seat's label is a new seat beside it, on the record.
                let named = seat_log
                    .get_or_insert_with(|| continuity::trusted_log(paths))
                    .as_ref()
                    .and_then(|log| continuity::seat_named(log, &request.host));
                let host = free_name(&doc, &request.host);
                doc.add_machine(&host, &request.recipient)
                    .map_err(|e| Error::new(e.to_string()))?;
                if let Some(label) = named {
                    done.beside.push((host.clone(), label));
                }
                newly_named.push((
                    request.recipient.clone(),
                    request.host.clone(),
                    host.clone(),
                ));
                if matches!(method, AdmissionMethod::Manual) {
                    done.allowed.push(host.clone());
                }
                done.approved.push(host);
                kept.extend(Answered::of(&request));
                admissions.push((request.recipient.clone(), method));
                members.push(request.recipient.clone());
            } else if approval == Some(Approval::Password) {
                done.refused.push(request.recipient.clone());
                if let Some(proof) = &request.proof {
                    refusals.push((request.recipient.clone(), join::proof_tag(proof)));
                }
                done.asking.push(Asking::of(&request));
            } else {
                if code_only.contains(&request.recipient) {
                    done.code_only.push(request.recipient.clone());
                }
                done.asking.push(Asking::of(&request));
            }
        }
        done.judged.push(judged);
    }
    let rostered = |doc: &Document| {
        crate::swarm_roster::SwarmRoster::from_document(doc)
            .ok()
            .flatten()
            .map(|roster| roster.to_toml())
    };
    let roster_before = rostered(&doc);
    if !done.approved.is_empty() {
        roster::maintain_swarm_roster_on_approve(&mut doc, &newly_named)?;
    }
    if !done.approved.is_empty() || unaddressed || rostered(&doc) != roster_before {
        let published = publish_as(paths, admin, &doc, false, &admissions, false);
        // A version the vault refused before storing it (another admin's publish came first)
        // cannot have landed; any other failed publish may have.
        *publishing =
            !matches!(&published, Err(error) if error.kind() == crate::ErrorKind::NotStored);
        let published_seq = published?.seq;
        let approved_recipients: Vec<String> = admissions.iter().map(|(r, _)| r.clone()).collect();
        ledger_publish::verify_approved_in_served_head(
            paths,
            admin,
            published_seq,
            &approved_recipients,
        )?;
        done.seq = Some(published_seq);
    }
    *answered = kept;
    // The joining Mac reads the verdict and asks its person again at once; one that is not
    // told still waits as before, so a verdict that does not arrive changes nothing.
    let reply_at = queue_url.or(match &admin.config.source {
        Source::Url(url) => Some(url.as_str()),
        Source::Path(_) => None,
    });
    if let Some(url) = reply_at {
        for (recipient, tag) in &refusals {
            let _ = transport::post_verdict(url, recipient, tag);
        }
    }
    Ok(done)
}

/// A machine asking to join that an admin approves by hand: what its notice names, and what
/// its Approve approves ([`approve_request`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Asking {
    /// Its approval code, which its own screen shows.
    pub code: String,
    /// Its key: the one machine an Approve lets in.
    pub recipient: String,
    /// The name it asked for.
    pub host: String,
}

impl Asking {
    fn of(request: &Request) -> Asking {
        Asking {
            code: request.code.clone(),
            recipient: request.recipient.clone(),
            host: request.host.clone(),
        }
    }
}

/// The most machines the record of those asking to join holds.
const ASKING_MOST: usize = 64;

/// A join request the vault answered: one queued while its key was in the vault. The server keeps
/// a request until it expires, and a Mac in the vault asks no more, so after a version lets that
/// key go the request still stands as it was: no new ask, and no Mac for a person to approve. A
/// Mac that asks again is read anew once the server moves its request's time, which a changed
/// request does at once and an unchanged one hourly ([`crate::server::JOIN_REFRESH_EVERY_SECS`]).
/// New records also bind the proof: different requests can share a server second.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Answered {
    recipient: String,
    host: String,
    at: u64,
    /// A domain-separated digest including proof presence; absent in legacy records.
    proof: Option<String>,
}

impl Answered {
    /// `request` as answered: none for one the server gave no time, which a later ask from the
    /// same Mac could not be told apart from.
    fn of(request: &Request) -> Option<Answered> {
        let mut proof = aterm_digest::Sha256::new();
        proof.update(b"aterm/secrets/answered-proof/1\0");
        proof.update([u8::from(request.proof.is_some())]);
        if let Some(value) = &request.proof {
            proof.update(value.as_bytes());
        }
        // A request with no Allow digests as it did before Allows existed.
        if let Some(allowed) = &request.allowed {
            proof.update(b"\0allowed\0");
            proof.update(allowed.as_bytes());
        }
        Some(Answered {
            recipient: request.recipient.clone(),
            host: request.host.clone(),
            at: request.at?,
            proof: Some(aterm_codec::hex::encode_lower(&proof.finalize())),
        })
    }

    /// A legacy record cannot distinguish proofs and still suppresses every
    /// matching old request; losing new metadata must not grant a standing ask.
    fn matches(&self, request: &Self) -> bool {
        self.recipient == request.recipient
            && self.host == request.host
            && self.at == request.at
            && (self.proof.is_none() || self.proof == request.proof)
    }
}

/// Answered records span independent queues and are bounded by bytes, rather
/// than by the number of requests one server can hold.
const ANSWERED_BYTES: usize = crate::text::MAX_TEXT + (64 << 10);

/// Optional proof digests by request-line index. Older readers skip these comments.
const ANSWERED_PROOF: &str = "#\tproof\t";

/// Integrity of the complete request lines and indexed proof comments.
const ANSWERED_CHECKSUM: &str = "#\tproofs-checksum\t";

fn answered_checksum(text: &str) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/secrets/answered-record/1\0");
    hash.update(text.as_bytes());
    aterm_codec::hex::encode_lower(&hash.finalize())
}

/// The line of the record's queue file ([`Paths::answered_queue_file`]) that names the join
/// queue the record was begun on ([`queue_of`]).
const ANSWERED_BEGUN: &str = "#\tbegun\t";

/// The join queue an approver judges, as its record of answered requests names it: the vault's
/// name and where it is served, so a new signing key or host name is the same queue.
fn queue_of(config: &Config) -> String {
    let source = match &config.source {
        Source::Url(url) => url.clone(),
        Source::Path(path) => path.display().to_string(),
    };
    format!("{} {source}", config.name)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// The lines of the record's queue file that name the keys on `code-only` lines at its last
/// judged pass of the queue it was begun on, one each.
const ANSWERED_SEEN: &str = "#\tcode-only\t";

/// The line of the record's queue file that names the vault version its last judged pass of
/// the queue read. A build before it reads the other lines by their own heads and skips it.
const ANSWERED_JUDGED: &str = "#\tjudged\t";

/// What an approver's record of answered join requests holds ([`read_answered`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct AnsweredRecord {
    answered: std::collections::BTreeSet<Answered>,
    /// The join queue it was begun on ([`ANSWERED_BEGUN`]).
    begun: Option<String>,
    /// The keys on `code-only` lines at its last judged pass of that queue ([`ANSWERED_SEEN`]).
    seen: std::collections::BTreeSet<String>,
    /// The vault version that pass read ([`ANSWERED_JUDGED`]).
    judged: Option<u64>,
}

/// This admin Mac's approver's record, as it last wrote it: the join requests it found answered
/// ([`Answered`]) in `answered`, and in its queue file the join queue it was begun on
/// ([`ANSWERED_BEGUN`]), the vault version its last judged pass read ([`ANSWERED_JUDGED`]) and
/// the keys it knew on `code-only` lines ([`ANSWERED_SEEN`]). Each is empty or none where the
/// record holds none of it: all of them where there is no record, the queue, the version and the
/// keys where a build before the queue file wrote it, whose requests still read, and the version
/// where a build before that line wrote it. A record a build of 2026-10-26 wrote, with the queue
/// and key lines at the head of `answered`, reads them there.
fn read_answered(paths: &Paths) -> AnsweredRecord {
    let read = |path: &Path, limit: usize| match fsx::read_private(path, limit) {
        Ok(Some(bytes)) => Some(String::from_utf8_lossy(bytes.as_slice()).into_owned()),
        _ => None,
    };
    // Its `code-only` lines are as long as the vault's own, so the vault's limit bounds them,
    // in a record of 2026-10-26 too.
    let limit = ANSWERED_BYTES;
    let requests = read(&paths.answered_file(), limit).unwrap_or_default();
    // This build writes no head lines in `answered`, so any there are newer than the queue file.
    let queue = if requests
        .lines()
        .any(|line| line.starts_with(ANSWERED_BEGUN))
    {
        requests.clone()
    } else {
        read(&paths.answered_queue_file(), limit).unwrap_or_else(|| requests.clone())
    };
    let begun = queue
        .lines()
        .find_map(|line| line.strip_prefix(ANSWERED_BEGUN))
        .map(str::to_owned);
    let seen = queue
        .lines()
        .filter_map(|line| line.strip_prefix(ANSWERED_SEEN))
        .filter(|key| key.starts_with("age1") && !key.contains(char::is_whitespace))
        .map(str::to_owned)
        .collect();
    let judged = queue
        .lines()
        .find_map(|line| line.strip_prefix(ANSWERED_JUDGED))
        .filter(|seq| !seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|seq| seq.parse().ok());
    AnsweredRecord {
        answered: answered_lines(&requests),
        begun,
        seen,
        judged,
    }
}

/// The requests in a record's `key<TAB>time<TAB>name` lines, with
/// optional [`ANSWERED_PROOF`] comments. Missing or conflicting digests remain legacy records;
/// their indices apply only when [`ANSWERED_CHECKSUM`] covers the exact record contents.
fn answered_lines(text: &str) -> std::collections::BTreeSet<Answered> {
    let mut answered: Vec<_> = text
        .lines()
        .filter_map(|line| {
            let mut words = line.splitn(3, '\t');
            let (recipient, at, host) = (words.next()?, words.next()?, words.next()?);
            (recipient.starts_with("age1")
                && !host.is_empty()
                && !at.is_empty()
                && at.bytes().all(|b| b.is_ascii_digit()))
            .then_some(())?;
            Some(Answered {
                recipient: recipient.to_owned(),
                host: host.to_owned(),
                at: at.parse().ok()?,
                proof: None,
            })
        })
        .collect();
    let mut recorded = None;
    let mut covered = String::new();
    for line in text.split_inclusive('\n') {
        if let Some(checksum) = line.strip_prefix(ANSWERED_CHECKSUM) {
            if recorded
                .replace(checksum.strip_suffix('\n').unwrap_or(checksum))
                .is_some()
            {
                return answered.into_iter().collect();
            }
        } else {
            covered.push_str(line);
        }
    }
    let Some(recorded) = recorded else {
        return answered.into_iter().collect();
    };
    if recorded != answered_checksum(&covered) {
        return answered.into_iter().collect();
    }
    let mut proofs: BTreeMap<usize, Option<String>> = BTreeMap::new();
    for line in text
        .lines()
        .filter_map(|line| line.strip_prefix(ANSWERED_PROOF))
    {
        let mut words = line.split('\t');
        let Some(index) = words
            .next()
            .filter(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index < answered.len())
        else {
            continue;
        };
        let proof = words
            .next()
            .filter(|proof| proof.len() == 64 && proof.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .filter(|_| words.next().is_none())
            .map(str::to_ascii_lowercase);
        proofs
            .entry(index)
            .and_modify(|held| {
                if *held != proof {
                    *held = None;
                }
            })
            .or_insert(proof);
    }
    for (index, proof) in proofs {
        if let Some(answer) = answered.get_mut(index) {
            answer.proof = proof;
        }
    }
    answered.into_iter().collect()
}

/// The lines [`answered_lines`] reads: one `key<TAB>time<TAB>name` line each,
/// each followed by its optional proof-digest comment. Each request keeps its
/// legacy key/time/name layout; the record's size is bounded by [`ANSWERED_BYTES`].
/// A final checksum binds the proof comments to those exact lines and their ordering.
fn answered_text(answered: &std::collections::BTreeSet<Answered>) -> String {
    let mut text = answered
        .iter()
        .enumerate()
        .map(|(index, answered)| {
            let host: String = answered.host.chars().filter(|c| !c.is_control()).collect();
            let mut line = format!("{}\t{}\t{host}\n", answered.recipient, answered.at);
            if let Some(proof) = &answered.proof {
                line.push_str(&format!("{ANSWERED_PROOF}{index}\t{proof}\n"));
            }
            line
        })
        .collect::<String>();
    if !text.is_empty() {
        let checksum = answered_checksum(&text);
        text.push_str(&format!("{ANSWERED_CHECKSUM}{checksum}\n"));
    }
    text
}

/// Remove `path`, which may be gone already.
fn remove_record(path: &Path) -> crate::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
    }
}

/// Write the record [`read_answered`] reads: one `key<TAB>time<TAB>name` line for each of
/// `answered` in `answered`, gone when none stand; then in its queue file the queue it was `begun`
/// on ([`ANSWERED_BEGUN`]), the version `judged` then ([`ANSWERED_JUDGED`]) and the keys `seen` on
/// `code-only` lines then ([`ANSWERED_SEEN`]), gone when it was begun on none. A write cut off
/// between the two leaves the new requests beside the keys named before, whose standing requests
/// the next pass only seeds again; the other way round, a key named but its request missing would
/// read as an ask.
fn write_answered(
    paths: &Paths,
    answered: &std::collections::BTreeSet<Answered>,
    begun: Option<&str>,
    judged: Option<u64>,
    seen: &std::collections::BTreeSet<String>,
) -> crate::Result<()> {
    fsx::ensure_private_dir(&paths.dir)?;
    let text = answered_text(answered);
    if text.len() > ANSWERED_BYTES {
        return Err(Error::new("answered request record exceeds its byte limit"));
    }
    if answered.is_empty() {
        remove_record(&paths.answered_file())?;
    } else {
        fsx::write_atomic(&paths.answered_file(), text.as_bytes(), 0o600)?;
    }
    let Some(queue) = begun else {
        return remove_record(&paths.answered_queue_file());
    };
    let plain = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let text: String = std::iter::once(format!("{ANSWERED_BEGUN}{}\n", plain(queue)))
        .chain(judged.map(|seq| format!("{ANSWERED_JUDGED}{seq}\n")))
        .chain(
            seen.iter()
                .map(|key| format!("{ANSWERED_SEEN}{}\n", plain(key))),
        )
        .collect();
    fsx::write_atomic(&paths.answered_queue_file(), text.as_bytes(), 0o600)
}

/// The requests this Mac let in by code and has not observed withdrawn from all
/// authorized queues ([`record_approved`]).
fn read_approved(paths: &Paths) -> std::collections::BTreeSet<Answered> {
    match fsx::read_private(&paths.answered_approved_file(), ANSWERED_BYTES) {
        Ok(Some(bytes)) => answered_lines(&String::from_utf8_lossy(bytes.as_slice())),
        _ => std::collections::BTreeSet::new(),
    }
}

/// Record the requests in `pending` from `recipients`, which this Mac just let in by code, as
/// answered for its approver ([`read_approved`]), as the approver records a member's request: an
/// approver that reads no version between this one and one that lets such a key go again (off,
/// stuck, or outrun) still takes the request for no ask. The approvals keep their own file, which
/// the approver only reads, so neither's write undoes the other's. A complete observation
/// retires prior requests no queue still lists; a partial or absent observation retains them.
pub(crate) fn record_approved(
    paths: &Paths,
    pending: &[Request],
    recipients: &[String],
    complete: bool,
) -> crate::Result<()> {
    let listed: std::collections::BTreeSet<Answered> =
        pending.iter().filter_map(Answered::of).collect();
    let before = read_approved(paths);
    let mut approved: std::collections::BTreeSet<Answered> = if complete {
        listed
            .iter()
            .filter(|answer| before.iter().any(|held| held.matches(answer)))
            .cloned()
            .collect()
    } else {
        // An unobserved queue may still hold any earlier answered request.
        // Neither an unavailable mirror nor a direct-key approval withdraws it.
        before.clone()
    };
    approved.extend(
        pending
            .iter()
            .filter(|request| recipients.contains(&request.recipient))
            .filter_map(Answered::of),
    );
    if approved == before {
        return Ok(());
    }
    if approved.is_empty() {
        return remove_record(&paths.answered_approved_file());
    }
    fsx::ensure_private_dir(&paths.dir)?;
    let text = answered_text(&approved);
    if text.len() > ANSWERED_BYTES {
        return Err(Error::new("approved request record exceeds its byte limit"));
    }
    fsx::write_atomic(&paths.answered_approved_file(), text.as_bytes(), 0o600)
}

/// The recipients of the requests this Mac's approvals by code recorded ([`record_approved`]).
#[cfg(test)]
pub(crate) fn approved_recipients(paths: &Paths) -> Vec<String> {
    read_approved(paths)
        .into_iter()
        .map(|answered| answered.recipient)
        .collect()
}

/// The head line of the record of those asking to join that names the approver process that
/// wrote it ([`write_asking`]): `<pid>@<birth>`, its start as the kernel records it, so a later
/// process given the same pid is not taken for it (`<pid>` alone where the start could not be
/// read). A reader before it skips the line, which holds no tab.
const ASKING_BY: &str = "# approver ";

/// This process as the head of the record of those asking to join names it ([`ASKING_BY`]).
fn approver_named() -> String {
    let pid = std::process::id();
    match i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::read_birth)
    {
        Some(birth) => format!("{pid}@{}", birth.0),
        None => pid.to_string(),
    }
}

/// Whether the approver a record's head names (`<pid>@<birth>`, or `<pid>`) still runs: that pid
/// with that start, or with only a pid named, that pid.
fn approver_runs(named: &str) -> bool {
    let (pid, birth) = named.split_once('@').unwrap_or((named, ""));
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(pid) = pid.parse::<u32>() else {
        return false;
    };
    if birth.is_empty() {
        return aterm_uds::process::pid_alive(pid);
    }
    if !birth.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(birth) = birth.parse::<u64>() else {
        return false;
    };
    i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::read_birth)
        .is_some_and(|now| now.0 == birth)
}

/// Write `asking`, the machines an admin Mac's approver left for a person to approve, where this
/// Mac's aterm reads them ([`asking`]): a head line naming this process ([`ASKING_BY`]), then one
/// `code<TAB>key<TAB>name` line each, the record gone when none ask.
fn write_asking(paths: &Paths, asking: &[Asking]) -> crate::Result<()> {
    let path = paths.asking_file();
    if asking.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
        };
    }
    let text: String = std::iter::once(format!("{ASKING_BY}{}\n", approver_named()))
        .chain(asking.iter().take(ASKING_MOST).map(|asking| {
            let host: String = asking.host.chars().filter(|c| !c.is_control()).collect();
            format!("{}\t{}\t{host}\n", asking.code, asking.recipient)
        }))
        .collect();
    fsx::ensure_private_dir(&paths.dir)?;
    fsx::write_atomic(&path, text.as_bytes(), 0o600)
}

/// The machines asking to join that this admin Mac's approver left for a person to approve, as
/// it last recorded them: from the local record alone, empty where none ask or no approver runs
/// (the process the record names is gone: the subscriber turned off, stopped or killed, so
/// nothing would keep the record current). A record that names no process (an approver before
/// [`ASKING_BY`]) is read as it stands.
pub fn asking(paths: &Paths) -> Vec<Asking> {
    asking_with(paths, &approver_runs)
}

/// [`asking`], `runs` saying whether the approver the record's head names runs.
fn asking_with(paths: &Paths, runs: &dyn Fn(&str) -> bool) -> Vec<Asking> {
    let Ok(Some(bytes)) = fsx::read_private(&paths.asking_file(), 64 << 10) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(bytes.as_slice());
    let by = text
        .lines()
        .next()
        .and_then(|head| head.strip_prefix(ASKING_BY));
    if let Some(by) = by
        && !runs(by)
    {
        return Vec::new();
    }
    text.lines()
        .filter_map(|line| {
            let mut words = line.splitn(3, '\t');
            let (code, recipient, host) = (words.next()?, words.next()?, words.next()?);
            (!code.is_empty() && recipient.starts_with("age1") && !host.is_empty()).then(|| {
                Asking {
                    code: code.to_owned(),
                    recipient: recipient.to_owned(),
                    host: host.to_owned(),
                }
            })
        })
        .take(ASKING_MOST)
        .collect()
}

/// Approve exactly the machine with key `recipient` that asks to join now, under the name it
/// asked for (or the next free one), and publish: what an admin Mac's notice Approve does. The
/// name the vault gave it.
/// # Errors
/// This Mac is no admin, no request from that key is waiting, the queue lists that key under
/// more than one name ([`one_name_asked`]), or the publish failed.
pub fn approve_request(paths: &Paths, recipient: &str) -> crate::Result<String> {
    let admin = admin_open(paths)?;
    approve_request_opened(paths, &admin, recipient)
}

fn approve_request_opened(paths: &Paths, admin: &Admin, recipient: &str) -> crate::Result<String> {
    let observed = requests_for_approval(admin, &[ApprovalTarget::Recipient(recipient)])?;
    approve_observed_request_opened(paths, admin, &observed, recipient)
}

fn approve_observed_request_opened(
    paths: &Paths,
    admin: &Admin,
    observed: &ApprovalRequests,
    recipient: &str,
) -> crate::Result<String> {
    let mut doc = admin.doc.clone();
    let pending = &observed.requests;
    let request = pending
        .iter()
        .find(|request| request.recipient == recipient)
        .ok_or_else(|| Error::new("that Mac no longer asks to join"))?;
    one_name_asked(pending, request)?;
    let host = free_name(&doc, &request.host);
    doc.add_machine(&host, recipient)
        .map_err(|e| Error::new(e.to_string()))?;
    roster::maintain_swarm_roster_on_approve(
        &mut doc,
        &[(recipient.to_owned(), request.host.clone(), host.clone())],
    )?;
    let published = admin_publish_approved(paths, admin, &doc, &[recipient.to_owned()])?;
    let approved_recipients = [recipient.to_owned()];
    ledger_publish::verify_approved_in_served_head(
        paths,
        admin,
        published.seq,
        &approved_recipients,
    )?;
    // Bookkeeping only: the approver records the request too once it reads this version.
    let _ = record_approved(paths, pending, &approved_recipients, observed.complete);
    Ok(host)
}

/// How long a look for a vault ([`probe_within`]) takes at most, all of it: a vault on this
/// LAN answers within milliseconds, and a Mac elsewhere is not kept waiting.
pub const PROBE_DEADLINE: Duration = Duration::from_millis(1500);

/// `product`, or the hang detector a test set on this thread (`ServersAnswering`): every
/// vault server such a test reads is one it started on loopback, which answers, so a read a
/// loaded machine slows is waited for rather than taken for a server that is not there.
pub(crate) fn servers_within(product: Duration) -> Duration {
    #[cfg(any(test, debug_assertions))]
    if let Some(within) = SERVERS_WITHIN.get() {
        return within;
    }
    product
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SERVERS_WITHIN: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// While held, every read of a vault server on this thread waits the hang detector for its
/// answer ([`servers_within`]); its connect keeps [`transport::PROBE_CONNECT`], which a loopback
/// listener's kernel completes without the server running. Its drop, a panic's unwind included,
/// puts back the bound it replaced. Public under `debug_assertions` for the tests of the crates
/// that call this one, and of this crate's `tests/`, which build it without `cfg(test)`.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub struct ServersAnswering(Option<Duration>);

#[cfg(any(test, debug_assertions))]
impl ServersAnswering {
    /// [`Self::within`] the hang detector, for this crate's own tests.
    #[cfg(test)]
    pub(crate) fn set() -> Self {
        Self::within(aterm_spec::wait::MUST_HAPPEN)
    }

    /// Lift this thread's server reads to `bound`, a test's hang detector
    /// (`aterm_spec::wait::MUST_HAPPEN`), until the guard drops.
    #[must_use = "the bound is put back when the guard drops"]
    pub fn within(bound: Duration) -> Self {
        Self(SERVERS_WITHIN.replace(Some(bound)))
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for ServersAnswering {
    fn drop(&mut self) {
        SERVERS_WITHIN.set(self.0);
    }
}

/// A vault that answered a [`probe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    /// The URL that served it.
    pub url: String,
    /// The version it serves.
    pub seq: u64,
    /// Its signer's fingerprint.
    pub fingerprint: String,
    /// The envelope it served, verified: what a join that follows may take in place of its
    /// own fetch ([`JoinOptions::fetched`]).
    pub envelope: Vec<u8>,
}

/// What a [`probe`] heard back from a server that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The vault asked for, signed by the pin or by a key it endorsed.
    Found(Probed),
    /// The server holds no such vault (or, for [`probe_default`], no signer is compiled in).
    Absent,
    /// The endpoint answered with something other than the requested vault.
    NotThisVault(String),
    /// Another vault of the requested name answered, its signature good under a signer the pin
    /// does not reach: a vault someone else keeps at that address. Unlike
    /// [`ProbeOutcome::NotThisVault`] (an error page, a redirect, a server mid-restart), it says
    /// what the network holds.
    AnotherVault(String),
}

/// Check whether `url` serves a vault verified under `pin`: one GET, all of it within `total`
/// ([`transport::get_within`]).
pub fn probe(url: &str, pin: &[u8; 32], total: Duration) -> crate::Result<ProbeOutcome> {
    let base_url = unpinned(url);
    let name = transport::vault_name(&Source::Url(base_url.to_string()))?;
    let answer = transport::get_within(base_url, total)?;
    Ok(probe_answer(base_url, &name, pin, answer))
}

/// `url` without the `#<signer>` pin it may carry: the address a [`probe`] reads.
fn unpinned(url: &str) -> &str {
    url.split_once('#').map_or(url, |(base, _)| base)
}

/// What `answer`, `base_url`'s to a [`probe`] for the vault `name`, says under `pin`.
fn probe_answer(
    base_url: &str,
    name: &str,
    pin: &[u8; 32],
    answer: transport::Answer,
) -> ProbeOutcome {
    let body = match answer {
        transport::Answer::Fetched(Fetched::Body(body)) => body,
        transport::Answer::Fetched(Fetched::Missing | Fetched::NoNewer(_)) => {
            return ProbeOutcome::Absent;
        }
        transport::Answer::Foreign(why) => return ProbeOutcome::NotThisVault(why),
    };
    let verified = envelope::verify_for_reader(&body, name, pin, &[], None);
    match verified {
        Ok((envelope, _, _)) => ProbeOutcome::Found(Probed {
            url: base_url.to_owned(),
            seq: envelope.seq,
            fingerprint: envelope::fingerprint(&envelope.signer),
            envelope: body,
        }),
        Err(error) => match envelope::parse(&body) {
            Ok(other) if other.name == name && other.verify_signature().is_ok() => {
                ProbeOutcome::AnotherVault(error.to_string())
            }
            _ => ProbeOutcome::NotThisVault(error.to_string()),
        },
    }
}

/// [`probe`] of the vault at `url` (a `#<signer>` pin allowed) under its pin: the URL's own,
/// else the key built in for the ALab vault's URL. A URL with neither is refused, since only a
/// pin tells the vault apart from anything else that answers there.
pub fn probe_within(url: &str, total: Duration) -> crate::Result<ProbeOutcome> {
    let (source, fragment) = transport::parse_source(url, Path::new("/"))?;
    let Source::Url(base) = &source else {
        return Err(Error::new("a probe looks for a vault at an http(s):// URL"));
    };
    let pin = fragment.or_else(|| default_pin(&source)).ok_or_else(|| {
        Error::new(format!(
            "{}: nothing pins this vault's signing key; add #<signer> to the URL",
            transport::describe(base)
        ))
    })?;
    probe(base, &pin, total)
}

/// Probe `urls` in parallel under `pin` within `total` and return the verified candidate with the
/// strictly highest [`Probed::seq`] (ties keep `urls`'s order). Unverified or forged responses
/// never win regardless of their advertised or header `seq`. These explicitly supplied URLs
/// form the authoritative set: only when every one answers [`ProbeOutcome::AnotherVault`] can
/// their aggregate stand for a later look.
pub fn probe_candidates(
    urls: &[String],
    pin: &[u8; 32],
    total: Duration,
) -> crate::Result<ProbeOutcome> {
    probe_candidates_with(
        urls,
        pin,
        total,
        |_| ProbeOrigin::Default,
        transport::look_within,
    )
}

/// The compiled addresses can certify a network's negative discovery record; an independently
/// discovered service can supply a verified vault, but cannot certify those addresses absent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProbeOrigin {
    Default,
    Discovered,
}

/// Where a candidate of [`probe_default`] came from: [`crate::ALAB_DEFAULT_URLS`] or a browse.
fn default_or_discovered(url: &str) -> ProbeOrigin {
    if crate::is_alab_default_url(url) {
        ProbeOrigin::Default
    } else {
        ProbeOrigin::Discovered
    }
}

/// [`probe_candidates`] of `urls`, each one's origin as `origin` says, each answer read by `look`
/// ([`probes_within`]).
fn probe_candidates_with(
    urls: &[String],
    pin: &[u8; 32],
    total: Duration,
    origin: impl Fn(&str) -> ProbeOrigin,
    look: impl Fn(&str, Duration, usize) -> Looked + Sync,
) -> crate::Result<ProbeOutcome> {
    if urls.is_empty() {
        return Ok(ProbeOutcome::Absent);
    }
    let outcomes = probes_within(urls, pin, total, look);
    select_probed_outcome(
        urls.iter()
            .zip(outcomes)
            .map(|(url, outcome)| (origin(url), outcome)),
    )
}

/// [`probe_candidates_with`] of the ALab vault at the compiled-in addresses the look asks
/// ([`crate::discover::look_default_urls`]: [`crate::ALAB_DEFAULT_URLS`], none in a lab's
/// process) and the servers a browse `discovered`, under `pin`, all within `total`, each answer
/// read by `look`: the look [`probe_default`] and a bounded join's probe both take.
fn probe_alab(
    pin: &[u8; 32],
    discovered: &[crate::discover::DiscoveredVault],
    total: Duration,
    look: impl Fn(&str, Duration, usize) -> Looked + Sync,
) -> crate::Result<ProbeOutcome> {
    let urls = crate::discover::candidate_urls(
        crate::discover::look_default_urls(),
        discovered,
        crate::ALAB_VAULT,
    );
    probe_candidates_with(&urls, pin, total, default_or_discovered, look)
}

/// What a probe's read of at most some bytes of one server's answer heard
/// ([`transport::look_within`]).
type Looked = crate::Result<transport::Pulled<transport::Answer>>;

/// [`probe`] of each of `urls` under `pin` at once, all within `total`, each answer read by
/// `look` ([`transport::look_within`]) under [`fan_out::reads_within`]'s bound
/// ([`fan_out::ENVELOPES`]): every answer under the peek at once, and one longer than that whole
/// under one of a few slots, or, for a compiled-in default address, without one. A Mac that is
/// joining trusts none of the servers a browse names, so nothing it probes holds more unverified
/// bodies than that at once. Each outcome is what [`probe`] says of that server's answer, in
/// `urls`' order, and a read that failed is the error it failed with, as `look` said it (one
/// `total` left no time for, [`fan_out::reads_within`]'s [`crate::ErrorKind::TimedOut`] one),
/// which the fold ([`select_probed_outcome`]) answers with when no server answered.
fn probes_within(
    urls: &[String],
    pin: &[u8; 32],
    total: Duration,
    look: impl Fn(&str, Duration, usize) -> Looked + Sync,
) -> Vec<crate::Result<ProbeOutcome>> {
    fan_out::reads_within(
        fan_out::ENVELOPES,
        urls,
        total,
        |url, left, limit| {
            let base_url = unpinned(url);
            // A URL that names no vault is refused before anything is asked of it.
            transport::vault_name(&Source::Url(base_url.to_owned()))?;
            look(base_url, left, limit)
        },
        |url, read| {
            let read = read?;
            let base_url = unpinned(url);
            let name = transport::vault_name(&Source::Url(base_url.to_owned()))?;
            Ok(match read {
                transport::Pulled::Fetched(answer) => probe_answer(base_url, &name, pin, answer),
                transport::Pulled::Larger => {
                    ProbeOutcome::NotThisVault(transport::past_any_envelope(base_url))
                }
            })
        },
    )
    .into_iter()
    .map(|outcome| outcome.unwrap_or_else(|| Err(Error::new("a vault probe panicked"))))
    .collect()
}

fn select_probed_outcome(
    outcomes: impl IntoIterator<Item = (ProbeOrigin, crate::Result<ProbeOutcome>)>,
) -> crate::Result<ProbeOutcome> {
    let mut best_found: Option<Probed> = None;
    let mut defaults = 0usize;
    let mut defaults_elsewhere = 0usize;
    let mut default_elsewhere = None;
    let mut saw_absent = false;
    let mut another: Option<String> = None;
    let mut foreign: Option<String> = None;
    let mut first_err: Option<crate::Error> = None;

    for (origin, outcome) in outcomes {
        if origin == ProbeOrigin::Default {
            defaults += 1;
        }
        match outcome {
            Ok(ProbeOutcome::Found(probed)) => {
                if best_found
                    .as_ref()
                    .is_none_or(|current| probed.seq > current.seq)
                {
                    best_found = Some(probed);
                }
            }
            Ok(ProbeOutcome::Absent) => saw_absent = true,
            Ok(ProbeOutcome::AnotherVault(why)) => {
                if origin == ProbeOrigin::Default {
                    defaults_elsewhere += 1;
                    default_elsewhere.get_or_insert_with(|| why.clone());
                }
                another.get_or_insert(why);
            }
            Ok(ProbeOutcome::NotThisVault(why)) => {
                foreign.get_or_insert(why);
            }
            Err(err) => {
                first_err.get_or_insert(err);
            }
        }
    }

    if let Some(probed) = best_found {
        return Ok(ProbeOutcome::Found(probed));
    }
    // A timeout or a 404 at any authoritative address proves nothing about a vault that may
    // wake there later. Discovery cannot complete this certificate, or mask a complete one.
    if defaults != 0
        && defaults_elsewhere == defaults
        && let Some(why) = default_elsewhere
    {
        return Ok(ProbeOutcome::AnotherVault(why));
    }
    if saw_absent {
        return Ok(ProbeOutcome::Absent);
    }
    if let Some(why) = another {
        return Ok(ProbeOutcome::NotThisVault(why));
    }
    if let Some(why) = foreign {
        return Ok(ProbeOutcome::NotThisVault(why));
    }
    if let Some(err) = first_err {
        return Err(err);
    }
    Ok(ProbeOutcome::Absent)
}

/// [`probe_within`] of the ALab vault across [`crate::ALAB_DEFAULT_URLS`] and `_alab-vault._tcp`
/// mDNS browse under the compiled-in signer ([`ProbeOutcome::Absent`] when none is compiled in),
/// taking the highest verified `seq`. In a lab's process
/// ([`crate::discover::LAB_VAULT_SERVICE_ENV`] set) it asks only what the lab's browse heard
/// ([`probe_alab`]).
pub fn probe_default(total: Duration) -> crate::Result<ProbeOutcome> {
    let Some(pin) = decode_key32(crate::ALAB_SIGNER) else {
        return Ok(ProbeOutcome::Absent);
    };
    let started = Instant::now();
    // A browse that cannot run (no LAN broker) finds none: the defaults are still asked.
    let discovered = crate::discover::browse_alab(total).unwrap_or_default();
    let left = total
        .saturating_sub(started.elapsed())
        .max(Duration::from_millis(100).min(total));
    probe_alab(&pin, &discovered, left, transport::look_within)
}

/// Whether this build carries the ALab vault's signer, without which [`probe_default`] asks
/// nothing of the network and answers [`ProbeOutcome::Absent`].
#[must_use]
pub fn alab_signer_built_in() -> bool {
    decode_key32(crate::ALAB_SIGNER).is_some()
}

/// Who asks to join, which says what [`request_join`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asker {
    /// A person's join: it records the proof it asked with, and when it asked.
    Join,
    /// The subscriber asking again with the proof it read ([`ask_again_if_due`]): it records
    /// only when it asked, and only while the state still holds that proof.
    Again,
}

fn request_join(
    paths: &Paths,
    config: &Config,
    proof: Option<String>,
    asker: Asker,
) -> crate::Result<String> {
    request_join_until(paths, config, proof, asker, None)
}

fn request_join_until(
    paths: &Paths,
    config: &Config,
    proof: Option<String>,
    asker: Asker,
    deadline: Option<Instant>,
) -> crate::Result<String> {
    request_join_until_admitted(paths, config, proof, asker, deadline, &|| Ok(()))
}

fn request_join_until_admitted(
    paths: &Paths,
    config: &Config,
    proof: Option<String>,
    asker: Asker,
    deadline: Option<Instant>,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<String> {
    current()?;
    refuse_pending_native_enrollment(paths, config)?;
    // A12: an ask nobody typed (the subscriber's re-ask, the handover's ask with its `Allowed`)
    // goes to ALab's vault only from an identity that is no lab identity. A join a person typed
    // was judged where they typed it (`join`).
    if asker == Asker::Again && alab_vault(config) {
        refuse_lab_identity(paths)?;
    }
    let identity = {
        let _lock = bounded::take(paths, deadline)?;
        current()?;
        machine_identity(paths)?
    };
    let recipient = identity.recipient();
    let code = join_code(&recipient);
    // A join keeps its new proof before it asks: a request cut short (no answer, an interrupt,
    // the change gap's pause) leaves the proof the next ask sends, never the one before it.
    if asker == Asker::Join {
        change_request_until_admitted(paths, config, deadline, current, |state| {
            if state.proof != proof {
                state.refused = None;
            }
            state.proof.clone_from(&proof);
            if matches!(state.membership, Stored::None | Stored::Revoked) {
                state.membership = Stored::Pending;
            }
            true
        })?;
    }
    current()?;
    let posted = match &config.source {
        Source::Url(_) => {
            let mut line = format!("{} {} {code}", recipient.to_bech32(), config.host);
            if let Some(allowed) = handover::stored_allowed(paths) {
                line.push_str(&format!(" allowed={allowed}"));
            }
            if let Some(proof) = &proof {
                line.push_str(&format!(" proof={proof}"));
            }
            join_routing::post(paths, config, &line, deadline)
        }
        Source::Path(_) => Ok(()),
    };
    let queued = posted.is_ok();
    // Only while the proof sent is still this machine's (a later join may have changed it).
    let recorded = change_request_until_admitted(paths, config, deadline, current, |state| {
        if state.proof != proof {
            return false;
        }
        state.requested = if queued { now() } else { 0 };
        true
    });
    posted.and(recorded).map(|()| code)
}

fn change_request(
    paths: &Paths,
    config: &Config,
    change: impl FnOnce(&mut State) -> bool,
) -> crate::Result<()> {
    change_request_until(paths, config, None, change)
}

fn change_request_until(
    paths: &Paths,
    config: &Config,
    deadline: Option<Instant>,
    change: impl FnOnce(&mut State) -> bool,
) -> crate::Result<()> {
    change_request_until_admitted(paths, config, deadline, &|| Ok(()), change)
}

fn change_request_until_admitted(
    paths: &Paths,
    config: &Config,
    deadline: Option<Instant>,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
    change: impl FnOnce(&mut State) -> bool,
) -> crate::Result<()> {
    let _lock = bounded::take(paths, deadline)?;
    current()?;
    let current = self::config(paths)?;
    if current
        .as_ref()
        .is_none_or(|current| (&current.source, &current.name) != (&config.source, &config.name))
    {
        return Ok(());
    }
    let mut state = load_state(paths)?;
    if !change(&mut state) {
        return Ok(());
    }
    save_state(paths, &state)
}

fn default_pin(source: &Source) -> Option<[u8; 32]> {
    match source {
        Source::Url(url) if crate::is_alab_default_url(url) => decode_key32(crate::ALAB_SIGNER),
        _ => None,
    }
}

/// Join a vault: configure the source and machine key, verify, then sync or queue a join request.
/// The vault that dropped this Mac ([`dropped`]) is not asked again with its key: that join is
/// refused with [`crate::ErrorKind::Dropped`] and [`dropped_words`], and nothing changes.
pub fn join(paths: &Paths, options: &JoinOptions<'_>) -> crate::Result<JoinOutcome> {
    join_live(paths, options, ExpectedConfig::Any)
}

/// Join only over the configuration the caller read.
/// # Errors
/// As [`fn@join`], or the current configuration differs from `expected`.
pub fn join_if_unchanged(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: Option<&Config>,
) -> crate::Result<JoinOutcome> {
    join_live(paths, options, ExpectedConfig::Read(expected))
}

/// Join under the caller's current authority, without constraining its source choice.
/// `current` is read-only and must acquire neither the vault lock nor another lock that can
/// wait for it: publication checks call it while holding the vault lock. It must retain the
/// same admission throughout this operation rather than accepting a later allowed choice.
///
/// # Errors
/// As [`fn@join`], or the caller's authority is withdrawn before a subsequent effect.
pub fn join_admitted(
    paths: &Paths,
    options: &JoinOptions<'_>,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<JoinOutcome> {
    join_live(paths, options, ExpectedConfig::AdmittedAny(current))
}

/// Join only over the configuration the caller read, under its current authority.
/// As [`join_admitted`], `current` is checked under the vault lock and must remain read-only
/// without acquiring that lock. After this join saves its source, further effects recheck
/// `current`; they do not require the old `expected` configuration to remain in place.
///
/// # Errors
/// As [`join_if_unchanged`], or the caller's authority is withdrawn.
pub fn join_if_unchanged_admitted(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: Option<&Config>,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<JoinOutcome> {
    join_live(
        paths,
        options,
        ExpectedConfig::AdmittedRead(expected, current),
    )
}

fn join_live(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: ExpectedConfig<'_>,
) -> crate::Result<JoinOutcome> {
    join_live_started(paths, options, expected, Instant::now())
}

fn join_live_started(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: ExpectedConfig<'_>,
    started: Instant,
) -> crate::Result<JoinOutcome> {
    expected.check_sharing(paths)?;
    let typed = options.source.trim();
    // No URL, or the default one (System State's first run passes it), asks the live ALab: a lab
    // identity is refused before the look and the fetch (A12). A URL a person typed goes; its
    // vault decides.
    if typed.is_empty() || crate::is_alab_default_url(typed) {
        refuse_lab_identity(paths)?;
    }
    if options.fetched.is_none()
        && alab_signer_built_in()
        && (typed.is_empty() || crate::is_alab_default_url(typed))
        && let Ok(ProbeOutcome::Found(probed)) = probe_default(Duration::from_secs(10))
    {
        let resolved_source = if crate::is_alab_default_url(&probed.url) {
            probed.url
        } else {
            format!("{}#{}", probed.url, crate::ALAB_SIGNER)
        };
        let resolved = JoinOptions {
            source: resolved_source,
            host: options.host.clone(),
            interactive: options.interactive,
            wait: options.wait,
            confirm: options.confirm,
            password: match &options.password {
                JoinPassword::Ask(ask) => JoinPassword::Ask(*ask),
                JoinPassword::Given(pw) => JoinPassword::Given(pw.clone()),
            },
            fetched: Some(&probed.envelope),
            requested: options.requested,
            say: options.say,
        };
        return join_with(paths, &resolved, expected, |source| {
            fetch_required_primary(source, None, started)
        });
    }
    join_with(paths, options, expected, |source| {
        fetch_required_primary(source, None, started)
    })
}

fn join_with(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: ExpectedConfig<'_>,
    fetch: impl FnOnce(&Source) -> crate::Result<Fetched>,
) -> crate::Result<JoinOutcome> {
    join_with_until(paths, options, expected, fetch, None)
}

/// Whether a join of the vault `name` that `pin` verified (handing off to `pinned`) joins
/// `previous`, the vault this Mac has configured, again: the same name, signed by a key its pin
/// admits ([`pin_admitted`]), wherever it is served from (a LAN-probed URL is the same vault).
fn joins_again(previous: &Config, name: &str, pin: [u8; 32], pinned: [u8; 32]) -> bool {
    previous.name == name
        && (pin_admitted(previous, Some(pin)) || pin_admitted(previous, Some(pinned)))
}

/// Refuse, with [`dropped_words`] and [`crate::ErrorKind::Dropped`], a join of the vault that
/// dropped this Mac ([`dropped`]): every member that read the rotation refuses its key, and the
/// join would move it to pending, losing the reading that says how it comes back (one-mesh E2,
/// f299). A Mac an act took out, or one its log does not judge, joins as before.
/// # Errors
/// This Mac was dropped, or its state cannot be read.
fn refuse_dropped(paths: &Paths) -> crate::Result<()> {
    match dropped_words(paths, load_state(paths)?.seq) {
        Some(words) => Err(Error::of_kind(crate::ErrorKind::Dropped, words)),
        None => Ok(()),
    }
}

fn join_with_until(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: ExpectedConfig<'_>,
    fetch: impl FnOnce(&Source) -> crate::Result<Fetched>,
    deadline: Option<Instant>,
) -> crate::Result<JoinOutcome> {
    expected.check_sharing(paths)?;
    let typed = options.source.trim();
    let typed = if typed.is_empty() {
        crate::ALAB_DEFAULT_URL
    } else {
        typed
    };
    let (source, fragment) = transport::parse_source(typed, &paths.home)?;
    let name = transport::vault_name(&source)?;
    let source_display = match &source {
        Source::Url(url) => transport::describe(url),
        Source::Path(path) => path.display().to_string(),
    };
    let host = match &options.host {
        Some(host) if text::valid_machine_name(host) => host.clone(),
        Some(host) => return Err(Error::new(format!("{host:?} is not a valid machine name"))),
        None => default_host_name(),
    };
    // A password the caller holds stretches while the fetch is out.
    let stretching = match &options.password {
        JoinPassword::Given(Some(password)) if matches!(source, Source::Url(_)) => {
            let (vault, password) = (name.clone(), password.clone());
            Some(std::thread::spawn(move || {
                join::Key::derive(&vault, password.as_slice())
            }))
        }
        _ => None,
    };
    let fetched = match options.fetched {
        Some(bytes) if fetched_serves(paths, &source, &name, bytes)? => {
            Fetched::Body(bytes.to_vec())
        }
        _ => fetch(&source)?,
    };
    let bytes = match fetched {
        Fetched::Body(bytes) => bytes,
        Fetched::Missing => return Err(Error::new(format!("{source_display}: no vault there"))),
        Fetched::NoNewer(_) => return Err(Error::new("the source answered with no vault")),
    };
    let previous = self::config(paths)?;
    // The machine key is made only once the source holds a vault this join takes, so a join
    // that fails before leaves none.
    let announce = |identity: &Identity| {
        let recipient = identity.recipient();
        (options.say)(&JoinEvent::Machine {
            host: &host,
            code: &join_code(&recipient),
            recipient: &recipient.to_bech32(),
        });
    };
    if !is_envelope(&bytes) {
        let Source::Path(path) = &source else {
            return Err(Error::new(format!(
                "{source_display} does not serve an aterm secrets vault"
            )));
        };
        // A signer typed with the path, or the one this Mac pins for it, says a signed vault
        // belongs there: plain text in its place is not that vault, and reading it would drop
        // the pin without a word, taking whatever anyone who can write the file puts there.
        let stored = previous
            .as_ref()
            .filter(|previous| previous.source == source && previous.name == name)
            .and_then(|previous| previous.signer);
        if let Some(pin) = fragment.or(stored) {
            return Err(Error::new(format!(
                "{source_display} holds plain text, not the vault signed by {} that this join expects; nothing changed (to read a plain file, join it at another path)",
                envelope::fingerprint(&pin)
            )));
        }
        let config = Config {
            source: source.clone(),
            mirrors: Vec::new(),
            name,
            signer: None,
            followed: Vec::new(),
            host: host.clone(),
        };
        plain_entries(&config, path)?;
        let identity = {
            let _lock = bounded::take(paths, deadline)?;
            creation::permit_join(paths)?;
            expected.check(paths)?;
            let identity = machine_identity(paths)?;
            let mut state = load_state(paths)?;
            state.seq = 0;
            state.digest = None;
            state.records_digest = None;
            state.membership = Stored::Plain;
            expected.check_sharing(paths)?;
            prepare_materialization(paths, &mut state)?;
            expected.check_sharing(paths)?;
            save_config(paths, &config)?;
            identity
        };
        announce(&identity);
        expected.check_sharing(paths)?;
        let fetched = transport::fetch(
            &config.source,
            None,
            bounded::budget(deadline, servers_within(PROBE_DEADLINE))?,
        )?;
        apply_join_fetched(paths, fetched, expected, deadline)?;
        return Ok(JoinOutcome::Plain {
            names: registered_names(paths)?,
        });
    }
    let parsed = envelope::parse(&bytes)?;
    parsed.verify_signature()?;
    let same_place = previous
        .as_ref()
        .is_some_and(|previous| previous.source == source && previous.name == name);
    let stored = previous
        .as_ref()
        .filter(|_| same_place)
        .and_then(|previous| previous.signer);
    let pin = match fragment.or(stored).or_else(|| default_pin(&source)) {
        Some(pin) => pin,
        None => {
            let shown = envelope::fingerprint(&parsed.signer);
            if !options.interactive {
                return Err(Error::new(format!(
                    "{source_display} is signed by {shown}, which nothing pins: run this in a terminal to confirm it, or add #<signer> to the URL"
                )));
            }
            if !(options.confirm)(&shown) {
                return Err(Error::new(
                    "the vault's signing key was not confirmed; nothing changed",
                ));
            }
            parsed.signer
        }
    };
    let followed = previous
        .as_ref()
        .filter(|previous| same_place && previous.signer == Some(pin))
        .map(|previous| previous.followed.clone())
        .unwrap_or_default();
    let (parsed, checked, _) = envelope::verify_for_reader(&bytes, &name, &pin, &followed, None)?;
    let (pinned, followed) = match checked {
        Pin::Same => (pin, followed),
        Pin::Rotated(new) | Pin::Replaced(new) => (new, parsed.reached_from()),
    };
    let replaced = matches!(checked, Pin::Replaced(_));
    // The vault this Mac read goes on (the same place, name and pin, or a key that pin handed
    // off to): so does the state it holds of it, and so do its other addresses. Any other join
    // starts with none, so another vault of that name is never read at this one's addresses.
    let goes_on = same_place && stored == Some(pin) && !replaced;
    let mirrors = previous
        .as_ref()
        .filter(|_| goes_on)
        .map(|previous| previous.mirrors.clone())
        .unwrap_or_default();
    let config = Config {
        source: source.clone(),
        mirrors,
        name: name.clone(),
        signer: Some(pinned),
        followed,
        host: host.clone(),
    };
    let identity = {
        let _lock = bounded::take(paths, deadline)?;
        creation::permit_join(paths)?;
        expected.check(paths)?;
        // Native possession persists its retry marker under this same StateLock: a slow
        // default HTTP fetch cannot publish over a proof that may have signed admission.
        refuse_pending_native_enrollment(paths, &config)?;
        // Before the reset below clears it, and before this Mac's key is announced or asked
        // with: the vault that dropped this Mac refuses that key.
        if previous
            .as_ref()
            .is_some_and(|previous| joins_again(previous, &name, pin, pinned))
        {
            refuse_dropped(paths)?;
        }
        let identity = machine_identity(paths)?;
        let mut state = load_state(paths)?;
        if !goes_on {
            state.seq = 0;
            state.digest = None;
            state.records_digest = None;
            state.membership = Stored::None;
            state.proof = None;
            state.requested = in_flight();
            expected.check_sharing(paths)?;
            prepare_materialization(paths, &mut state)?;
        } else if previous.as_ref() != Some(&config) {
            if previous
                .as_ref()
                .is_some_and(|previous| previous.host != host)
            {
                state.proof = None;
                state.requested = in_flight();
            }
            expected.check_sharing(paths)?;
            prepare_materialization(paths, &mut state)?;
        }
        expected.check_sharing(paths)?;
        save_config(paths, &config)?;
        identity
    };
    announce(&identity);
    let recipient_text = identity.recipient().to_bech32();
    let code = join_code(&identity.recipient());
    expected.check_sharing(paths)?;
    // The envelope just verified is the version this join applies: no second fetch.
    let synced =
        apply_join_fetched(paths, Fetched::Body(bytes), expected, deadline).inspect_err(|_| {
            let _ = change_request_until_admitted(
                paths,
                &config,
                deadline,
                &|| expected.check_sharing(paths),
                |state| {
                    state.requested = 0;
                    true
                },
            );
        });
    if let SyncOutcome::Applied { seq, report } = synced? {
        return Ok(JoinOutcome::Joined {
            seq,
            names: names_of(&report),
        });
    }
    if let Some(JoinOutcome::Joined { seq, names }) = already_in(paths)? {
        return Ok(JoinOutcome::Joined { seq, names });
    }
    let prove = |key: join::Key| key.prove(&name, &recipient_text, &host);
    let mut by_password = false;
    match &source {
        Source::Url(_) => {
            let key = match (stretching, &options.password) {
                (Some(stretching), _) => Some(
                    stretching
                        .join()
                        .map_err(|_| Error::new("stretching the join password failed"))?,
                ),
                (None, JoinPassword::Ask(ask)) => {
                    ask().map(|password| join::Key::derive(&name, password.as_slice()))
                }
                (None, JoinPassword::Given(_)) => None,
            };
            by_password = key.is_some();
            expected.check_sharing(paths)?;
            request_join_until_admitted(
                paths,
                &config,
                key.map(prove),
                Asker::Join,
                deadline,
                &|| expected.check_sharing(paths),
            )?;
            (options.say)(&JoinEvent::Asked {
                vault: &name,
                code: &code,
                by_password,
            });
            if by_password {
                (options.say)(&JoinEvent::Retype(&mistyped_hint(options, &source, &host)));
            }
        }
        Source::Path(_) => {
            let _lock = bounded::take(paths, deadline)?;
            expected.check_sharing(paths)?;
            let mut state = load_state(paths)?;
            state.membership = Stored::Pending;
            save_state(paths, &state)?;
            (options.say)(&JoinEvent::NotInPath {
                vault: &name,
                recipient: &recipient_text,
                host: &host,
            });
        }
    }
    expected.check_sharing(paths)?;
    (options.requested)();
    if !options.wait.is_zero() {
        (options.say)(&JoinEvent::Waiting);
        let deadline = Instant::now() + options.wait.min(Duration::from_secs(365 * 24 * 60 * 60));
        let mut pause = Duration::ZERO;
        while Instant::now() < deadline {
            expected.check_sharing(paths)?;
            // An admin Mac's verdict (which also ends the server's long-poll): a refused
            // password is asked again at once, or ends a wait that cannot ask. Without one, a
            // look that came back within 5 s waits out the rest of them before the next.
            let refused = by_password
                && read_verdict_within_admitted(paths, Duration::MAX, &|| {
                    expected.check_sharing(paths)
                })
                .unwrap_or(false);
            expected.check_sharing(paths)?;
            if refused {
                (options.say)(&JoinEvent::Refused { vault: &name });
                let JoinPassword::Ask(ask) = &options.password else {
                    break;
                };
                match ask() {
                    Some(password) => {
                        let key = join::Key::derive(&name, password.as_slice());
                        request_join_until_admitted(
                            paths,
                            &config,
                            Some(prove(key)),
                            Asker::Join,
                            None,
                            &|| expected.check_sharing(paths),
                        )?;
                        (options.say)(&JoinEvent::Asked {
                            vault: &name,
                            code: &code,
                            by_password,
                        });
                    }
                    None => by_password = false,
                }
            } else {
                std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
            }
            let started = Instant::now();
            let left = deadline
                .saturating_duration_since(started)
                .min(Duration::from_secs(50));
            let wait = match source {
                Source::Url(_) => Some(left),
                Source::Path(_) => None,
            };
            match sync_join_approval(paths, wait, expected) {
                Ok(SyncOutcome::Applied { seq, report }) => {
                    return Ok(JoinOutcome::Joined {
                        seq,
                        names: names_of(&report),
                    });
                }
                Ok(_) => {
                    if let Some(joined) = already_in(paths)? {
                        return Ok(joined);
                    }
                }
                Err(error) => (options.say)(&JoinEvent::StillWaiting(&error)),
            }
            pause = Duration::from_secs(5).saturating_sub(started.elapsed());
        }
    }
    Ok(JoinOutcome::Pending { code, host })
}

fn apply_join_fetched(
    paths: &Paths,
    fetched: Fetched,
    expected: ExpectedConfig<'_>,
    deadline: Option<Instant>,
) -> crate::Result<SyncOutcome> {
    apply_until_admitted(
        paths,
        configured(paths)?,
        load_state(paths)?,
        ApplyInput::Response(Ok(fetched.into())),
        None,
        deadline,
        (false, &|| expected.check_sharing(paths)),
    )
}

/// An admitted join's bounded confirmation reads only its selected source. Its subscriber
/// owns broader peer repair; every received response still checks the same authority under
/// the materialization lock. Ordinary explicit joins keep their existing sync policy.
fn sync_join_approval(
    paths: &Paths,
    wait: Option<Duration>,
    expected: ExpectedConfig<'_>,
) -> crate::Result<SyncOutcome> {
    if !matches!(
        expected,
        ExpectedConfig::AdmittedAny(_) | ExpectedConfig::AdmittedRead(..)
    ) {
        return sync(paths, wait);
    }
    expected.check_sharing(paths)?;
    let config = configured(paths)?;
    let before = load_state(paths)?;
    let fetched = match &config.source {
        Source::Url(url) => transport::fetch_within(
            url,
            wait.and_then(|_| long_poll_after(&config, &before)),
            wait.unwrap_or(servers_within(Duration::from_secs(30))),
        )?,
        Source::Path(_) => transport::fetch(&config.source, None, servers_within(PROBE_DEADLINE))?,
    };
    apply_until_admitted(
        paths,
        config,
        before,
        ApplyInput::Response(Ok(fetched.into())),
        None,
        None,
        (false, &|| expected.check_sharing(paths)),
    )
}

fn refuse_pending_native_enrollment(paths: &Paths, config: &Config) -> crate::Result<()> {
    if alab_vault(config) && crate::members::join::pending_proof(paths)? {
        return Err(Error::new(
            "a native member may have signed this Mac's admission; the same proof retries before any vault enrollment",
        ));
    }
    Ok(())
}

/// Whether `bytes`, what the caller fetched from `source` earlier, still serve a join there:
/// not when this machine holds a newer version of that vault, which a fetch now serves.
fn fetched_serves(paths: &Paths, source: &Source, name: &str, bytes: &[u8]) -> crate::Result<bool> {
    if !matches!(source, Source::Url(_)) {
        return Ok(false);
    }
    let held = match self::config(paths)? {
        Some(config) if config.source == *source && config.name == name => load_state(paths)?.seq,
        _ => 0,
    };
    Ok(envelope::parse(bytes).is_ok_and(|parsed| parsed.seq >= held))
}

fn mistyped_hint(options: &JoinOptions<'_>, source: &Source, host: &str) -> String {
    let mut again = String::from("aterm secrets join");
    let other = !matches!(source, Source::Url(url) if crate::is_alab_default_url(url));
    if other {
        again.push_str(" <vault-url>");
    }
    if options.host.is_some() {
        again.push_str(&format!(" --name {host}"));
    }
    let fix = if options.interactive {
        format!("run {again} again, and it asks again")
    } else {
        format!(
            "pipe it again to {again} --password-stdin, or run {again} in a terminal, which asks for it"
        )
    };
    let source_note = if other {
        "; replace <vault-url> with this vault's full configured URL, including any query or signer pin"
    } else {
        ""
    };
    format!(
        "if no approval comes within a minute, the join password may be mistyped: {fix}{source_note}"
    )
}

fn already_in(paths: &Paths) -> crate::Result<Option<JoinOutcome>> {
    let state = load_state(paths)?;
    Ok(
        (state.membership == Stored::In).then(|| JoinOutcome::Joined {
            seq: state.seq,
            names: registry_names(&state),
        }),
    )
}

/// An open vault, as an admin sees it: text, private authority custody and source.
pub struct Admin {
    /// The vault text.
    pub doc: Document,
    /// The source config.
    pub config: Config,
    /// Authenticated baseline, unaffected by edits through the public draft.
    published_doc: Document,
    /// The exact accepted envelope/log source of the baseline, before pending local writes.
    version: version::AuthenticatedVersion,
    /// Control metadata from the authenticated envelope, before the log overlay.
    published_control: inspection::PublishedControl,
    /// The age recipients the opened version's own `[machines]` names, before the membership
    /// log's overlay adds the Macs a member let in: the Macs that version's lockers address.
    served_machines: BTreeSet<String>,
    commons_eligibility: crate::commons::membership::Eligibility,
    admissions: admission::Ledger,
    seed: [u8; 32],
    commons_seed: [u8; 32],
    commons_bound: bool,
    /// The admin locker's format as opened: a publish writes [`admin_keys::MAGIC`].
    custody: &'static str,
    seq: u64,
    /// The handoff chain from the vault's first key to the current one, carried whole on
    /// every version, so a machine pinned to any earlier key (asleep across rotations, or a
    /// new Mac whose build pins an older key) still follows.
    handoff: Vec<([u8; 32], [u8; 64])>,
    /// How many handoff pairs the version opened carried (fewer than `handoff` when this
    /// machine's copy put back links that version dropped).
    opened_pairs: usize,
    /// Bounded ancestor head digests (`[opened_head, ...prior_parents]`) for parent-CAS publishing.
    opened_heads: Vec<[u8; 32]>,
    /// A parented vault must not return to parentless publication.
    parented: bool,
    /// How this Mac's log stood to the version opened: an edit is written to the log only from
    /// a base the log holds ([`log_entries::LogBase::Covers`]) or from one a writer with no seat
    /// published ([`log_entries::LogBase::Unstamped`], whose entries the edit's records import).
    log_base: log_entries::LogBase,
    /// Entries a person saved ([`Admin::resolving`]): the publish settles a conflict the log holds
    /// for each.
    resolve: BTreeSet<(Audience, String)>,
    /// This Mac's `[everyone]` writes waiting for a removal's new key that `doc` shows over the
    /// log's values ([`Admin::waiting`]), which a publish drafts against (one-mesh W3).
    shown: log_entries::Waiting,
    /// This Mac's waiting writes a later change overtook that no view showed yet
    /// ([`Admin::overtaken`]).
    overtaken: Vec<Overtaken>,
}

impl Drop for Admin {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.seed);
        zeroize::Zeroize::zeroize(&mut self.commons_seed);
    }
}

impl std::fmt::Debug for Admin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Admin({} seq {})", self.config.name, self.seq)
    }
}

impl Admin {
    /// Immutable authenticated baseline, excluding edits to the public draft.
    pub fn published_document(&self) -> &Document {
        &self.published_doc
    }

    /// The logical authenticated version of the immutable accepted baseline for this requester.
    /// Progress permits another consumer attempt; it does not prove that an entry changed or
    /// that its consumer accepted it. Public draft edits cannot advance this version.
    /// A local waiting write has not entered the accepted log yet; other entries remain usable.
    ///
    /// # Errors
    /// A requested entry is supplied by a waiting write whose accepted envelope origin is not
    /// retained in the waiting view. The ordinary vault pass makes it covered once signed.
    pub fn version_for_requester(
        &self,
        recipient: &age::Recipient,
        entries: &[&str],
    ) -> crate::Result<version::AuthenticatedVersion> {
        let host = self
            .host_of_recipient(recipient)
            .ok_or_else(|| Error::new("the authenticated vault names no requester"))?;
        let baseline = self.published_doc.entries();
        let received = crate::text::merge_for(&host, baseline.clone())
            .map_err(|error| Error::new(error.to_string()))?;
        for name in entries {
            // An existing recipient reads its host override, then everyone. A missing
            // recipient entry is copied from the first baseline occurrence, as copy_into_host.
            let source = received
                .iter()
                .find(|entry| entry.name == *name)
                .or_else(|| baseline.iter().find(|entry| entry.name == *name));
            if self.shown.contains(name)
                && source.is_none_or(|source| source.audience == Audience::Everyone)
            {
                return Err(Error::new(
                    "the requested credential still waits for its authenticated log write",
                ));
            }
        }
        Ok(self.version.clone())
    }

    /// The `[everyone]` entries `doc` shows as this Mac's writes waiting for a removal's new key
    /// (one-mesh M4, W3): each as this Mac's waiting write leaves it, over the value the log holds.
    /// A vault pass signs each once the key arrives; a publish that changes one, or sets it back to
    /// the log's value (or removes one the log does not hold), takes it back.
    pub fn waiting(&self) -> Vec<String> {
        self.shown.names()
    }

    /// This Mac's waiting writes a later change overtook, and a vault pass dropped, that no view
    /// showed yet: a record of each, never a value ([`Overtaken::line`]). [`overtaken_shown`]
    /// marks them shown.
    pub fn overtaken(&self) -> &[Overtaken] {
        &self.overtaken
    }

    /// Entry from the authenticated snapshot, unaffected by edits to `doc`.
    pub fn published_entry(&self, audience: &Audience, name: &str) -> Option<&crate::text::Entry> {
        self.published_doc.entry(audience, name)
    }

    /// Mark `name` in `audience` as a person's save (`aterm secrets set`): where the membership
    /// log holds concurrent values for it, the next publish settles them on the value the edited
    /// text gives it, even where that is the value every Mac already writes. Nothing else settles
    /// a conflict: `aterm secrets status` shows it until a person saves or removes the entry.
    pub fn resolving(&mut self, audience: &Audience, name: &str) {
        self.resolve.insert((audience.clone(), name.to_owned()));
    }

    /// Admission metadata from the authenticated published snapshot. A row is
    /// historical provenance, not current membership or a transferable grant.
    pub fn commons_admission(&self, host: &str) -> Option<AdmissionEvidence> {
        let machine = self
            .published_doc
            .machines()
            .into_iter()
            .find(|m| m.name == host)?;
        self.admissions
            .evidence(&Recipient::parse(&machine.recipient).ok()?)
    }

    /// Automatically certify only a proven recipient with durable admission
    /// evidence under this root, an open-vault admission included. Current
    /// audience, root, retirement and possession checks still apply.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Password",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Manual",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "OpenClosed",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Removed",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Readded",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "GenericEdit",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Rotate",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Rename",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Excluded",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Legacy",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Draft",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "InvalidProof",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "ForeignRoot",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAdmissionEvidence",
            action = "Founder",
            project = "aterm_secrets::client::admission::tests::admission_transition"
        )
    )]
    pub fn auto_approve_commons_member(
        &self,
        journal: &crate::commons::journal::Journal,
        host: &str,
        possession: crate::commons::binding::Possession,
    ) -> crate::Result<crate::commons::membership::Certificate> {
        if !self
            .admissions
            .permits(journal.trust(), &possession.binding().recipient())
        {
            return Err(Error::new(
                "Commons enrollment needs authenticated admission evidence",
            ));
        }
        self.approve_commons_member(journal, host, possession)
    }

    /// The public Commons authority, whose private custody survives vault signer
    /// rotations. Legacy lockers retain their original shared signing seed.
    pub fn commons_authority(&self) -> crate::Result<[u8; 32]> {
        envelope::signer_public(&self.commons_seed)
    }

    /// Prepare an initial typed Commons roster from consumed joint-possession
    /// proofs and the authenticated eligibility snapshot captured on open.
    /// Use only for a new network and durably publish its returned genesis as
    /// the authenticated trust anchor; an unavailable anchor is not a new lab.
    /// Retain and ingest the exact returned bytes before publishing them.
    pub fn prepare_commons_genesis(
        &self,
        network: [u8; 16],
        members: Vec<crate::commons::roster::Enrollment>,
    ) -> crate::Result<crate::commons::node::Node> {
        crate::commons::roster::create(
            &self.commons_seed,
            &self.commons_eligibility,
            network,
            members,
        )
    }

    /// Prepare a fresh key epoch and typed roster extending the exact current
    /// control head. An omitted device is absent from this roster; permanent
    /// retirement is a separate retained control fact. Persist and retry the
    /// exact returned bytes rather than preparing another random epoch.
    pub fn prepare_commons_rotation(
        &self,
        journal: &crate::commons::journal::Journal,
        members: Vec<crate::commons::roster::Grant>,
    ) -> crate::Result<crate::commons::node::Node> {
        crate::commons::roster::rotate(
            &self.commons_seed,
            &self.commons_eligibility,
            journal,
            members,
        )
    }

    /// Prepare one atomic retirement and fresh-key roster update. Retired actors
    /// must have retained bindings and must be absent from the resulting roster.
    pub fn prepare_commons_retirement(
        &self,
        journal: &crate::commons::journal::Journal,
        members: Vec<crate::commons::roster::Grant>,
        retirements: &[[u8; 48]],
    ) -> crate::Result<crate::commons::node::Node> {
        crate::commons::roster::rotate_retiring(
            &self.commons_seed,
            &self.commons_eligibility,
            journal,
            members,
            retirements,
        )
    }

    /// Explicitly approve a jointly proven Commons device binding. This is an
    /// authority decision, not automatic eligibility from the vault machine list.
    /// The host must already belong to the explicit Commons control audience.
    /// Eligibility is the authenticated snapshot captured on open; reopen after
    /// publishing edits. Unpublished changes to `doc` cannot approve a member.
    /// The certificate is inactive until a validated current roster includes it.
    pub fn approve_commons_member(
        &self,
        journal: &crate::commons::journal::Journal,
        host: &str,
        possession: crate::commons::binding::Possession,
    ) -> crate::Result<crate::commons::membership::Certificate> {
        crate::commons::membership::approve(
            &self.commons_seed,
            journal,
            &self.commons_eligibility,
            host,
            possession,
        )
    }

    /// Prepare a domain-separated control node without exporting its authority key.
    /// Persist and retry its exact bytes: a freshly encrypted draft is a distinct
    /// event. Named recipients must already be in this admin's document; callers
    /// still decide which machines qualify for Commons membership.
    pub fn seal_commons(
        &self,
        network: [u8; 16],
        edit: crate::commons::node::Edit<'_>,
    ) -> crate::Result<crate::commons::node::Node> {
        self.seal_commons_restricted(network, edit, &[])
    }

    /// Seal a control snapshot with permanent, independently readable restrictions.
    /// Recovery cannot add facts; all prior facts survive its payload selection. This has no
    /// control history to gate on, so it stays within what 0.145 verifies (C26).
    pub fn seal_commons_restricted(
        &self,
        network: [u8; 16],
        edit: crate::commons::node::Edit<'_>,
        facts: &[crate::commons::constraints::Fact],
    ) -> crate::Result<crate::commons::node::Node> {
        use crate::commons::node::{Edit, Node};
        crate::commons::hearing::Gate::admit(
            None,
            facts.len(),
            false,
            [] as [&crate::commons::binding::Binding; 0],
        )?;
        let recipients = match &edit {
            Edit::Create { hosts, .. } | Edit::Update { hosts, .. } => {
                recipients_of(hosts, &self.doc)?
            }
            Edit::Recover { .. } => Vec::new(),
        };
        let authored_ms = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("system clock cannot represent an author timestamp"))?;
        if facts.is_empty() {
            Node::seal(&self.commons_seed, network, edit, &recipients, authored_ms)
        } else {
            Node::seal_restricted(
                &self.commons_seed,
                network,
                edit,
                &recipients,
                authored_ms,
                facts,
            )
        }
    }

    /// The version this was opened at.
    pub const fn seq(&self) -> u64 {
        self.seq
    }
}

fn seq_of(bytes: &[u8]) -> Option<u64> {
    envelope::parse(bytes).ok().map(|parsed| parsed.seq)
}

/// Open as an admin: fetch or restore the latest verified envelope and decrypt its admin locker.
pub fn admin_open(paths: &Paths) -> crate::Result<Admin> {
    admin_open_with_candidates(paths, &[])
}

/// Hang detector shared by a required vault primary response and its original caller.
const REQUIRED_PRIMARY_HANG: Duration = Duration::from_secs(60);

fn fetch_required_primary(
    source: &Source,
    reader: Option<&str>,
    started: Instant,
) -> crate::Result<Fetched> {
    let left = (started + REQUIRED_PRIMARY_HANG).saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Error::new(
            "required vault primary response did not finish before its hang detector",
        ));
    }
    match source {
        Source::Url(url) => transport::fetch_within_as(url, None, left, reader),
        Source::Path(_) => transport::fetch_as(source, None, left, reader),
    }
}

#[cfg(test)]
#[path = "client/primary_wait_tests.rs"]
mod primary_wait_tests;

/// [`admin_open`], also probing candidate servers (`aterm-swarm-policy`'s `servers`, live ALab
/// peers, and `extra_urls`) when `config.source` is unreachable, empty, or lagging behind a
/// descendant head on a peer server (`docs/DESIGN-fluid-swarm-2026-10-28.md` F6).
///
/// # Errors
/// Vault configuration or admin locker is missing, unreadable, or refused by verification;
/// no verified envelope is available;
/// the primary response exhausts its hang detector, or its fetch fails and no candidate supplies
/// a verified envelope; or the authenticated history or admin authorization refuses the opening.
pub fn admin_open_with_candidates(paths: &Paths, extra_urls: &[String]) -> crate::Result<Admin> {
    admin_open_with_candidates_started(paths, extra_urls, Instant::now())
}

fn admin_open_with_candidates_started(
    paths: &Paths,
    extra_urls: &[String],
    started: Instant,
) -> crate::Result<Admin> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    let pin = config
        .signer
        .ok_or_else(|| Error::new("a plain text file has no admins: edit the file itself"))?;
    let state = load_state(paths)?;
    let held = floor(paths, &state, &config.name, &config);
    let verifies = |bytes: &[u8]| {
        envelope::verify_for_reader(bytes, &config.name, &pin, &config.followed, None).is_ok()
    };
    let cached = chain_cache::held(&paths.cache_file())?
        .map(|parsed| parsed.bytes().to_vec())
        .filter(|bytes| verifies(bytes));
    let mut primary_reachable = true;
    // Every read here names this Mac's recipient, as sync's do: a server's heard history then
    // counts it as the member it is, never as a plain read a `0.145` Mac may have made, which
    // would keep the F20 mirror on while an admin opens the vault routinely (the approver's pass,
    // the ALab release check, the steward's lease).
    let reader = subscriber_recipient(paths);
    let reader = reader.as_deref();
    // What every candidate server past the source serves that verifies, read at once within one
    // probe deadline and verified as it is read ([`fan_out::bodies_within`]): one after another,
    // each server that takes the read and never answers held every admin command for a probe
    // deadline of its own. In the candidates' order, so each choice below falls as it did read by
    // read.
    let candidate_bodies = || {
        let mut urls = ledger_publish::candidate_server_urls(paths, &config, extra_urls);
        if !urls.is_empty() {
            urls.remove(0);
        }
        fan_out::bodies_within(&urls, servers_within(PROBE_DEADLINE), reader, |bytes| {
            verifies(&bytes).then_some(bytes)
        })
        .into_iter()
        .flatten()
    };
    let served = match fetch_required_primary(&config.source, reader, started) {
        Ok(Fetched::Body(bytes)) => {
            let mut chosen = bytes;
            if matches!(config.source, Source::Url(_))
                && verifies(&chosen)
                && let Ok(primary_env) = envelope::parse(&chosen)
            {
                let primary_digest = primary_env.digest();
                let mut best_seq = primary_env
                    .seq
                    .max(cached.as_deref().and_then(seq_of).unwrap_or(0));
                for cand_bytes in candidate_bodies() {
                    if let Ok(cand_env) = envelope::parse(&cand_bytes)
                        && cand_env.seq > best_seq
                        && (cand_env.parents.is_empty()
                            || cand_env.parents.contains(&primary_digest))
                    {
                        best_seq = cand_env.seq;
                        chosen = cand_bytes;
                    }
                }
            }
            Some(chosen)
        }
        Ok(Fetched::Missing | Fetched::NoNewer(_)) => {
            let mut best: Option<(u64, Vec<u8>)> = None;
            if matches!(config.source, Source::Url(_)) {
                for cand_bytes in candidate_bodies() {
                    if let Some(cand_seq) = seq_of(&cand_bytes)
                        && best.as_ref().is_none_or(|(s, _)| cand_seq > *s)
                    {
                        best = Some((cand_seq, cand_bytes));
                    }
                }
            }
            best.map(|(_, b)| b)
        }
        Err(primary_err) => {
            if !matches!(config.source, Source::Url(_)) {
                return Err(primary_err);
            }
            primary_reachable = false;
            let mut best: Option<(u64, Vec<u8>)> = None;
            for cand_bytes in candidate_bodies() {
                if let Some(cand_seq) = seq_of(&cand_bytes)
                    && best.as_ref().is_none_or(|(s, _)| cand_seq > *s)
                {
                    best = Some((cand_seq, cand_bytes));
                }
            }
            match best {
                Some((_, b)) => Some(b),
                None => return Err(primary_err),
            }
        }
    };
    let known: Vec<[u8; 32]> = std::iter::once(pin)
        .chain(config.followed.iter().copied())
        .collect();
    let foreign = |served: &[u8]| match envelope::parse(served) {
        Err(_) => true,
        Ok(served) => {
            let retired = Retired::compiled();
            served.verify_signature().is_err()
                || served.check_signer(&retired).is_err()
                || retired.gives_way(&served.signer, &chain_keys(&served))
                || config.followed.contains(&served.signer)
                || served.forges_handoff_from(&known)
        }
    };
    let mut put_back: Option<Option<Vec<u8>>> = None;
    let bytes = match (served, cached) {
        (Some(served), Some(_))
            if matches!(config.source, Source::Path(_))
                && !verifies(&served)
                && !foreign(&served) =>
        {
            let place = match &config.source {
                Source::Path(path) => path.display().to_string(),
                Source::Url(url) => transport::describe(url),
            };
            let served = envelope::parse(&served)?;
            let aside = "move the file aside and the next admin command puts this machine's \
                         copy back";
            return Err(Error::new(if served.name == config.name {
                format!(
                    "{place} holds a version of {} signed by {}, a key this machine does not \
                     follow: a fork, or a rotation by a build before handoff chains. If it is \
                     not the vault, {aside}; if it is, `aterm secrets status` on an admin Mac \
                     that holds it names its key, and if that key's fingerprint is this one, \
                     aterm secrets join '{place}#<that key>' follows it",
                    config.name,
                    envelope::fingerprint(&served.signer)
                )
            } else {
                format!(
                    "{place} holds the vault {}, not {}: if this machine's vault belongs there, \
                     {aside}",
                    served.name, config.name
                )
            }));
        }
        (Some(served), Some(cached))
            if !verifies(&served)
                || seq_of(&cached).unwrap_or(0) > seq_of(&served).unwrap_or(0) =>
        {
            put_back = Some((!verifies(&served) && foreign(&served)).then_some(served));
            cached
        }
        (Some(served), _) => served,
        (None, Some(cached)) => {
            put_back = Some(None);
            cached
        }
        (None, None) => return Err(missing(&config)),
    };
    let unchained = held.as_ref().map(|floor| Floor {
        chain: None,
        ..floor.clone()
    });
    let (envelope, pinned, _) = envelope::verify_for_reader(
        &bytes,
        &config.name,
        &pin,
        &config.followed,
        unchained.as_ref(),
    )?;
    let handoff = match held.and_then(|floor| floor.chain) {
        Some((signer, chain)) if !matches!(pinned, Pin::Replaced(_)) => {
            envelope.chain_carried_on(&signer, &chain)?
        }
        _ => envelope.handoff.clone(),
    };
    let identity = machine_identity(paths)?;
    let locker = envelope
        .admin_locker()
        .ok_or_else(|| Error::new("this vault has no admin locker"))?;
    let plain = match age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes()) {
        Ok(plain) => plain,
        Err(age::AgeError::NoMatch) => {
            return Err(Error::new(format!(
                "this machine is not an admin of {}: an admin can run aterm secrets admins add {}",
                config.name,
                join_code(&identity.recipient())
            )));
        }
        Err(error) => return Err(Error::new(format!("admin locker: {error}"))),
    };
    let opened = parse_admin(&plain)?;
    opened.admissions.validate(&opened.doc, envelope.seq)?;
    if envelope::signer_public(&opened.seed)? != envelope.signer {
        return Err(Error::new(
            "the admin locker's key does not match the envelope's signer",
        ));
    }
    if matches!(config.source, Source::Url(_)) || !extra_urls.is_empty() {
        let _ = sync_records_with_candidates(paths, None, extra_urls);
    }
    if admin_removed_in_log(paths, &config, &envelope, &identity, &opened.doc) {
        return Err(Error::new(format!(
            "this machine is not an admin of {}: an admin can run aterm secrets admins add {}",
            config.name,
            join_code(&identity.recipient())
        )));
    }
    if primary_reachable && let Some(over) = put_back {
        let _lock = configured_admin(paths, &config)?;
        match over {
            Some(over) => transport::replace(&config.source, &bytes, &over)?,
            None => transport::publish(&config.source, &bytes)?,
        }
    }
    let served_machines = opened
        .doc
        .machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .collect();
    let mut doc = opened.doc.clone();
    let mut admissions = opened.admissions.clone();
    let AdminBase {
        log_base,
        shown,
        version,
    } = admin_base(
        paths,
        &config,
        &envelope,
        &handoff,
        &identity,
        &opened,
        &mut doc,
        &mut admissions,
        true,
    )?;
    Ok(Admin {
        commons_eligibility: crate::commons::membership::Eligibility::from_document(&doc),
        admissions,
        published_doc: doc.clone(),
        version,
        published_control: inspection::PublishedControl::read(&opened.doc)?,
        served_machines,
        doc,
        config,
        seed: opened.seed,
        commons_seed: opened.commons_seed,
        commons_bound: opened.commons_bound,
        custody: opened.custody,
        seq: envelope.seq,
        handoff,
        opened_pairs: envelope.handoff.len(),
        opened_heads: ledger_publish::extend_parent_spine(
            &[envelope.digest()],
            &[envelope.parents.as_slice()],
        ),
        parented: !envelope.parents.is_empty(),
        log_base,
        resolve: BTreeSet::new(),
        shown,
        overtaken: unshown_overtaken(paths),
    })
}

/// The records of this Mac's journal of overtaken writes no view showed yet ([`Admin::overtaken`]);
/// none where the journal cannot be read.
fn unshown_overtaken(paths: &Paths) -> Vec<Overtaken> {
    log_entries::read_overtaken(paths)
        .unwrap_or_default()
        .into_iter()
        .filter(|record| !record.shown)
        .collect()
}

/// A file of this Mac's `[everyone]` writes that it cannot read ([`waiting_unread`]).
pub enum UnreadWaiting {
    /// The writes waiting for a removal's new key: no save that writes `[everyone]` goes until
    /// they read, since any write it makes may be over one of them.
    Waiting(String),
    /// A publish's pending set: no save whose `[everyone]` writes wait for the new key, or
    /// replace a waiting one, goes until it reads, since it may be a stored version's writes.
    Pending(String),
}

/// The files of this Mac's `[everyone]` writes waiting for a removal's new key, or pending on a
/// publish's version, that it cannot read: while one is there, nothing waiting is signed or
/// landed over it (one-mesh W3). Empty where every one reads, or this Mac has no identity.
pub fn waiting_unread(paths: &Paths) -> Vec<UnreadWaiting> {
    let Ok(Some(identity)) = read_identity(paths) else {
        return Vec::new();
    };
    let (waiting, pending) = log_entries::unread_waiting(paths, &identity);
    waiting
        .map(UnreadWaiting::Waiting)
        .into_iter()
        .chain(pending.into_iter().map(UnreadWaiting::Pending))
        .collect()
}

/// Mark `admin`'s overtaken records ([`Admin::overtaken`]) shown: a person read them, and no
/// later view shows them again. They stay in the journal, a record a person may read.
///
/// # Errors
/// The state lock is not taken, or the journal cannot be read or written.
pub fn overtaken_shown(paths: &Paths, admin: &Admin) -> crate::Result<()> {
    log_entries::overtaken_seen(paths, &admin.overtaken)
}

struct AdminBase {
    log_base: log_entries::LogBase,
    shown: log_entries::Waiting,
    version: version::AuthenticatedVersion,
}

/// How this Mac's seeded log stands to the version `envelope` an admin opened, with `doc` (that
/// version's text) given the log's entries where the log holds the version. A version a writer
/// with no seat in the log published, or one whose records this log lacks, is edited as it
/// stands: the log never reverts or deletes what it does not hold. Where the log's entries do not
/// fit the version's text (the overlay fails), the version reads as one the log is behind
/// ([`log_entries::LogBase::Behind`]), so no edit is diffed into the log from that text. Where
/// `land`, the sets pending on the version opened land first (`log_entries::view_waiting`); a
/// cached open, whose callers may hold the state lock, passes none.
#[expect(
    clippy::too_many_arguments,
    reason = "the opened version, the text and ledger it fills, and whether its sets land"
)]
fn admin_base(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    handoff: &[([u8; 32], [u8; 64])],
    identity: &Identity,
    opened: &admin_keys::AdminText,
    doc: &mut Document,
    admissions: &mut admission::Ledger,
    land: bool,
) -> crate::Result<AdminBase> {
    let version = |log| {
        version::AuthenticatedVersion::from_source(
            &config.name,
            envelope.signer,
            handoff.iter().map(|(key, _)| *key),
            Some(version::EnvelopeVersion {
                seq: envelope.seq,
                digest: envelope.digest(),
            }),
            log,
        )
    };
    // An unreadable records file or locker opens the version as it stands, never fails the open
    // (`admin_cached` answers whether this Mac is an admin at all).
    let seeded = log_entries::trusted_seeded_log(paths, config, envelope)
        .ok()
        .flatten();
    let base = log_entries::log_base(seeded.as_ref().map(|(log, _)| log), envelope, identity)
        .unwrap_or(log_entries::LogBase::Behind);
    let mut shown = log_entries::Waiting::default();
    if base == log_entries::LogBase::Covers
        && let Some((log, _)) = &seeded
    {
        let mut overlaid = doc.clone();
        if !log.removes(&identity.recipient())
            && !log_entries::recipient_denied_by_roster_or_log(None, log, &identity.recipient())
            && !log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
            && log_entries::overlay_log_onto_document(log, identity, &mut overlaid, None).is_ok()
            && admin_keys::validate_commons(&overlaid, &opened.commons_seed).is_ok()
            && let Ok(next_admissions) = opened.admissions.retained(&overlaid)
            && next_admissions.validate(&overlaid, envelope.seq).is_ok()
        {
            *doc = overlaid;
            *admissions = next_admissions;
        } else {
            // The version's text without the log's entries is no base to diff the log against:
            // an edit drafted from it would write the version's older values over every log-only
            // edit. The edit goes out unstamped, read from its lockers, as from a log behind it.
            return Ok(AdminBase {
                log_base: log_entries::LogBase::Behind,
                shown,
                version: version(None)?,
            });
        }
        // One-mesh W3: this Mac's `[everyone]` writes that wait for a removal's new key show over
        // the log's values, so a person sees them and can change or take them back; the edit is
        // drafted against this view (`log_entries::everyone_edit`). The sets pending on the
        // version opened, which a server holds, land first. Where they do not fit the text, the
        // view is the log's alone.
        let waiting = log_entries::view_waiting(
            paths,
            identity,
            log,
            &log_entries::stored_versions(envelope),
            land,
        );
        let mut with = doc.clone();
        if !waiting.is_empty()
            && log_entries::overlay_waiting_onto_document(&mut with, &waiting).is_ok()
            && admin_keys::validate_commons(&with, &opened.commons_seed).is_ok()
            && let Ok(with_admissions) = opened.admissions.retained(&with)
            && with_admissions.validate(&with, envelope.seq).is_ok()
        {
            *doc = with;
            *admissions = with_admissions;
            shown = waiting;
        }
    } else if base == log_entries::LogBase::Unstamped
        && let Some((log, _)) = &seeded
    {
        let mut overlaid = doc.clone();
        if !log.removes(&identity.recipient())
            && !log_entries::recipient_denied_by_roster_or_log(None, log, &identity.recipient())
            && !log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
            && log_entries::overlay_log_commons_onto_document(log, identity, &mut overlaid, None)
                .is_ok()
            && admin_keys::validate_commons(&overlaid, &opened.commons_seed).is_ok()
            && let Ok(next_admissions) = opened.admissions.retained(&overlaid)
            && next_admissions.validate(&overlaid, envelope.seq).is_ok()
        {
            *doc = overlaid;
            *admissions = next_admissions;
        } else {
            return Ok(AdminBase {
                log_base: log_entries::LogBase::Behind,
                shown,
                version: version(None)?,
            });
        }
    }
    let accepted_log = if base == log_entries::LogBase::Covers {
        seeded.as_ref().map(|(log, _)| log)
    } else {
        None
    };
    Ok(AdminBase {
        log_base: base,
        shown,
        version: version(accepted_log)?,
    })
}

fn recipients_of(names: &[String], doc: &Document) -> crate::Result<Vec<Recipient>> {
    let machines = doc.machines();
    let mut out = Vec::new();
    for name in names {
        let machine = machines
            .iter()
            .find(|machine| &machine.name == name)
            .ok_or_else(|| Error::new(format!("{name} is not in [machines]")))?;
        out.push(
            Recipient::parse(&machine.recipient)
                .map_err(|_| Error::new(format!("{name}: bad key")))?,
        );
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "NewVault",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "VaultRotation",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "LegacyRotation",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "ChangedDescriptor",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
fn seal(
    name: &str,
    seq: u64,
    seed: &[u8; 32],
    commons_seed: &[u8; 32],
    commons_bound: bool,
    doc: &Document,
    handoff: &[([u8; 32], [u8; 64])],
) -> crate::Result<Vec<u8>> {
    seal_with_admissions(
        name,
        seq,
        seed,
        commons_seed,
        commons_bound,
        doc,
        handoff,
        &admission::Ledger::default().retained(doc)?,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "vault seal has independent custody and admission provenance"
)]
fn seal_with_admissions(
    name: &str,
    seq: u64,
    seed: &[u8; 32],
    commons_seed: &[u8; 32],
    commons_bound: bool,
    doc: &Document,
    handoff: &[([u8; 32], [u8; 64])],
    admissions: &admission::Ledger,
) -> crate::Result<Vec<u8>> {
    seal_with_admissions_and_parents(
        name,
        seq,
        seed,
        commons_seed,
        commons_bound,
        doc,
        handoff,
        &[],
        admissions,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "vault seal has independent custody, parent-CAS digests, and admission provenance"
)]
fn seal_with_admissions_and_parents(
    name: &str,
    seq: u64,
    seed: &[u8; 32],
    commons_seed: &[u8; 32],
    commons_bound: bool,
    doc: &Document,
    handoff: &[([u8; 32], [u8; 64])],
    parents: &[[u8; 32]],
    admissions: &admission::Ledger,
) -> crate::Result<Vec<u8>> {
    seal_stamped(
        name,
        seq,
        seed,
        commons_seed,
        commons_bound,
        doc,
        handoff,
        admissions,
        None,
        parents,
    )
}

/// Seal `doc` as version `seq`, with `stamp` ([`log_entries::stamp_line`]) after the header of
/// every locker but the admin one, where the writer's log holds the entries it seals.
#[expect(
    clippy::too_many_arguments,
    reason = "vault seal has independent custody, admission provenance, and the log stamp"
)]
fn seal_stamped(
    name: &str,
    seq: u64,
    seed: &[u8; 32],
    commons_seed: &[u8; 32],
    commons_bound: bool,
    doc: &Document,
    handoff: &[([u8; 32], [u8; 64])],
    admissions: &admission::Ledger,
    stamp: Option<&str>,
    parents: &[[u8; 32]],
) -> crate::Result<Vec<u8>> {
    admin_keys::validate_commons(doc, commons_seed)?;
    admissions.validate(doc, seq)?;
    let admins = recipients_of(&doc.admins(), doc)?;
    if admins.is_empty() {
        return Err(Error::new("the vault needs at least one admin"));
    }
    let mut lockers = vec![Locker {
        label: "admin".to_string(),
        armored: age::armor(&age::encrypt(
            &admins,
            admin_keys::admin_plaintext_with_admissions(
                seed,
                commons_seed,
                commons_bound || admin_keys::has_control(doc),
                doc,
                admissions,
            )
            .as_slice(),
        )?),
    }];
    for (index, plan) in doc.lockers().into_iter().enumerate() {
        let mut recipients = Vec::new();
        for text in &plan.recipients {
            recipients.push(Recipient::parse(text).map_err(|_| Error::new("bad machine key"))?);
        }
        if recipients.is_empty() {
            continue;
        }
        let stamped = stamp.map(|stamp| log_entries::stamped_text(plan.text.as_slice(), stamp));
        let text = stamped
            .as_ref()
            .map_or(plan.text.as_slice(), |text| text.as_slice());
        lockers.push(Locker {
            label: (index + 1).to_string(),
            armored: age::armor(&age::encrypt(&recipients, text)?),
        });
    }
    // Ordinary unparented vaults retain their legacy encoding. Conditional
    // publication names its exact opened head, and parented vaults retain it.
    envelope::sign(
        &Unsigned {
            name: name.to_string(),
            seq,
            time: now(),
            handoff: handoff.to_vec(),
            parents: parents.to_vec(),
            lockers,
        },
        seed,
    )
}

/// What a publish did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The version published.
    pub seq: u64,
    /// Exact published envelope and its drafted log frontier, never a later source observation.
    version: version::VersionClaim,
    /// Whether it is signed by a new vault key the old one endorsed.
    pub rotated: bool,
    /// Compiled-in keys required in the next release after an ALab vault key rotation.
    pub alab_release: Option<AlabRelease>,
    /// Whether this is the first version of the vault to carry two or more handoff pairs.
    pub first_long_chain: bool,
    /// Whether the join password was automatically rotated after removing a machine or admin.
    pub password_renewed: bool,
    /// Whether a machine removed from an open vault was added to `code-only`.
    pub kept_out: bool,
    /// Whether the vault is served at a URL, where a new key asks to join through its queue (a
    /// vault at a path lets in none without an admin).
    pub queued: bool,
    /// Waiting join requests added to `code-only` when opening the vault.
    pub held: Vec<Request>,
    /// Why this machine's own sync after the publish failed, if it did. The publish stands.
    pub local: Option<String>,
    /// The `[everyone]` entries this publish changed that wait for a removal's new key (one-mesh
    /// M4): this Mac's vault pass signs them once the key arrives, and until then a view shows
    /// them as waiting ([`Admin::waiting`]). A take-back waits with them, unnamed: it is done once
    /// its version lands, and nothing is signed for it.
    pub waiting: Vec<String>,
}

impl Published {
    /// The exact authenticated publication, for a signed delivery record.
    #[must_use]
    pub const fn version(&self) -> &version::VersionClaim {
        &self.version
    }
}

/// Compiled-in keys required after an ALab vault rotation, in padded base64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlabRelease {
    /// [`crate::ALAB_SIGNER`]'s new value: the key that signs now.
    pub signer: String,
    /// [`crate::ALAB_RETIRED`]'s new value, oldest first.
    pub retired: Vec<String>,
}

fn alab_release(
    retired: &Retired,
    handoff: &[([u8; 32], [u8; 64])],
    signer: &[u8; 32],
) -> Option<AlabRelease> {
    if !handoff.iter().any(|(key, _)| retired.knows(key)) {
        return None;
    }
    let mut retired: Vec<String> = retired
        .keys
        .iter()
        .map(|key| crate::b64::encode_padded(key))
        .collect();
    for (key, _) in handoff {
        let key = crate::b64::encode_padded(key);
        if !retired.contains(&key) {
            retired.push(key);
        }
    }
    Some(AlabRelease {
        signer: crate::b64::encode_padded(signer),
        retired,
    })
}

/// Publish `doc` as the next version, rotating keys or join credentials when members leave.
pub fn admin_publish(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<Published> {
    publish_as(paths, admin, doc, false, &[], false)
}

/// Publish only over the exact authenticated envelope opened by `admin`.
/// Each receiving store compares that signed parent under its publication lock.
/// Disconnected stores may still accept different children of the same parent;
/// this is not global consensus. A successful call upgrades an unparented vault
/// to parented publication, which requires a reader that understands parents.
///
/// # Errors
/// Missing opened head, expired deadline, lost admin custody, changed source,
/// changed receiving head or the normal sealing/publication failures. A lost
/// response may follow a stored publication and must not be treated as rollback.
pub fn admin_publish_conditional_until(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    deadline: Instant,
) -> crate::Result<Published> {
    publish_as_with_removal(
        paths,
        admin,
        doc,
        false,
        &[],
        false,
        &[],
        None,
        Some(deadline),
        None,
        true,
    )
}

#[cfg(test)]
#[path = "client/conditional_tests.rs"]
mod conditional_tests;

/// [`admin_publish`], also PUTting the signed envelope to explicit candidate server URLs
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F6).
///
/// # Errors
/// Validation or signing fails, or no candidate server accepts the published envelope.
pub fn admin_publish_with_candidates(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    extra_urls: &[String],
) -> crate::Result<Published> {
    publish_as_with(paths, admin, doc, false, &[], false, extra_urls, None, None)
}

#[cfg(test)]
fn admin_publish_found(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    found: Found<'_>,
) -> crate::Result<Published> {
    publish_as_with(paths, admin, doc, false, &[], false, &[], Some(found), None)
}

/// [`admin_publish`] for a revoke that ran [`custody_readable`] before its permanent Commons
/// retirement: no later read of this Mac's subscriber may refuse the publish that completes it.
#[cfg(test)]
fn admin_publish_checked(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<Published> {
    publish_as(paths, admin, doc, false, &[], true)
}

/// Revoke is a durable security operation: prepare its complete log batch and
/// final envelope before retiring anything, then publish those exact heads.
fn admin_publish_revocation(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    recipients: &[Recipient],
) -> crate::Result<(Published, Option<retirement::Retirement>)> {
    let mut retired = None;
    let outcome = publish_as_with_removal(
        paths,
        admin,
        doc,
        false,
        &[],
        true,
        &[],
        None,
        None,
        Some((recipients, &mut retired)),
        false,
    );
    let published = outcome.map_err(|error| {
        if retired.is_some() {
            Error::new(format!("Commons membership is permanently retired; vault removal was not confirmed: {error}. Retry the same revoke command"))
        } else {
            error
        }
    })?;
    Ok((published, retired))
}

/// Publish explicit manual recipient approvals with their machine-list changes.
/// This is an authority decision: ordinary document edits use `admin_publish`
/// and cannot create admission provenance. Existing recipients can be approved
/// again without creating a duplicate machine or trusting their old join path.
pub fn admin_publish_approved(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    recipients: &[String],
) -> crate::Result<Published> {
    publish_admitting(paths, admin, doc, recipients, AdmissionMethod::Manual)
}

/// Publish `doc`, an open vault, with each of `recipients` an admin let in at the terminal as
/// it opened the vault (`aterm secrets password open`): each is recorded as an open admission
/// ([`AdmissionMethod::Open`], never `Manual`: no code or password was checked), so in a vault
/// with Commons control the publication shares the control entry with it
/// (`enrollment::include_approved`) and it enrolls. An open approval never replaces a decision a
/// recipient already holds.
/// # Errors
/// `doc` is not open (an open admission needs an open vault), or the publish failed.
pub fn admin_publish_opened(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    recipients: &[String],
) -> crate::Result<Published> {
    if Approval::of(doc) != Some(Approval::Open) {
        return Err(Error::new(
            "an open admission needs an open vault; nothing was published",
        ));
    }
    publish_admitting(paths, admin, doc, recipients, AdmissionMethod::Open)
}

/// Publish `doc` with an admission decision `method` recorded for each of `recipients`.
fn publish_admitting(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    recipients: &[String],
    method: AdmissionMethod,
) -> crate::Result<Published> {
    let admissions = recipients
        .iter()
        .map(|r| (r.clone(), method))
        .collect::<Vec<_>>();
    publish_as(paths, admin, doc, false, &admissions, false)
}

/// Publish `doc`, which names `host` an admin. In a vault whose Commons control entry `host` is
/// outside, the publish carries this admin's approval of `host` ([`admin_publish_approved`]),
/// which puts it in the entry's audience: an admin enrolls as a Commons member only from there,
/// and removes a machine only once enrolled. Naming an admin again so brings in one an earlier
/// aterm named without it.
/// # Errors
/// `host` is not in the vault, or the publish failed.
pub fn admin_publish_admin(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    host: &str,
) -> crate::Result<Published> {
    let outside = admin_keys::control_descriptor(&admin.published_doc)?.is_some()
        && matches!(doc.commons_audience(), Some(Audience::Hosts(hosts)) if !hosts.iter().any(|h| h == host));
    if !outside {
        return admin_publish(paths, admin, doc);
    }
    let recipient = doc
        .machines()
        .into_iter()
        .find(|machine| machine.name == host)
        .ok_or_else(|| Error::new(format!("no machine {host} in the vault")))?
        .recipient;
    admin_publish_approved(paths, admin, doc, &[recipient])
}

/// Publish `doc` signed by a newly rotated vault key endorsed by the current key.
pub fn admin_rotate(paths: &Paths, admin: &Admin, doc: &Document) -> crate::Result<Published> {
    publish_as(paths, admin, doc, true, &[], false)
}

/// Refuse rewriting `admin`'s locker into this build's format while this Mac's approver may not
/// read it ([`locker_skew`]): before a publish, and before anything a publish completes.
fn custody_readable(paths: &Paths, admin: &Admin) -> crate::Result<()> {
    if admin.custody == admin_keys::MAGIC {
        return Ok(());
    }
    locker_skew::check(paths, admin.custody, admin_keys::MAGIC)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "UnboundRotation",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "BoundDemotion",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "RemovedDescriptor",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "LegacyUnknown",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbFoundationBirth",
        action = "Rotate",
        project = "aterm_secrets::client::birth_tests::birth_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbFoundationBirth",
        action = "Enroll",
        project = "aterm_secrets::client::birth_tests::birth_transition"
    )
)]
fn publish_as(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    rotate: bool,
    approved: &[(String, AdmissionMethod)],
    custody_checked: bool,
) -> crate::Result<Published> {
    publish_as_with(
        paths,
        admin,
        doc,
        rotate,
        approved,
        custody_checked,
        &[],
        None,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal publish implementation threads rotation, approvals, and candidate servers"
)]
fn publish_as_with(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    rotate: bool,
    approved: &[(String, AdmissionMethod)],
    custody_checked: bool,
    extra_urls: &[String],
    found: Option<Found<'_>>,
    deadline: Option<Instant>,
) -> crate::Result<Published> {
    publish_as_with_removal(
        paths,
        admin,
        doc,
        rotate,
        approved,
        custody_checked,
        extra_urls,
        found,
        deadline,
        None,
        false,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the common publication path plus explicit revocation preparation"
)]
fn publish_as_with_removal(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    rotate: bool,
    approved: &[(String, AdmissionMethod)],
    custody_checked: bool,
    extra_urls: &[String],
    found: Option<Found<'_>>,
    deadline: Option<Instant>,
    mut removing: Option<(&[Recipient], &mut Option<retirement::Retirement>)>,
    exact_parent: bool,
) -> crate::Result<Published> {
    publication_current(deadline)?;
    let parent = if exact_parent || admin.parented {
        Some(
            *admin
                .opened_heads
                .first()
                .ok_or_else(|| Error::new("publication has no authenticated parent"))?,
        )
    } else {
        None
    };
    let parents = if exact_parent {
        parent.as_slice()
    } else if admin.parented {
        admin.opened_heads.as_slice()
    } else {
        &[]
    };
    admin_keys::validate_commons_transition(&admin.published_doc, doc)?;
    if !custody_checked {
        custody_readable(paths, admin)?;
    }
    let seq = envelope::next_seq(admin.seq)?;
    let identity = machine_identity(paths)?;
    let (bound, demoted) = publication_rights(admin, doc, &identity)?;
    if removing.is_none() {
        retirement::validate_removals(paths, admin, doc)?;
    }
    let rotate = rotate || demoted;
    let renewed = renew_join_password(&admin.published_doc, doc)?;
    let password_renewed = renewed.is_some();
    let doc = renewed.as_ref().unwrap_or(doc);
    let (recorded, kept_out, held) = code_only_for(&admin.config, &admin.published_doc, doc)?;
    let doc = recorded.as_ref().unwrap_or(doc);
    let shared = enrollment::include_approved(doc, approved)?;
    let doc = shared.as_ref().unwrap_or(doc);
    let noted = note_code_only(&admin.published_doc, admin.seq, doc, seq)?;
    let doc = noted.as_ref().unwrap_or(doc);
    let mut admissions = admin.admissions.retained(doc)?;
    for (recipient, method) in approved {
        admissions.record(
            doc,
            recipient,
            AdmissionEvidence {
                method: *method,
                sequence: seq,
                authored_ms: aterm_time::unix_millis()
                    .ok_or_else(|| Error::new("system clock cannot represent admission time"))?,
                author: identity.recipient(),
            },
        )?;
    }
    // The servers the version may reach, found before the draft: no other publish of this Mac
    // waits on this one's discovery for its turn below.
    let bounded_found = PeerDiscovery::empty();
    let (urls, reach) = match &admin.config.source {
        Source::Url(_) => {
            let urls = ledger_publish::publish_candidate_urls(
                paths,
                &admin.config,
                &admin.published_doc,
                doc,
                extra_urls,
                if deadline.is_some() {
                    Some(&bounded_found)
                } else {
                    found
                },
            );
            let reach = urls.clone();
            (urls, reach)
        }
        Source::Path(path) => (Vec::new(), vec![path.display().to_string()]),
    };
    // The edit is drafted into this Mac's log before the envelope is sealed, and the envelope's
    // stamp names the log's heads with it in: a Mac whose log lacks them reads the envelope's
    // lockers or waits (`log_entries::readable_log`), so a version that reaches a Mac before its
    // records, or whose records never left this Mac, is never read from an older log. The records
    // are kept once a server took the envelope, so a refused ordinary edit leaves nothing
    // behind. An explicit revoke prepares the entire security operation and keeps it atomically
    // after sealing but before publication: failed publication cannot undo its retirement.
    // One-mesh P3: no other publish of this Mac drafts from before this draft reads the log until
    // its records are held and its pending set is written (`log_entries::draft_turn`), so a
    // waiting write either publish drafts reads the other's values in the log or its hold.
    let turn = log_entries::draft_turn(paths, deadline)?;
    // One-mesh W3: no vault pass signs this Mac's waiting writes from before the draft reads the
    // log until the pending set is written (`log_entries::claim_draft`).
    let claim = log_entries::claim_draft(paths, deadline)?;
    let mut attempt = 0;
    let (drafted, everyone, bytes, release, pairs) = loop {
        let prepared = match removing.as_ref() {
            Some((recipients, _)) => Some(removal_plan::prepare(
                paths, admin, doc, &identity, recipients, deadline,
            )?),
            None => None,
        };
        // A removal's prepared batch rotates itself; an edit's `[everyone]` writes may wait for
        // the new key of a removal here whose rotation this Mac lacks (one-mesh M4).
        let (drafted, everyone) = if let Some(prepared) = &prepared {
            (
                prepared.drafted.clone(),
                log_entries::EveryoneWrites::none(),
            )
        } else {
            match admin.log_base {
                log_entries::LogBase::Covers | log_entries::LogBase::Unstamped => {
                    log_entries::draft_document_diff_until(
                        paths,
                        &admin.config,
                        &envelope::signer_public(&admin.seed)?,
                        &admin.handoff,
                        &identity,
                        doc,
                        log_entries::Base {
                            // The version opened, as the log holds it: what an audience this Mac holds no
                            // key of is diffed against. An unstamped version is imported from its text.
                            doc: (admin.log_base == log_entries::LogBase::Covers)
                                .then_some(&admin.published_doc),
                            resolve: Some(&admin.resolve),
                            readmit: None,
                        },
                        // Drafted against what the person saw: the log with this Mac's waiting
                        // writes over it (one-mesh W3).
                        Some(&admin.shown),
                        deadline,
                    )?
                    .map_or(
                        (None, log_entries::EveryoneWrites::none()),
                        |(drafted, everyone)| (Some(drafted), everyone),
                    )
                }
                // A log behind the version edited would draft the older values it holds: the edit goes
                // out unstamped, read from its lockers until a writer whose log holds it imports it.
                log_entries::LogBase::Behind | log_entries::LogBase::NoLog => {
                    (None, log_entries::EveryoneWrites::none())
                }
            }
        };
        let stamp = drafted.as_ref().and_then(log_entries::Drafted::stamp);
        let (bytes, release, pairs) = if rotate {
            let mut seed = [0u8; 32];
            aterm_uds::rand::fill(&mut seed)?;
            let sealed = envelope::signer_public(&seed).and_then(|new| {
                let old = envelope::signer_public(&admin.seed)?;
                let endorsement = envelope::endorse(&admin.seed, &admin.config.name, &new)?;
                let mut handoff = admin.handoff.clone();
                handoff.push((old, endorsement));
                let mut commons_seed = zeroize::Zeroizing::new(admin.commons_seed);
                if !bound {
                    aterm_uds::rand::fill(commons_seed.as_mut())?;
                    let public = envelope::signer_public(&commons_seed)?;
                    if public == new || public == admin.commons_authority()? {
                        return Err(Error::new("new vault authorities must have distinct keys"));
                    }
                }
                let bytes = seal_stamped(
                    &admin.config.name,
                    seq,
                    &seed,
                    &commons_seed,
                    bound,
                    doc,
                    &handoff,
                    &admissions,
                    stamp.as_deref(),
                    parents,
                )?;
                let release = alab_release(&Retired::compiled(), &handoff, &new);
                Ok((bytes, release, handoff.len()))
            });
            zeroize::Zeroize::zeroize(&mut seed);
            sealed?
        } else {
            let bytes = seal_stamped(
                &admin.config.name,
                seq,
                &admin.seed,
                &admin.commons_seed,
                bound,
                doc,
                &admin.handoff,
                &admissions,
                stamp.as_deref(),
                parents,
            )?;
            (bytes, None, admin.handoff.len())
        };
        if let Some(prepared) = prepared {
            match prepared.commit(paths, admin, doc, deadline)? {
                removal_plan::Committed::Changed if attempt < 3 => {
                    attempt += 1;
                    continue;
                }
                removal_plan::Committed::Changed => {
                    return Err(Error::of_kind(
                        crate::ErrorKind::VaultChanged,
                        "the vault kept changing during removal planning; no retirement was committed",
                    ));
                }
                removal_plan::Committed::Done(retired) => {
                    if let Some((_, out)) = removing.as_mut() {
                        **out = retired;
                    }
                }
            }
            retirement::validate_removals(paths, admin, doc)?;
        }
        break (drafted, everyone, bytes, release, pairs);
    };
    #[cfg(test)]
    if let Some(hook) = AFTER_DRAFT.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    // The drafted records are held from here until what became of the envelope that stamps them
    // is known, in a hold of this version's own beside every server the envelope may reach: one
    // stored by a publish that never got to keep them (the process ended, every reply was lost)
    // names records nobody else has, and its writer takes them in from that hold when it reads
    // the version back as a head (`log_entries::adopt_held_draft`), whatever a later publish from
    // the same opened version was told meanwhile.
    publication_current(deadline)?;
    let sealed = envelope::parse(&bytes)?;
    let version = sealed.digest();
    let published_version = version::VersionClaim::new(
        sealed.name.clone(),
        sealed.signer,
        Some(version::EnvelopeVersion {
            seq: sealed.seq,
            digest: version,
        }),
        drafted.as_ref().and_then(|drafted| {
            drafted.network.map(|network| version::LogVersion {
                network,
                heads: drafted.heads.clone(),
            })
        }),
    )?;
    drop(sealed);
    if let Some(drafted) = &drafted {
        log_entries::hold_drafted(paths, drafted, seq, &version, &reach)?;
    }
    // The `[everyone]` writes waiting for the new key are kept from here too, pending on this
    // version beside the servers it may reach: they wait only once a server took it
    // (`log_entries::land_waiting`), here or at a later sync or mirror that reads it from a
    // server, and are dropped where every server refused it, or where each it may have reached
    // took a version that leaves it out for good (`log_entries::land_published`). So are the
    // writes that settle this Mac's waiting ones the edit replaces, where its records sign them.
    log_entries::wait_for_key(
        paths,
        &identity,
        &everyone.pending,
        seq,
        &version,
        &reach,
        &admin.opened_heads,
        deadline,
    )?;
    drop(claim);
    drop(turn);
    // A publish whose deadline ends here sent nothing: its set goes, where the state lock is free
    // now, rather than hold back the names it writes for good (`log_entries::forget_unsent`).
    if let Err(error) = publication_current(deadline) {
        log_entries::forget_unsent(paths, &version);
        return Err(error);
    }
    #[cfg(test)]
    if CUT_BEFORE_STORE.with(std::cell::Cell::take) {
        return Err(Error::new(
            "test: the publish ended before any server took its version",
        ));
    }
    let local = match &admin.config.source {
        Source::Url(_) => {
            let outcomes = ledger_publish::publish_to_servers_for_admin_until(
                paths,
                &admin.config,
                &urls,
                &bytes,
                deadline,
            )?;
            #[cfg(test)]
            if CUT_AFTER_STORE.with(std::cell::Cell::take) {
                return Err(Error::new(
                    "test: the publish ended after the servers answered",
                ));
            }
            #[cfg(test)]
            if let Some(hook) = AFTER_STORE.with(|hook| hook.borrow_mut().take()) {
                hook();
            }
            if !outcomes.iter().any(|(_, r)| r.is_ok()) {
                let mut not_stored = None;
                let mut first_err = None;
                // Only a refusal before any store proves the envelope is on no server; anything
                // else (a reply lost after a server took it) keeps the hold.
                let refused = aterm_mesh_core::no_lost_edit::publish_forgets(
                    outcomes.iter().map(|(_, res)| publish_answer(res)),
                );
                if refused {
                    // This version's hold alone: a server that refused it as not newer may hold
                    // an earlier one of this Mac's whose reply was lost.
                    log_entries::forget_held_draft(paths, seq, &version, deadline);
                    log_entries::forget_waiting(paths, &version);
                }
                for (_, res) in outcomes {
                    if let Err(err) = res {
                        if err.kind() == crate::ErrorKind::NotStored && not_stored.is_none() {
                            not_stored = Some(err);
                        } else if first_err.is_none() {
                            first_err = Some(err);
                        }
                    }
                }
                return Err(not_stored
                    .or(first_err)
                    .unwrap_or_else(|| Error::new("no candidate server accepted the publish")));
            }
            // The version has landed. Every best-effort records post that follows (the store to
            // each server below, and the posts of the pass that ends this publish) ends by one
            // deadline, one exchange from now: however many servers hang, and in however many
            // rounds the records go out, those posts hold the command for one exchange in all. A
            // server whose PUT ran out its deadline unanswered is not posted the store: it
            // answered nothing in an exchange, and a post to it would most likely hold the command
            // to the deadline the same way. It may have stored the envelope all the same (a
            // server that reads the PUT after this Mac gave up stores it, and its reply goes
            // nowhere), stamped with records it was not posted: it takes them from a later
            // records pass that hears it lack this Mac's frontier, and until then a Mac whose log
            // lacks them reads this version from its lockers (`log_entries::readable_log`).
            let posts_by = fan_out::one_exchange();
            #[cfg(test)]
            let posts_by = LANDED_POSTS_BY
                .with(std::cell::Cell::take)
                .unwrap_or(posts_by);
            let kept = match &drafted {
                Some(drafted) => {
                    let answered = answered_puts(&outcomes);
                    log_entries::keep_drafted_until(
                        paths,
                        drafted,
                        (seq, &version),
                        &answered,
                        deadline,
                        Some(posts_by),
                    )?
                }
                None => true,
            };
            // Its records kept, where it drafted any and its hold stood, this version is this
            // Mac's head, or the parent of the head that settled its hold by descent: its hold
            // goes, and the others are settled with the servers that took it, and with its
            // parents where its records were kept (`log_entries::forget_landed_draft`).
            let took: Vec<String> = outcomes
                .iter()
                .filter(|(_, result)| result.is_ok())
                .map(|(url, _)| url.clone())
                .collect();
            log_entries::forget_landed_draft(
                paths,
                seq,
                &version,
                parents,
                &took,
                drafted.is_some(),
                kept,
                deadline,
            );
            // Best effort: a sync or mirror that reads this version lands them where this does not.
            log_entries::land_published(paths, &identity, seq, &version, parents, &took, deadline);
            // A server took this version: it is this Mac's own last version now, the one its
            // mirror carries an approval back from should every server drop it at a heal
            // (`own_head`), unless the one kept still carries a Mac this version lacks.
            let log = envelope::parse(&bytes).ok().and_then(|sealed| {
                log_entries::trusted_seeded_log(paths, &admin.config, &sealed)
                    .ok()
                    .flatten()
            });
            own_head::keep(
                paths,
                &admin.config,
                &identity,
                &bytes,
                doc,
                log.as_ref().map(|(log, _)| log),
            );
            match deadline {
                Some(deadline) => sync_reader_until(paths, deadline)
                    .err()
                    .map(|error| error.to_string()),
                None => sync_without_reconcile(paths, &urls, Some(posts_by))
                    .err()
                    .map(|error| error.to_string()),
            }
        }
        Source::Path(_) => {
            let published = {
                let _lock = configured_admin_until(paths, &admin.config, deadline)?;
                transport::publish_until(&admin.config.source, &bytes, deadline)
            };
            if aterm_mesh_core::no_lost_edit::publish_forgets([publish_answer(&published)]) {
                // This version's hold alone, as above.
                log_entries::forget_held_draft(paths, seq, &version, deadline);
                // Outside the state lock, which forgetting the waiting writes takes.
                log_entries::forget_waiting(paths, &version);
            }
            published?;
            let kept = match &drafted {
                Some(drafted) => log_entries::keep_drafted_until(
                    paths,
                    drafted,
                    (seq, &version),
                    &[],
                    deadline,
                    None,
                )?,
                None => true,
            };
            // Best effort, as above: the vault's file took this version. After its records are
            // kept, as above, so a pass never signs again a write those records sign.
            log_entries::land_published(paths, &identity, seq, &version, parents, &reach, deadline);
            // As above: this version is this Mac's head, or the parent of the head that settled
            // its hold, and the vault's file took it.
            log_entries::forget_landed_draft(
                paths,
                seq,
                &version,
                parents,
                &reach,
                drafted.is_some(),
                kept,
                deadline,
            );
            match deadline {
                Some(deadline) => sync_reader_until(paths, deadline)
                    .err()
                    .map(|error| error.to_string()),
                None => sync(paths, None).err().map(|error| error.to_string()),
            }
        }
    };
    Ok(Published {
        seq,
        version: published_version,
        rotated: rotate,
        alab_release: release,
        first_long_chain: pairs >= 2 && admin.opened_pairs < 2,
        password_renewed,
        kept_out,
        queued: matches!(admin.config.source, Source::Url(_)),
        held,
        local,
        waiting: if everyone.waits {
            everyone.shows
        } else {
            Vec::new()
        },
    })
}

/// A server's answer to a publish as `no_lost_edit`'s kernels read it (one-mesh P3): whether it
/// stored the version, and whether it refused it before storing (`NotStored` or `Refused`). Only
/// a publish every answer of which refuses forgets its hold
/// ([`aterm_mesh_core::no_lost_edit::publish_forgets`]); a lost reply or any other error keeps it.
fn publish_answer<T>(answer: &crate::Result<T>) -> (bool, bool) {
    (
        answer.is_ok(),
        answer.as_ref().is_err_and(|err| {
            matches!(
                err.kind(),
                crate::ErrorKind::NotStored | crate::ErrorKind::Refused
            )
        }),
    )
}

/// The servers of a publish's `outcomes` that answered their PUT, or failed it some way other
/// than running out its deadline ([`crate::ErrorKind::TimedOut`]): the ones a records post may
/// still reach within an exchange. A server that ran the PUT out answered nothing in an exchange
/// (a Mac whose aterm hung took the connection and read nothing), and would most likely run a
/// records post out the same way. That proves nothing about the envelope: a server that reads
/// the PUT after its deadline still stores it, so one left out here may hold this version and
/// lack its records until a later records pass hears it lack this Mac's frontier.
fn answered_puts(outcomes: &[(String, crate::Result<()>)]) -> Vec<String> {
    outcomes
        .iter()
        .filter(|(_, put)| {
            !put.as_ref()
                .is_err_and(|err| err.kind() == crate::ErrorKind::TimedOut)
        })
        .map(|(url, _)| url.clone())
        .collect()
}

#[cfg(test)]
thread_local! {
    /// A test's deadline for the records posts that follow the next publish on this thread once
    /// its version lands, in place of one exchange from then.
    static LANDED_POSTS_BY: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// A test's hook, run once by the next publish on this thread between its draft and the
    /// pending set it keeps (`log_entries::wait_for_key`): a vault pass racing it, as one in
    /// another process may.
    pub(crate) static AFTER_DRAFT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// A test's cut: the next publish on this thread ends once the servers answered, before it
    /// keeps its records, as a process that ends there does.
    static CUT_AFTER_STORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// A test's hook, run once by the next publish to servers on this thread once they answered
    /// and before it keeps its records (`log_entries::keep_drafted_until`): another publish of
    /// this Mac drafting while this one's version is stored and its records are held, not kept,
    /// as one in another process may. The publish then goes on as it would have.
    pub(crate) static AFTER_STORE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// A test's cut: the next publish on this thread ends once it kept its records and waiting
    /// writes held, before any server took its version, as a process that ends there does.
    static CUT_BEFORE_STORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Shared preflight for publication and irreversible membership retirement.
/// Returns whether Commons custody is bound and a vault admin was removed.
fn publication_rights(
    admin: &Admin,
    doc: &Document,
    identity: &Identity,
) -> crate::Result<(bool, bool)> {
    let mine = identity.recipient().to_bech32();
    let me = doc
        .machines()
        .into_iter()
        .find(|machine| machine.recipient == mine)
        .ok_or_else(|| {
            Error::new("this machine would no longer be in the vault; refusing to publish")
        })?;
    if !doc.admins().contains(&me.name) {
        return Err(Error::new(format!(
            "{} would no longer be an admin; refusing to publish (make another admin first)",
            me.name
        )));
    }
    let before = recipients_of(&admin.published_doc.admins(), &admin.published_doc)?;
    let after = recipients_of(&doc.admins(), doc)?;
    let bound = admin.commons_bound
        || admin_keys::has_control(&admin.published_doc)
        || admin_keys::has_control(doc);
    let demoted = before.iter().any(|old| !after.contains(old));
    if demoted && bound {
        return Err(Error::new(
            "cannot demote a vault admin whose Commons authority may already be pinned; \
            a Commons authority transfer is required and is not yet supported",
        ));
    }
    Ok((bound, demoted))
}

fn configured_admin(paths: &Paths, expected: &Config) -> crate::Result<StateLock> {
    configured_admin_until(paths, expected, None)
}

fn publication_current(deadline: Option<Instant>) -> crate::Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(Error::new("vault publication deadline expired"))
    } else {
        Ok(())
    }
}

fn configured_admin_until(
    paths: &Paths,
    expected: &Config,
    deadline: Option<Instant>,
) -> crate::Result<StateLock> {
    let lock = match deadline {
        Some(deadline) => StateLock::take_at_until(paths, &paths.lock_file(), deadline)?,
        None => StateLock::take(paths)?,
    };
    if config(paths)?.as_ref() != Some(expected) {
        return Err(Error::new(
            "the configured vault changed while it was open; open it again before publishing",
        ));
    }
    Ok(lock)
}

fn let_go(old: &Document, doc: &Document) -> Vec<String> {
    let kept: Vec<String> = doc.machines().into_iter().map(|m| m.recipient).collect();
    old.machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .filter(|recipient| !kept.contains(recipient))
        .collect()
}

/// `doc` with a `code-only` line for each key it lets go of `old`'s machines, on any vault, and,
/// in a version that opens the vault, for each request already waiting from a key not in it;
/// whether a key was let go from an open vault (the one a person is told about, [`Published`]);
/// and the requests held. A let-go key's line keeps its request, still queued, from being let in
/// when the vault opens and from being shown as asking by an approver that never read it
/// ([`code_only_standing`]); a password or a code still lets that key in.
fn code_only_for(
    config: &Config,
    old: &Document,
    doc: &Document,
) -> crate::Result<(Option<Document>, bool, Vec<Request>)> {
    let named = doc.code_only();
    let gone: Vec<String> = let_go(old, doc)
        .into_iter()
        .filter(|key| !named.contains(key))
        .collect();
    let waiting: Vec<Request> = if !doc.open() || old.open() {
        Vec::new()
    } else {
        pending_before_opening(config, old, doc)?
            .into_iter()
            .filter(|request| {
                !named.contains(&request.recipient) && !gone.contains(&request.recipient)
            })
            .collect()
    };
    if gone.is_empty() && waiting.is_empty() {
        return Ok((None, false, Vec::new()));
    }
    let mut held = doc.clone();
    for key in gone
        .iter()
        .chain(waiting.iter().map(|request| &request.recipient))
    {
        held.add_code_only(key)
            .map_err(|error| Error::new(error.to_string()))?;
    }
    Ok((Some(held), doc.open() && !gone.is_empty(), waiting))
}

/// `doc`, version `seq` made over `old`, version `old_seq`, with `[join]`'s notes rewritten
/// ([`Document::set_code_only_notes`]), or none when it has no `code-only` line and no note. A
/// key on a `code-only` line in both, and in `old` no member, keeps the version `old` noted for
/// it (one no later than `old_seq`; a key `old` noted none for stays so); any other is named
/// anew in `seq`: it came since, or was let in and let go since. The run note keeps where
/// `old`'s began when it ran through `old_seq`, and begins at `seq` otherwise: `old` was
/// written by an aterm before the notes, or had no `code-only` line. An approver that read the
/// vault at an earlier version tells from them which keys it knew on such lines stayed named
/// since ([`stayed_named`]).
fn note_code_only(
    old: &Document,
    old_seq: u64,
    doc: &Document,
    seq: u64,
) -> crate::Result<Option<Document>> {
    let named = doc.code_only();
    if named.is_empty() && !doc.code_only_notes() {
        return Ok(None);
    }
    let members: Vec<String> = old.machines().into_iter().map(|m| m.recipient).collect();
    let before: Vec<String> = old
        .code_only()
        .into_iter()
        .filter(|key| !members.contains(key))
        .collect();
    let old_in = old.code_only_named_in();
    let named_in: std::collections::BTreeMap<String, u64> = named
        .iter()
        .filter_map(|key| {
            let version = if before.contains(key) {
                *old_in.get(key).filter(|&&version| version <= old_seq)?
            } else {
                seq
            };
            Some((key.clone(), version))
        })
        .collect();
    let from = match old.code_only_noted() {
        Some((from, through)) if through == old_seq => from,
        _ => seq,
    };
    let mut noted = doc.clone();
    noted
        .set_code_only_notes(&named_in, (!named.is_empty()).then_some((from, seq)))
        .map_err(|error| Error::new(error.to_string()))?;
    Ok(Some(noted))
}

fn renew_join_password(old: &Document, doc: &Document) -> crate::Result<Option<Document>> {
    let let_go = !let_go(old, doc).is_empty();
    let admins = recipients_of(&doc.admins(), doc)?;
    let demoted = recipients_of(&old.admins(), old)?
        .iter()
        .any(|admin| !admins.contains(admin));
    let unchanged = match (old.join_password(), doc.join_password()) {
        (Some(before), Some(now)) => before.as_slice() == now.as_slice(),
        _ => false,
    };
    if !((let_go || demoted) && unchanged) {
        return Ok(None);
    }
    let mut renewed = doc.clone();
    renewed
        .set_join_password(Some(join::generate()?.as_slice()))
        .map_err(|error| Error::new(error.to_string()))?;
    Ok(Some(renewed))
}

/// What [`create`] made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The vault's name.
    pub name: String,
    /// The signing key's public half (the pin), padded base64.
    pub signer: String,
    /// Its fingerprint.
    pub fingerprint: String,
    /// Names imported into `[everyone]`.
    pub imported: Vec<String>,
    /// Why this machine's local installation after the publish failed, if it did.
    pub local: Option<String>,
}

/// Create a new vault at `source` with this machine as its initial admin.
///
/// The vault starts open (`[join]`'s open note, [`text::OPEN_NOTE`]): each Mac that asks needs a
/// member's Allow or an admin's approval by code. On the pinned default ALab vault a member on
/// the home ALab LAN also lets in a Mac there that proves itself, whatever `[join]` says.
/// `aterm secrets password new|set` enables admission by join password instead, and
/// `password off` leaves each machine to a member's Allow or an admin's approval by code.
pub fn create(
    paths: &Paths,
    source: &str,
    import: Option<&Path>,
    host: Option<&str>,
) -> crate::Result<Created> {
    create_initial(paths, source, import, host, true, Approval::Open)
}

/// Create a new vault, as [`create`] does, that lets machines in by a generated join password
/// instead of opening: the state `create` then `password new` reaches, in its first version.
/// This seam is absent from production release builds.
#[cfg(any(test, debug_assertions))]
pub fn test_create_with_password(
    paths: &Paths,
    source: &str,
    import: Option<&Path>,
    host: Option<&str>,
) -> crate::Result<Created> {
    create_initial(paths, source, import, host, true, Approval::Password)
}

/// An explicitly requested remote creation still needs background recovery.
pub(crate) fn creation_pending(paths: &Paths) -> crate::Result<bool> {
    creation::pending_remote(paths)
}

pub(crate) fn creation_requires_device(paths: &Paths) -> crate::Result<bool> {
    creation::requires_device(paths)
}

/// Construct a pre-foundation vault for compatibility tests. New installations
/// always use [`create`]; this seam is absent from production release builds.
#[cfg(any(test, debug_assertions))]
pub fn test_create_unbound(
    paths: &Paths,
    source: &str,
    import: Option<&Path>,
    host: Option<&str>,
) -> crate::Result<Created> {
    create_initial(paths, source, import, host, false, Approval::Password)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbFoundationBirth",
        action = "Create",
        project = "aterm_secrets::client::birth_tests::birth_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbFoundationBirth",
        action = "Legacy",
        project = "aterm_secrets::client::birth_tests::birth_transition"
    )
)]
fn create_initial(
    paths: &Paths,
    source: &str,
    import: Option<&Path>,
    host: Option<&str>,
    foundation: bool,
    approval: Approval,
) -> crate::Result<Created> {
    if foundation {
        return creation::create(paths, source, import, host, approval);
    }
    let (source, _) = transport::parse_source(source, &paths.home)?;
    let name = transport::vault_name(&source)?;
    match &source {
        Source::Path(path) => {
            if fsx::kind(path)? != fsx::Kind::Missing {
                return Err(Error::new(format!(
                    "{} already exists; create the vault at a new path",
                    path.display()
                )));
            }
        }
        Source::Url(_) => match transport::fetch(&source, None, Duration::from_secs(30))? {
            Fetched::Missing => {}
            Fetched::Body(bytes) if is_envelope(&bytes) => {
                return Err(Error::new(format!(
                    "a vault named {name} already exists there; join it instead"
                )));
            }
            _ => {
                return Err(Error::new(
                    "something other than a vault server answers at that URL",
                ));
            }
        },
    }
    let host = match host {
        Some(host) if text::valid_machine_name(host) => host.to_string(),
        Some(host) => return Err(Error::new(format!("{host:?} is not a valid machine name"))),
        None => default_host_name(),
    };
    let (bytes, signer, imported) = prepare_initial(paths, &name, import, &host, false, approval)?;
    transport::publish(&source, &bytes)?;
    let config = Config {
        source,
        mirrors: Vec::new(),
        name: name.clone(),
        signer: Some(signer),
        followed: Vec::new(),
        host,
    };
    {
        let _lock = StateLock::take(paths)?;
        let mut state = load_state(paths)?;
        state.seq = 0;
        state.digest = None;
        state.records_digest = None;
        state.membership = Stored::None;
        prepare_materialization(paths, &mut state)?;
        save_config(paths, &config)?;
    }
    let local = sync(paths, None).err().map(|error| error.to_string());
    Ok(Created {
        name,
        signer: crate::b64::encode_padded(&signer),
        fingerprint: envelope::fingerprint(&signer),
        imported,
        local,
    })
}

fn prepare_initial(
    paths: &Paths,
    name: &str,
    import: Option<&Path>,
    host: &str,
    foundation: bool,
    approval: Approval,
) -> crate::Result<(Vec<u8>, [u8; 32], Vec<String>)> {
    let identity = machine_identity(paths)?;
    let mut doc = Document::new_vault(host, &identity.recipient().to_bech32());
    match approval {
        Approval::Open => doc.set_open(true),
        Approval::Password => doc.set_join_password(Some(join::generate()?.as_slice())),
    }
    .map_err(|e| Error::new(e.to_string()))?;
    let mut imported = Vec::new();
    let empty = doc.clone();
    if let Some(dir) = import {
        let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                (!name.starts_with('.') && text::valid_entry_name(&name))
                    .then(|| (name, entry.path()))
            })
            .collect();
        names.sort();
        for (name, path) in names {
            if fsx::kind(&path)? != fsx::Kind::File {
                continue;
            }
            let Some(value) = fsx::read_owned(&path, text::MAX_VALUE)? else {
                continue;
            };
            let value = SecretBytes::new(value);
            doc.set_entry(&Audience::Everyone, &name, value.as_slice())
                .map_err(|e| Error::new(format!("{name}: {e}")))?;
            imported.push(name);
        }
        // A name no vault text writes, or two spellings of one file, refuse the import whole.
        if let Some(why) = doc.added_refusal(&empty) {
            return Err(Error::new(format!("{}: {why}", dir.display())));
        }
    }
    let mut seed = zeroize::Zeroizing::new([0u8; 32]);
    aterm_uds::rand::fill(seed.as_mut())?;
    let signer = envelope::signer_public(&seed)?;
    let mut commons_seed = zeroize::Zeroizing::new([0u8; 32]);
    aterm_uds::rand::fill(commons_seed.as_mut())?;
    if envelope::signer_public(&commons_seed)? == signer {
        return Err(Error::new("new vault authorities must have distinct keys"));
    }
    if foundation {
        let anchor =
            crate::commons::node::Node::foundation(envelope::signer_public(&commons_seed)?)?;
        let descriptor = crate::commons::bootstrap::Descriptor::from_genesis(anchor)?;
        doc.set_entry(
            &Audience::Hosts(vec![host.to_string()]),
            crate::commons::ENTRY,
            &descriptor.bytes(),
        )
        .map_err(|error| Error::new(error.to_string()))?;
        // Creation explicitly authorizes this first owner. Do not reuse this
        // path for admission to an existing vault: its eligibility must come
        // from the authenticated published document captured by Admin.
        let descriptor = creation::enroll_founder(paths, &doc, host, &commons_seed, descriptor)?;
        doc.set_entry(
            &Audience::Hosts(vec![host.to_string()]),
            crate::commons::ENTRY,
            &descriptor.bytes(),
        )
        .map_err(|error| Error::new(error.to_string()))?;
    }
    let bytes = if foundation {
        let mut admissions = admission::Ledger::default();
        admissions.record(
            &doc,
            &identity.recipient().to_bech32(),
            AdmissionEvidence {
                method: AdmissionMethod::Founder,
                sequence: 1,
                authored_ms: aterm_time::unix_millis()
                    .ok_or_else(|| Error::new("system clock cannot represent admission time"))?,
                author: identity.recipient(),
            },
        )?;
        seal_with_admissions(name, 1, &seed, &commons_seed, true, &doc, &[], &admissions)?
    } else {
        seal(name, 1, &seed, &commons_seed, false, &doc, &[])?
    };
    Ok((bytes, signer, imported))
}

/// A pending join request.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    /// The machine's key.
    pub recipient: String,
    /// The name it asked for.
    pub host: String,
    /// Its code.
    pub code: String,
    /// When it asked (unix), when the server says.
    pub at: Option<u64>,
    /// A member's signed Allow of it (`crate::members::Allowed`), if it carried one.
    pub allowed: Option<String>,
    /// Its proof of the join password, if it sent one.
    pub proof: Option<String>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("recipient", &self.recipient)
            .field("host", &self.host)
            .field("code", &self.code)
            .field("at", &self.at)
            .field("member_allow_present", &self.allowed.is_some())
            .field("password_proof_present", &self.proof.is_some())
            .finish()
    }
}

/// The names the queue lists `key` under, sorted, each once.
#[must_use]
pub fn names_asked<'a>(pending: &'a [Request], key: &str) -> Vec<&'a str> {
    let mut hosts: Vec<&str> = pending
        .iter()
        .filter(|r| r.recipient == key)
        .map(|r| r.host.as_str())
        .collect();
    hosts.sort_unstable();
    hosts.dedup();
    hosts
}

/// Whether `request` may be approved by its code: the queue lists its key under one name. Anyone
/// on the LAN can queue a key under a name of its choosing, so a key listed under two is approved
/// only under the name its machine asked for, which an admin names (design §7).
/// # Errors
/// The queue lists the key under more than one name; the error names them and that command.
pub fn one_name_asked(pending: &[Request], request: &Request) -> crate::Result<()> {
    let hosts = names_asked(pending, &request.recipient);
    if hosts.len() > 1 {
        return Err(Error::new(format!(
            "code {} asks to join under {} names ({}), and anyone on the LAN can queue a key under a name of its choosing: approve it under the name its machine asked for with aterm secrets approve {} --name <host>",
            request.code,
            hosts.len(),
            hosts.join(", "),
            request.recipient
        )));
    }
    Ok(())
}

/// Requests still requiring the administrator document's publication step.
/// A log-admitted member may still need that step, so this work queue is distinct
/// from the pending-admission view returned by [`inspect_requests`].
pub fn requests(paths: &Paths, doc: Option<&Document>) -> crate::Result<Vec<Request>> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    requests_for(paths, &config, doc)
}

/// A fresh manual approval identifies a code or a recipient, independently of which
/// queue a previous listing selected. Automatic admission keeps each queue's origin.
pub(crate) enum ApprovalTarget<'a> {
    Code(&'a str),
    Recipient(&'a str),
}

/// Request observations and whether they cover every authorized queue. Only a
/// complete observation may retire previously answered requests it no longer lists.
pub(crate) struct ApprovalRequests {
    pub(crate) requests: Vec<Request>,
    pub(crate) complete: bool,
}

/// Queue policy from the authenticated baseline, never a partial materialized read.
fn approval_queue_policy(config: &Config, baseline: &Document) -> crate::Result<Vec<String>> {
    let mut policy = Vec::new();
    let entries = baseline.entries();
    let mut policies = entries
        .iter()
        .filter(|entry| entry.name == crate::SWARM_POLICY_ENTRY);
    if config.signer.is_some()
        && let Some(entry) = policies.next()
    {
        if policies.next().is_some() || entry.if_absent || entry.anchor.is_some() {
            return Err(Error::new(
                "join queue policy must be unique and unconditional",
            ));
        }
        let text = std::str::from_utf8(entry.value.as_slice())
            .map_err(|_| Error::new("join queue policy is not UTF-8"))?;
        let parsed = crate::swarm_roster::SwarmPolicy::from_toml(text)?;
        ledger_publish::push_policy_server_urls(&parsed, &config.name, |url| {
            policy.push(url.to_owned())
        });
    }
    Ok(policy)
}

fn approval_requests(config: &Config, baseline: &Document) -> crate::Result<(Vec<Request>, bool)> {
    if !matches!(config.source, Source::Url(_)) {
        return Ok((Vec::new(), false));
    }
    // The same authenticated baseline authorizes both the queue set and the
    // eventual publication. A missing or busy materialized policy cannot erase
    // a mirror from a supposedly complete observation.
    let policy = approval_queue_policy(config, baseline)?;
    let observed = join_queues::lookup(
        config,
        &policy,
        Instant::now() + servers_within(PROBE_DEADLINE.saturating_mul(2)),
    )?;
    let mut pending = Vec::new();
    for queue in observed.queues {
        for mut request in requests_in(queue.lines, &[]) {
            // Keep each replica's timestamp and proofs for answered bookkeeping.
            // Selection below counts identities, never these transport copies.
            request.recipient = Recipient::parse(&request.recipient)
                .map_err(|error| Error::new(error.to_string()))?
                .to_bech32();
            pending.push(request);
        }
    }
    Ok((pending, observed.incomplete))
}

/// Find every explicit target in one fresh observation of the authorized queues.
/// A missing target is not a definite absence when some queues did not answer.
pub(crate) fn requests_for_approval(
    admin: &Admin,
    targets: &[ApprovalTarget<'_>],
) -> crate::Result<ApprovalRequests> {
    if targets.is_empty() {
        return Ok(ApprovalRequests {
            requests: Vec::new(),
            complete: false,
        });
    }
    let (pending, incomplete) = approval_requests(&admin.config, &admin.published_doc)?;
    for target in targets {
        let matches: Vec<_> = pending
            .iter()
            .filter(|request| match target {
                ApprovalTarget::Code(code) => request.code.eq_ignore_ascii_case(code),
                ApprovalTarget::Recipient(recipient) => request.recipient == *recipient,
            })
            .collect();
        let Some(request) = matches.first() else {
            return Err(if incomplete {
                Error::of_kind(
                    crate::ErrorKind::JoinQueueIncomplete,
                    "join queue lookup is incomplete; the requested identity may be on an unavailable queue",
                )
            } else {
                Error::new("no machine waiting to join matches the requested identity")
            });
        };
        if matches
            .iter()
            .any(|other| other.recipient != request.recipient)
        {
            return Err(Error::new("admission code is ambiguous"));
        }
        one_name_asked(&pending, request)?;
    }
    Ok(ApprovalRequests {
        requests: pending,
        complete: !incomplete,
    })
}

/// Every request in one complete observation of the authenticated administrator's
/// authorized queues. Bulk approval cannot promise every pending machine when a
/// queue was unavailable. Keep admitted identities here for answered bookkeeping;
/// the caller separately selects which pending identities to offer.
pub(crate) fn all_requests_for_approval(admin: &Admin) -> crate::Result<ApprovalRequests> {
    let (requests, incomplete) = approval_requests(&admin.config, &admin.published_doc)?;
    if incomplete {
        return Err(Error::of_kind(
            crate::ErrorKind::JoinQueueIncomplete,
            "cannot approve all while a join queue is unavailable; approve individual codes instead",
        ));
    }
    Ok(ApprovalRequests {
        requests,
        complete: true,
    })
}

/// Opening holds every already-waiting identity. A partial observation cannot
/// establish that hold, so unlike a found manual approval it refuses to publish.
pub(crate) fn requests_before_opening(
    admin: &Admin,
    doc: &Document,
) -> crate::Result<Vec<Request>> {
    pending_before_opening(&admin.config, &admin.published_doc, doc)
}

fn pending_before_opening(
    config: &Config,
    baseline: &Document,
    doc: &Document,
) -> crate::Result<Vec<Request>> {
    let (mut pending, incomplete) = approval_requests(config, baseline)?;
    if incomplete {
        return Err(Error::of_kind(
            crate::ErrorKind::JoinQueueIncomplete,
            "cannot open the vault while a join queue is unavailable",
        ));
    }
    let known: BTreeSet<_> = doc.machines().into_iter().map(|m| m.recipient).collect();
    pending.retain(|request| !known.contains(&request.recipient));
    Ok(pending)
}

/// One observed join queue, after authenticated membership filtering.
/// Its server is the queue's reader endpoint, not the address of a joining machine.
#[derive(Debug, Clone, Default)]
pub struct RequestInspection {
    /// Requests not excluded by the available authenticated membership.
    pub requests: Vec<Request>,
    /// Whether authenticated membership was available for filtering.
    pub known_members_filtered: bool,
    /// Selected queue's scheme, host and path, with query and fragment omitted.
    pub queue_source: Option<String>,
    /// Address recorded by the same connection that returned this queue.
    pub queue_server: Option<std::net::IpAddr>,
    /// Server's join-queue version, not a vault or membership sequence.
    pub queue_version: Option<u64>,
}

impl RequestInspection {
    fn from_queue(requests: Vec<Request>, known_members_filtered: bool, heard: Heard) -> Self {
        Self {
            requests,
            known_members_filtered,
            queue_source: (!heard.url.is_empty()).then(|| transport::describe(&heard.url)),
            queue_server: heard.from,
            queue_version: heard.version,
        }
    }
}

/// Pending requests and the origin of the queue that actually answered.
/// # Errors
/// Configuration, authenticated membership, or the selected queue cannot be read.
pub fn inspect_requests(paths: &Paths, doc: Option<&Document>) -> crate::Result<RequestInspection> {
    inspect_requests_with_deadline(paths, doc, None)
}

/// Pending join identities observed within one caller-owned deadline. Available
/// local admin custody excludes approved recipients without fetching a vault.
/// Unknown membership remains explicitly unfiltered, just as in [`inspect_requests`].
/// # Errors
/// Configuration, local membership or the queue is unreadable, or the deadline expires.
pub fn inspect_requests_until(
    paths: &Paths,
    deadline: Instant,
) -> crate::Result<RequestInspection> {
    let admin = admin_cached_until(paths, deadline).ok();
    inspect_requests_with_deadline(
        paths,
        admin.as_ref().map(|admin| &admin.doc),
        Some(deadline),
    )
}

fn inspect_requests_with_deadline(
    paths: &Paths,
    doc: Option<&Document>,
    deadline: Option<Instant>,
) -> crate::Result<RequestInspection> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    let known = request_members(paths, &config, doc)?;
    let mut heard = Heard::default();
    let (lines, _) = match deadline {
        Some(deadline) => join_queues::hear_until(paths, &config, &mut heard, deadline)?,
        None => join_queues::hear(paths, &config, &mut heard, Duration::ZERO)?,
    };
    let mut pending = requests_in(lines, &[]);
    if let Some(known) = &known {
        pending.retain(|request| !known.contains(&request.recipient));
    }
    Ok(RequestInspection::from_queue(
        pending,
        known.is_some(),
        heard,
    ))
}

fn request_members(
    paths: &Paths,
    config: &Config,
    doc: Option<&Document>,
) -> crate::Result<Option<Vec<String>>> {
    // An administrator's grants can precede enrollment into the membership log.
    // Either fact means the request no longer needs an approval from this reader.
    let mut known: Option<Vec<String>> =
        doc.map(|doc| doc.machines().into_iter().map(|m| m.recipient).collect());
    if let Some(log) = log_entries::trusted_local_log(paths, config)? {
        let known = known.get_or_insert_with(Vec::new);
        known.extend(
            log.membership()
                .members()
                .values()
                .map(|member| member.binding.recipient().to_bech32()),
        );
        known.sort_unstable();
        known.dedup();
    }
    Ok(known)
}

/// The join requests at the configured queue or a verified policy mirror, and the actual queue
/// URL (empty for a path).
fn requests_from(paths: &Paths, config: &Config) -> crate::Result<(Vec<Request>, String)> {
    let mut heard = Heard::default();
    let (lines, _) = join_queues::hear(paths, config, &mut heard, Duration::ZERO)?;
    Ok((requests_in(lines, &[]), heard.url))
}

fn requests_for(
    paths: &Paths,
    config: &Config,
    doc: Option<&Document>,
) -> crate::Result<Vec<Request>> {
    let known: Vec<String> = doc
        .map(|doc| doc.machines().into_iter().map(|m| m.recipient).collect())
        .unwrap_or_default();
    let (mut pending, _) = requests_from(paths, config)?;
    pending.retain(|request| !known.contains(&request.recipient));
    Ok(pending)
}

/// The join requests the queue's `lines` hold, but those of keys in `known`.
fn requests_in(lines: Vec<String>, known: &[String]) -> Vec<Request> {
    let mut out = Vec::new();
    for line in lines {
        let Ok(request) = transport::parse_join_line(&line) else {
            continue;
        };
        let Ok(parsed) = Recipient::parse(&request.recipient) else {
            continue;
        };
        if join_code(&parsed) != request.code || known.contains(&request.recipient) {
            continue;
        }
        out.push(Request {
            recipient: request.recipient,
            host: request.host,
            code: request.code,
            at: request.time,
            allowed: request.allowed,
            proof: request.proof,
        });
    }
    out
}

/// Return an unused valid machine name in `doc` based on `wanted`, avoiding join-code shapes.
pub fn free_name(doc: &Document, wanted: &str) -> String {
    let taken: Vec<String> = doc.machines().into_iter().map(|m| m.name).collect();
    let takeable = |name: &str| text::valid_machine_name(name) && !transport::valid_code(name);
    let base = if takeable(wanted) { wanted } else { "machine" };
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    (2u64..)
        .map(|n| {
            let suffix = format!("-{n}");
            let room = 63usize.saturating_sub(suffix.len());
            let stem = match base.get(..room) {
                Some(cut) if base.len() > room => cut.trim_end_matches('-'),
                _ => base,
            };
            format!("{stem}{suffix}")
        })
        .find(|name| !taken.contains(name) && takeable(name))
        .unwrap_or_else(|| "machine".to_string())
}

/// Resolve `word` (a join code, swarm `m<N>` label/alias, or machine name) to a machine name in `doc`.
pub fn machine_named(doc: &Document, word: &str) -> crate::Result<String> {
    let code = word.to_ascii_lowercase();
    if !transport::valid_code(&code) {
        let machines = doc.machines();
        if machines.iter().any(|m| m.name == word) {
            return Ok(word.to_string());
        }
        if let Ok(Some(roster)) = crate::swarm_roster::SwarmRoster::from_document(doc)
            && let Some(member) = roster.member_by_label_or_alias(word)
            && machines.iter().any(|m| m.name == member.vault_name)
        {
            return Ok(member.vault_name.clone());
        }
        if let Some((_, canonical)) = crate::swarm_roster::leading_m_label(word) {
            let mut matched = machines.iter().filter(|m| {
                crate::swarm_roster::leading_m_label(&m.name)
                    .is_some_and(|(_, label)| label == canonical)
            });
            if let (Some(only), None) = (matched.next(), matched.next()) {
                return Ok(only.name.clone());
            }
        }
        return Ok(word.to_string());
    }
    let machines = doc.machines();
    let by_code = machines.iter().find(|machine| {
        Recipient::parse(&machine.recipient).is_ok_and(|key| join_code(&key) == code)
    });
    let by_name = machines.iter().find(|machine| machine.name == code);
    match (by_code, by_name) {
        (Some(keyed), Some(named)) if keyed.name != named.name => Err(Error::new(format!(
            "{code} is the code of machine {} and the name of another machine (key {}): name the one you mean by its name, or rename the other in aterm secrets edit",
            keyed.name, named.recipient
        ))),
        (Some(machine), _) => Ok(machine.name.clone()),
        (None, Some(named)) => Err(Error::new(format!(
            "no machine of the vault has code {code}; a machine is named {code} (key {}{}), and its own code names it",
            named.recipient,
            Recipient::parse(&named.recipient)
                .map(|key| format!(", code {}", join_code(&key)))
                .unwrap_or_default()
        ))),
        (None, None) => Err(Error::new(format!(
            "no machine of the vault has code {code}"
        ))),
    }
}

/// The machines of `current` whose key `base` does not hold: those admitted since `base`.
pub fn admitted_since(base: &Document, current: &Document) -> Vec<text::Machine> {
    let known: Vec<String> = base.machines().into_iter().map(|m| m.recipient).collect();
    current
        .machines()
        .into_iter()
        .filter(|machine| !known.contains(&machine.recipient))
        .collect()
}

/// Merge membership, admin, and `[join]` changes made between `base` and `current` into `edited`.
pub fn keep_admitted(
    base: &Document,
    current: &Document,
    edited: &mut Document,
) -> crate::Result<Vec<String>> {
    let refused = |error: text::TextError| Error::new(error.to_string());
    let known: Vec<String> = base.machines().into_iter().map(|m| m.recipient).collect();
    let now = current.machines();
    for gone in base.machines() {
        if now
            .iter()
            .any(|machine| machine.recipient == gone.recipient)
        {
            continue;
        }
        let Some(held) = edited
            .machines()
            .into_iter()
            .find(|machine| machine.recipient == gone.recipient)
        else {
            continue;
        };
        if let Some(other) = now
            .iter()
            .find(|machine| machine.name == gone.name && !known.contains(&machine.recipient))
        {
            return Err(Error::new(format!(
                "{} has another key since your edit began ({}): put it on {}'s line to give it what your text gives {}, or delete the machine, then save again",
                gone.name, other.recipient, held.name, held.name
            )));
        }
        edited.remove_machine(&held.name, true).map_err(refused)?;
    }
    let mut kept = Vec::new();
    for machine in admitted_since(base, current) {
        if edited
            .machines()
            .iter()
            .any(|known| known.recipient == machine.recipient)
        {
            continue;
        }
        let name = free_name(edited, &machine.name);
        edited
            .add_machine(&name, &machine.recipient)
            .map_err(refused)?;
        kept.push(name);
    }
    let admins = |doc: &Document| -> Vec<String> {
        let names = doc.admins();
        doc.machines()
            .into_iter()
            .filter(|machine| names.contains(&machine.name))
            .map(|machine| machine.recipient)
            .collect()
    };
    let (were, are) = (admins(base), admins(current));
    for machine in edited.machines() {
        if are.contains(&machine.recipient) && !were.contains(&machine.recipient) {
            edited.add_admin(&machine.name).map_err(refused)?;
        }
    }
    for machine in edited.machines() {
        if were.contains(&machine.recipient) && !are.contains(&machine.recipient) {
            edited.remove_admin(&machine.name).map_err(refused)?;
        }
    }
    if !same_join(base, current) && same_join(base, edited) {
        let password = current.join_password();
        edited
            .set_join_password(password.as_ref().map(|password| password.as_slice()))
            .map_err(refused)?;
        edited.set_open(current.open()).map_err(refused)?;
    }
    let before = base.code_only();
    let members: Vec<String> = edited.machines().into_iter().map(|m| m.recipient).collect();
    for key in current.code_only() {
        if !before.contains(&key) && !members.contains(&key) {
            edited.add_code_only(&key).map_err(refused)?;
        }
    }
    Ok(kept)
}

fn same_join(a: &Document, b: &Document) -> bool {
    a.open() == b.open()
        && match (a.join_password(), b.join_password()) {
            (Some(x), Some(y)) => x.as_slice() == y.as_slice(),
            (x, y) => x.is_none() && y.is_none(),
        }
}

struct HeldLock(std::fs::File);

impl HeldLock {
    fn try_take(path: &Path) -> crate::Result<Option<HeldLock>> {
        let file: std::fs::File = fsx::open_lock(path)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of HeldLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(Some(HeldLock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(Error::new(format!("{}: {e}", path.display())))
            }
        }
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// An exclusively created `0700` edit directory, locked while it lives.
pub struct Scratch {
    dir: PathBuf,
    _lock: HeldLock,
}

impl Scratch {
    /// Create a new edit directory. Existing directories are never adopted or swept.
    pub fn new(paths: &Paths) -> crate::Result<Scratch> {
        fsx::ensure_private_dir(&paths.dir)?;
        let dir = aterm_tempfile::Builder::new()
            .prefix("edit-")
            .tempdir_in(&paths.dir)
            .map_err(|error| Error::new(format!("{}: {error}", paths.dir.display())))?
            .keep();
        let lock = HeldLock::try_take(&dir.join(".lock"))?
            .ok_or_else(|| Error::new("a fresh edit directory is already locked"))?;
        Ok(Scratch { dir, _lock: lock })
    }

    /// The directory. Its contents survive this guard unless explicitly discarded.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Discard `file` if it still holds `expected`, then remove the lock and empty directory.
    pub fn discard(self, file: &Path, expected: &[u8]) -> crate::Result<()> {
        if file.parent() != Some(self.dir.as_path())
            || file.file_name().is_none_or(|name| name == ".lock")
        {
            return Err(Error::new(
                "an edit draft must be one file directly in its scratch directory",
            ));
        }
        if let Some(bytes) = fsx::read_private(file, text::MAX_TEXT)?
            && !aterm_digest::ct_eq(bytes.as_slice(), expected)
        {
            return Err(Error::new(format!(
                "{}: the draft changed before cleanup; kept",
                file.display()
            )));
        }
        fsx::wipe_and_remove(file)?;
        let lock = self.dir.join(".lock");
        match std::fs::remove_file(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::new(format!("{}: {error}", lock.display()))),
        }
        match std::fs::remove_dir(&self.dir) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(Error::new(format!("{}: {error}", self.dir.display()))),
        }
    }
}

#[cfg(test)]
#[path = "client/probe_selection_tests.rs"]
mod probe_selection_tests;

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "client_commons_tests.rs"]
mod commons_tests;

#[cfg(test)]
#[path = "client_edit_scratch_tests.rs"]
mod edit_scratch_tests;

#[cfg(test)]
#[path = "client_capability_tests.rs"]
mod capability_tests;
