// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet admission as this Mac's membership log says it (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §4.1, F17): which node keys the log admits and which it holds out, for the fleet listener's
//! handshake, a join it answers, the signed book entries it merges and a drive it admits.
//!
//! The log is the record store F13 fills (the secrets directory's `records`,
//! `aterm_secrets::client::load_records`); this reads it back, evaluates it, and keeps the verdict
//! until the file changes. A Mac whose store holds no genesis yet has no verdict, and admission
//! falls back to its book's keyring.

use aterm_net::fleet::NodeKey;
use aterm_net::fleet_member::{MemberKeys, Standing};
use aterm_secrets::membership_log::{Log, Membership, NodeStanding};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Each node key `membership` binds, as admission reads it: a member's current key is a
/// member's; the key of a Mac the log removed, one its member replaced and one two members claim
/// are out. A key no record binds is not listed.
#[must_use]
pub fn member_keys(membership: &Membership) -> MemberKeys {
    membership
        .nodes()
        .iter()
        .filter_map(|(key, standing)| {
            let standing = match standing {
                NodeStanding::Member(_) => Standing::Member,
                NodeStanding::Removed(_) => {
                    Standing::Out("the membership log removed the Mac that holds it".to_owned())
                }
                NodeStanding::Retired(_) => {
                    Standing::Out("its member bound another node key since".to_owned())
                }
                NodeStanding::Contested => {
                    Standing::Out("two members of the membership log claim it".to_owned())
                }
                NodeStanding::Unknown => return None,
            };
            Some((NodeKey::from_bytes(*key), standing))
        })
        .collect()
}

/// Each node key `log` binds, as admission reads it: a member's current key is a member's unless
/// the newest `[everyone]` epoch leaves that member out; the key of a Mac the log removed, one
/// its member replaced, one two members claim, and one left out of `[everyone]` are out.
#[must_use]
pub fn log_member_keys(log: &Log) -> MemberKeys {
    log_member_keys_with_roster(log, None)
}

#[must_use]
pub fn log_member_keys_with_roster(
    log: &Log,
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
) -> MemberKeys {
    let membership = log.membership();
    membership
        .nodes()
        .iter()
        .filter_map(|(key, standing)| {
            let standing = match standing {
                NodeStanding::Member(actor)
                    if membership
                        .get(actor)
                        .is_some_and(|m| log.recipient_is_removed(&m.binding.recipient())) =>
                {
                    Standing::Out("the membership log removed the Mac that holds it".to_owned())
                }
                NodeStanding::Member(actor)
                    if log.audience_left_out(
                        &aterm_secrets::membership_log::EpochAudience::Everyone,
                        actor,
                    ) =>
                {
                    Standing::Out(
                        "the vault's newest key leaves the Mac that holds it out".to_owned(),
                    )
                }
                NodeStanding::Member(actor)
                    if membership.get(actor).is_some_and(|m| {
                        roster.is_some_and(|r| {
                            r.is_label_denied(&m.label)
                                || r.is_label_denied(&m.name)
                                || r.is_node_denied(&aterm_codec::hex::encode_lower(key))
                        })
                    }) =>
                {
                    Standing::Out("the swarm roster denies the Mac that holds it".to_owned())
                }
                NodeStanding::Member(_) => Standing::Member,
                NodeStanding::Removed(_) => {
                    Standing::Out("the membership log removed the Mac that holds it".to_owned())
                }
                NodeStanding::Retired(_) => {
                    Standing::Out("its member bound another node key since".to_owned())
                }
                NodeStanding::Contested => {
                    Standing::Out("two members of the membership log claim it".to_owned())
                }
                NodeStanding::Unknown => return None,
            };
            Some((NodeKey::from_bytes(*key), standing))
        })
        .collect()
}

/// How a [`LocalLog`] reads its store: the member keys it evaluates, `None` before a genesis.
type Reader = Box<dyn Fn() -> aterm_secrets::Result<Option<MemberKeys>> + Send + Sync>;

/// What the store file's metadata says of it. A write replaces the whole file
/// (`aterm_secrets::fsx::write_atomic`), so each one moves it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

fn stamp(file: &Path) -> Option<Stamp> {
    let meta = std::fs::symlink_metadata(file).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        inode: meta.ino(),
    })
}

/// The membership store one process reads, with the verdict it last evaluated and the stamp it
/// read it at.
pub struct LocalLog {
    file: PathBuf,
    aux_file: Option<PathBuf>,
    read: Reader,
    held: Mutex<Option<Held>>,
}

struct Held {
    stamp: Stamp,
    aux_stamp: Option<Stamp>,
    keys: Option<Arc<MemberKeys>>,
}

impl LocalLog {
    /// The store `paths` keeps (`Paths::records_file`, read by
    /// `aterm_secrets::client::load_records`).
    #[must_use]
    pub fn of(paths: &aterm_secrets::client::Paths) -> Self {
        let from = paths.clone();
        Self::reading(
            paths.records_file(),
            Some(paths.dir.join("applied.vault")),
            Box::new(move || {
                let store = aterm_secrets::client::load_records(&from)?;
                Ok(store.log().map(|log| {
                    let roster = aterm_secrets::client::local_swarm_roster_for_log(&from, log);
                    log_member_keys_with_roster(log, roster.as_ref())
                }))
            }),
        )
    }

    /// The store file `file`, founded on the genesis `network` names: a test's lab of its own.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn founded_at(file: PathBuf, network: aterm_secrets::membership_log::RecordId) -> Self {
        let from = file.clone();
        Self::reading(
            file,
            None,
            Box::new(move || {
                let bytes = std::fs::read(&from)
                    .map_err(|e| aterm_secrets::Error::new(format!("the record store: {e}")))?;
                let mut store = aterm_secrets::membership_log::Store::with_compiled(Some(network));
                store.ingest(&bytes)?;
                Ok(store.log().map(log_member_keys))
            }),
        )
    }

    fn reading(file: PathBuf, aux_file: Option<PathBuf>, read: Reader) -> Self {
        Self {
            file,
            aux_file,
            read,
            held: Mutex::new(None),
        }
    }

    /// The store's file.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The log's admission now: read again only when the store's file changed since the last read.
    /// `None` while the store is missing or holds no genesis. A store that cannot be read keeps
    /// the verdict last read from it (none before a first good read), so a moment's fault never
    /// lets in a key the log held out.
    pub fn keys(&self) -> Option<Arc<MemberKeys>> {
        // Taken before the read: a write between the two moves the next stamp.
        let file_stamp = stamp(&self.file)?;
        let aux_stamp = self.aux_file.as_deref().and_then(stamp);
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(held) = held.as_ref()
            && held.stamp == file_stamp
            && held.aux_stamp == aux_stamp
        {
            return held.keys.clone();
        }
        let Ok(keys) = (self.read)() else {
            return held.as_ref().and_then(|held| held.keys.clone());
        };
        let keys = keys.map(Arc::new);
        *held = Some(Held {
            stamp: file_stamp,
            aux_stamp,
            keys: keys.clone(),
        });
        keys
    }
}

/// Ensure this Mac's fleet `node` identity is bound in `paths`'s membership log via a signed
/// [`aterm_secrets::membership_log::Body::Node`] record when this Mac is an active bound member
/// whose `node.key()` is not yet bound in `log_keys` (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §3.3, F17).
pub fn ensure_local_node_key_in_log(
    paths: &aterm_secrets::client::Paths,
    node: &aterm_net::fleet::NodeIdentity,
    log_keys: Option<&MemberKeys>,
) -> bool {
    if log_keys.is_some_and(|keys| keys.admits(&node.key())) {
        return false;
    }
    let key_bytes = *node.key().bytes();
    aterm_secrets::client::publish_local_node_key(paths, key_bytes, |network, actor| {
        let mut body = Vec::with_capacity(network.len() + actor.len());
        body.extend_from_slice(network);
        body.extend_from_slice(actor);
        node.sign_record("membership node", &body)
    })
    .ok()
    .flatten()
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_net::fleet::NodeIdentity;

    #[test]
    fn ensure_local_node_key_in_log_binds_founder_and_retires_rotated_key() {
        let root = aterm_tempfile::tempdir().unwrap();
        let server_dir = root.path().join("server");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: server_dir,
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/test", running.addr);
        let home = root.path().join("m22-home");
        let paths = aterm_secrets::client::Paths::for_home(&home);
        let created = aterm_secrets::client::create(&paths, &url, None, Some("m22")).unwrap();
        aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).unwrap();
        aterm_secrets::client::seed_vault_genesis(&paths, &root.path().join("test.genesis"))
            .unwrap();
        aterm_secrets::client::admin_mirror_log(&paths, &[]).unwrap();

        let local_log = LocalLog::of(&paths);
        let node_1 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x11; 32]).unwrap();
        let node_2 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x22; 32]).unwrap();

        // 1. Founder starts in Genesis v2 with `node_key: None`.
        let initial_keys = local_log.keys().expect("seeded log");
        assert!(!initial_keys.admits(&node_1.key()));

        // 2. First call publishes `Body::Node` for `node_1`; second call is a no-op.
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_1,
            Some(&initial_keys)
        ));
        let keys_after_1 = local_log.keys().expect("keys after node_1");
        assert!(keys_after_1.admits(&node_1.key()));
        assert!(!ensure_local_node_key_in_log(
            &paths,
            &node_1,
            Some(&keys_after_1)
        ));

        let roster_1 = aterm_secrets::client::swarm_roster(&paths)
            .unwrap()
            .unwrap();
        assert_eq!(
            roster_1
                .member_by_label_or_alias("m22")
                .and_then(|m| m.node_key.as_deref()),
            Some(node_1.key().hex().as_str())
        );

        // 3. Rotating to `node_2` admits `node_2` and retires `node_1` (`Standing::Out`).
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_2,
            Some(&keys_after_1)
        ));
        let keys_after_2 = local_log.keys().expect("keys after node_2");
        assert!(keys_after_2.admits(&node_2.key()));
        assert!(keys_after_2.holds_out(&node_1.key()));

        let roster_2 = aterm_secrets::client::swarm_roster(&paths)
            .unwrap()
            .unwrap();
        assert_eq!(
            roster_2
                .member_by_label_or_alias("m22")
                .and_then(|m| m.node_key.as_deref()),
            Some(node_2.key().hex().as_str())
        );

        // 4. Admit a second member `m7` via the membership log, verify its node key is admitted,
        //    then remove `m7` from `[machines]` (which rotates `EpochAudience::Everyone` leaving
        //    `m7` out) and verify `LocalLog::of` now holds `m7`'s node key out (`Standing::Out`).
        let m7_home = root.path().join("m7-home");
        let m7_paths = aterm_secrets::client::Paths::for_home(&m7_home);
        let _ = aterm_secrets::client::machine_identity(&m7_paths).unwrap();
        let dev7 = aterm_secrets::commons::bootstrap::Device::open(&m7_paths)
            .unwrap()
            .unwrap();
        let store = aterm_secrets::client::load_records(&paths).unwrap();
        let log = store.log().unwrap();
        let aterm_secrets::membership_log::Body::Genesis { root: log_root, .. } =
            log.genesis().body()
        else {
            panic!("expected Genesis");
        };
        let req7 = dev7.request(log_root.network, log_root.authority).unwrap();
        let node_7 = NodeIdentity::from_seed(&"b2".repeat(16), &[0x33; 32]).unwrap();
        let admit_res = aterm_secrets::client::admit_by_phrase(
            &paths,
            &req7,
            *node_7.key().bytes(),
            "m7",
            1_000,
        )
        .unwrap();
        assert!(matches!(
            admit_res,
            aterm_secrets::client::ByLog::Admitted(_)
        ));
        let _ = aterm_secrets::client::post_records_to_servers(&paths).unwrap();
        let keys_with_m7 = local_log.keys().expect("keys with m7");
        assert!(keys_with_m7.admits(&node_7.key()));

        // Narrowing `aterm-commons-control-v1` leaves `m7` out of `EpochAudience::Commons`,
        // while keeping `m7` admitted in `EpochAudience::Everyone` (and in `LocalLog::of`).
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc_commons_only = admin.doc.clone();
        let ctrl = doc_commons_only
            .entries()
            .into_iter()
            .find(|e| e.name == aterm_secrets::commons::ENTRY)
            .expect("commons control entry");
        doc_commons_only
            .unset_entry(&ctrl.audience, aterm_secrets::commons::ENTRY)
            .unwrap();
        doc_commons_only
            .set_entry(
                &aterm_secrets::text::Audience::Hosts(vec!["m22".to_owned()]),
                aterm_secrets::commons::ENTRY,
                ctrl.value.as_slice(),
            )
            .unwrap();
        aterm_secrets::client::admin_publish(&paths, &admin, &doc_commons_only).unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys after commons narrowed")
                .admits(&node_7.key()),
            "member left out of EpochAudience::Commons only is still a fleet member"
        );

        // Denying `m7` in `aterm-swarm-roster` (before removing from `[machines]`) also holds
        // `node_7` out in `LocalLog::of`.
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc_roster_deny = admin.doc.clone();
        let mut r = aterm_secrets::swarm_roster::SwarmRoster::from_document(&doc_roster_deny)
            .unwrap()
            .unwrap_or_default();
        r.deny_labels.push("m7".to_owned());
        doc_roster_deny
            .set_entry(
                &aterm_secrets::text::Audience::Everyone,
                aterm_secrets::SWARM_ROSTER_ENTRY,
                r.to_toml().as_bytes(),
            )
            .unwrap();
        aterm_secrets::client::admin_publish(&paths, &admin, &doc_roster_deny).unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys after roster deny")
                .holds_out(&node_7.key()),
            "member denied in aterm-swarm-roster must be held out by LocalLog"
        );

        // Removing `m7` from `[machines]` rotates `EpochAudience::Everyone` leaving `m7` out,
        // so `LocalLog::of` now holds `node_7` out (`Standing::Out`).
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        assert!(doc.remove_machine("m7", false).unwrap());
        aterm_secrets::client::admin_publish(&paths, &admin, &doc).unwrap();
        let keys_after_m7_out = local_log.keys().expect("keys after m7 excluded");
        assert!(
            keys_after_m7_out.holds_out(&node_7.key()),
            "member left out of EpochAudience::Everyone must be held out by LocalLog"
        );
        let pinned_url = format!("{url}#{}", created.signer);
        aterm_secrets::client::configure_log_member(&m7_paths, &pinned_url, "m7").unwrap();
        aterm_secrets::client::sync_records(&m7_paths, None).unwrap();
        assert!(
            !ensure_local_node_key_in_log(&m7_paths, &node_7, None),
            "member left out of EpochAudience::Everyone must not author Body::Node"
        );

        let now = 1_760_000_000_000_u64;
        let node_unproved = NodeIdentity::from_seed(&"c3".repeat(16), &[0x44; 32]).unwrap();
        let mut book = Vec::new();
        let served = [
            aterm_net::fleet_member::Peer::own(&node_2, ("m22.local", 7100, "0.146.0"), now),
            aterm_net::fleet_member::Peer::own(&node_unproved, ("m3.local", 7100, "0.146.0"), now),
            aterm_net::fleet_member::Peer::own(&node_1, ("m22-old.local", 7100, "0.146.0"), now),
            aterm_net::fleet_member::Peer::own(&node_7, ("m7.local", 7100, "0.146.0"), now),
        ];
        assert_eq!(
            aterm_net::fleet_member::merge_signed(
                &mut book,
                &served,
                (None, now),
                (
                    Some(&keys_after_m7_out),
                    &[],
                    &aterm_net::fleet_member::Keyring::default(),
                ),
            ),
            2
        );
        assert_eq!(
            book.iter().filter_map(|p| p.key).collect::<Vec<_>>(),
            vec![node_2.key(), node_unproved.key()]
        );

        running.stop();
    }
}
