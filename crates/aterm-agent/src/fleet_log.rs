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
    let member_denied_in_roster =
        |r: &aterm_secrets::swarm_roster::SwarmRoster, rec: &str, label: &str, name: &str| {
            if let Some(rm) = r.member_by_recipient(rec) {
                r.is_member_denied(rm)
            } else {
                aterm_secrets::swarm_roster::leading_m_label(label)
                    .or_else(|| aterm_secrets::swarm_roster::leading_m_label(name))
                    .is_some_and(|(_, c)| r.is_label_denied(&c))
            }
        };
    let mut entries: std::collections::BTreeMap<NodeKey, Standing> = membership
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
                    ) || membership.get(actor).is_some_and(|m| {
                        log.audience_left_out_recipient(
                            &aterm_secrets::membership_log::EpochAudience::Everyone,
                            &m.binding.recipient(),
                        )
                    }) =>
                {
                    Standing::Out(
                        "the vault's newest key leaves the Mac that holds it out".to_owned(),
                    )
                }
                NodeStanding::Member(actor)
                    if membership.get(actor).is_some_and(|m| {
                        roster.is_some_and(|r| {
                            r.is_node_denied(&aterm_codec::hex::encode_lower(key))
                                || member_denied_in_roster(
                                    r,
                                    &m.binding.recipient().to_bech32(),
                                    &m.label,
                                    &m.name,
                                )
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
        .collect();
    if let aterm_secrets::membership_log::Body::Genesis { founders, .. } = log.genesis().body() {
        let founder_out = |founder: &aterm_secrets::membership_log::Founder,
                           nk: [u8; 32]|
         -> Option<&'static str> {
            if log.recipient_is_removed(&founder.recipient) {
                Some("the membership log removed the Mac that holds it")
            } else if log.audience_left_out_recipient(
                &aterm_secrets::membership_log::EpochAudience::Everyone,
                &founder.recipient,
            ) {
                Some("the vault's newest key leaves the Mac that holds it out")
            } else if membership.members().values().any(|lm| {
                lm.binding.recipient() == founder.recipient
                    && lm.node_key.is_some_and(|mk| mk != [0; 32] && mk != nk)
            }) {
                Some("its member bound another node key since")
            } else if roster.is_some_and(|r| {
                r.is_node_denied(&aterm_codec::hex::encode_lower(&nk))
                    || member_denied_in_roster(
                        r,
                        &founder.recipient.to_bech32(),
                        &founder.label,
                        &founder.name,
                    )
            }) {
                Some("the swarm roster denies the Mac that holds it")
            } else {
                None
            }
        };
        for founder in founders {
            let Some(nk) = founder.node_key.filter(|k| *k != [0; 32]) else {
                continue;
            };
            let key = NodeKey::from_bytes(nk);
            if entries.contains_key(&key) {
                continue;
            }
            if let Some(why) = founder_out(founder, nk)
                && !founders
                    .iter()
                    .any(|f| f.node_key == Some(nk) && founder_out(f, nk).is_none())
            {
                entries.insert(key, Standing::Out(why.to_owned()));
            }
        }
    }
    if let Some(r) = roster {
        let roster_member_out =
            |m: &aterm_secrets::swarm_roster::SwarmMember, key: NodeKey| -> Option<&'static str> {
                let rec = aterm_secrets::age::Recipient::parse(m.recipient.trim()).ok();
                if rec
                    .as_ref()
                    .is_some_and(|rec| log.recipient_is_removed(rec))
                {
                    Some("the membership log removed the Mac that holds it")
                } else if rec.as_ref().is_some_and(|rec| {
                    log.audience_left_out_recipient(
                        &aterm_secrets::membership_log::EpochAudience::Everyone,
                        rec,
                    )
                }) {
                    Some("the vault's newest key leaves the Mac that holds it out")
                } else if rec.as_ref().is_some_and(|rec| {
                    membership.members().values().any(|lm| {
                        lm.binding.recipient() == *rec
                            && lm
                                .node_key
                                .is_some_and(|nk| nk != [0; 32] && nk != *key.bytes())
                    })
                }) {
                    Some("its member bound another node key since")
                } else if r.is_member_denied(m) {
                    Some("the swarm roster denies the Mac that holds it")
                } else {
                    None
                }
            };
        for rm in &r.members {
            if let Some(key) = rm
                .node_key
                .as_deref()
                .and_then(|k| NodeKey::from_hex(&k.trim().to_ascii_lowercase()))
                && let Some(why) = roster_member_out(rm, key)
            {
                let key_hex = key.hex();
                let claimed_by_active = !r.is_node_denied(&key_hex)
                    && r.members.iter().any(|m| {
                        roster_member_out(m, key).is_none()
                            && m.node_key
                                .as_deref()
                                .is_some_and(|k| k.trim().eq_ignore_ascii_case(&key_hex))
                    });
                if !claimed_by_active {
                    entries
                        .entry(key)
                        .or_insert_with(|| Standing::Out(why.to_owned()));
                }
            }
        }
        for denied in &r.deny_nodes {
            if let Some(key) = NodeKey::from_hex(&denied.trim().to_ascii_lowercase()) {
                entries.entry(key).or_insert_with(|| {
                    Standing::Out("the swarm roster denies the Mac that holds it".to_owned())
                });
            }
        }
    }
    entries.into_iter().collect()
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
    paths: Option<aterm_secrets::client::Paths>,
    aux_files: Vec<PathBuf>,
    read: Reader,
    held: Mutex<Option<Held>>,
}

struct Held {
    stamp: Stamp,
    aux_stamps: Vec<Option<Stamp>>,
    keys: Option<Arc<MemberKeys>>,
}

impl LocalLog {
    /// The store `paths` keeps (`Paths::records_file`, read by
    /// `aterm_secrets::client::load_records`).
    #[must_use]
    pub fn of(paths: &aterm_secrets::client::Paths) -> Self {
        let from = paths.clone();
        let mut log = Self::reading(
            paths.records_file(),
            vec![
                paths.dir.join("state.toml"),
                paths.dir.join("vault"),
                paths.dir.join("source.toml"),
                paths.dir.join("identity"),
            ],
            Box::new(move || {
                let store = aterm_secrets::client::load_records(&from)?;
                let Some(log) = store.log() else {
                    return Ok(None);
                };
                let roster = aterm_secrets::client::local_swarm_roster_for_log(&from, log)?;
                Ok(Some(log_member_keys_with_roster(log, roster.as_ref())))
            }),
        );
        log.paths = Some(paths.clone());
        log
    }

    /// The store file `file`, founded on the genesis `network` names: a test's lab of its own.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn founded_at(file: PathBuf, network: aterm_secrets::membership_log::RecordId) -> Self {
        let from = file.clone();
        Self::reading(
            file,
            Vec::new(),
            Box::new(move || {
                use std::io::Read as _;
                const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;
                let (file, meta) = aterm_uds::regular_file::open(&from)
                    .map_err(|e| aterm_secrets::Error::new(format!("the record store: {e}")))?;
                if meta.len() > MAX_STORE_BYTES {
                    return Err(aterm_secrets::Error::new(
                        "the record store: file exceeds size limit",
                    ));
                }
                let cap = usize::try_from(meta.len().min(MAX_STORE_BYTES))
                    .unwrap_or(0)
                    .saturating_add(1);
                let mut bytes = Vec::with_capacity(cap);
                file.take(MAX_STORE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| aterm_secrets::Error::new(format!("the record store: {e}")))?;
                if bytes.len() as u64 > MAX_STORE_BYTES {
                    return Err(aterm_secrets::Error::new(
                        "the record store: file exceeds size limit",
                    ));
                }
                let mut store = aterm_secrets::membership_log::Store::with_compiled(Some(network));
                store.ingest(&bytes)?;
                Ok(store.log().map(log_member_keys))
            }),
        )
    }

    fn reading(file: PathBuf, aux_files: Vec<PathBuf>, read: Reader) -> Self {
        Self {
            file,
            paths: None,
            aux_files,
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
        let file = match &self.paths {
            Some(paths) => paths.records_data_file().ok()?,
            None => self.file.clone(),
        };
        let file_stamp = stamp(&file)?;
        let aux_stamps: Vec<Option<Stamp>> = self.aux_files.iter().map(|p| stamp(p)).collect();
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(held) = held.as_ref()
            && held.stamp == file_stamp
            && held.aux_stamps == aux_stamps
        {
            return held.keys.clone();
        }
        let Ok(keys) = (self.read)() else {
            return held.as_ref().and_then(|held| held.keys.clone());
        };
        let keys = keys.map(Arc::new);
        *held = Some(Held {
            stamp: file_stamp,
            aux_stamps,
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

        // Independent members keep roster denial from pre-removing the Everyone-removal case.
        let admit_member = |label: &str, node: &NodeIdentity| {
            let member_home = root.path().join(format!("{label}-home"));
            let member_paths = aterm_secrets::client::Paths::for_home(&member_home);
            let _ = aterm_secrets::client::machine_identity(&member_paths).unwrap();
            let device = aterm_secrets::commons::bootstrap::Device::open(&member_paths)
                .unwrap()
                .unwrap();
            let store = aterm_secrets::client::load_records(&paths).unwrap();
            let log = store.log().unwrap();
            let aterm_secrets::membership_log::Body::Genesis { root: log_root, .. } =
                log.genesis().body()
            else {
                panic!("expected Genesis");
            };
            let request = device
                .request(log_root.network, log_root.authority)
                .unwrap();
            let admitted = aterm_secrets::client::admit_by_phrase(
                &paths,
                &request,
                *node.key().bytes(),
                label,
                1_000,
            )
            .unwrap();
            assert!(matches!(
                admitted,
                aterm_secrets::client::ByLog::Admitted(_)
            ));
            let _ = aterm_secrets::client::post_records_to_servers(&paths).unwrap();
            member_paths
        };
        let node_7 = NodeIdentity::from_seed(&"b2".repeat(16), &[0x33; 32]).unwrap();
        let m7_paths = admit_member("m7", &node_7);
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

        // Roster denial is independent: it may already prune that member's machine line.
        let node_8 = NodeIdentity::from_seed(&"d4".repeat(16), &[0x66; 32]).unwrap();
        let _m8_paths = admit_member("m8", &node_8);
        assert!(
            local_log
                .keys()
                .expect("keys with m8")
                .admits(&node_8.key()),
            "roster-denial control starts admitted"
        );
        let records_with_m8 = std::fs::read(paths.records_file()).unwrap();
        let vault_with_m8 = std::fs::read(paths.dir.join("vault")).unwrap();
        let state_with_m8 = std::fs::read(paths.dir.join("state.toml")).unwrap();
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc_roster_deny = admin.doc.clone();
        let mut r = aterm_secrets::swarm_roster::SwarmRoster::from_document(&doc_roster_deny)
            .unwrap()
            .unwrap_or_default();
        r.deny_labels.push("m8".to_owned());
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
                .holds_out(&node_8.key()),
            "member denied in aterm-swarm-roster must be held out by LocalLog"
        );

        // Only vault/state change here; the records remain at m8's admitted snapshot.
        let records_after_deny = std::fs::read(paths.records_file()).unwrap();
        let vault_with_deny = std::fs::read(paths.dir.join("vault")).unwrap();
        let state_with_deny = std::fs::read(paths.dir.join("state.toml")).unwrap();
        aterm_secrets::fsx::write_atomic(&paths.records_file(), &records_with_m8, 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(&paths.dir.join("vault"), &vault_with_m8, 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(&paths.dir.join("state.toml"), &state_with_m8, 0o600)
            .unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys before vault-only roster deny")
                .admits(&node_8.key()),
            "before vault update, node_8 is admitted"
        );
        aterm_secrets::fsx::write_atomic(&paths.dir.join("vault"), &vault_with_deny, 0o600)
            .unwrap();
        aterm_secrets::fsx::write_atomic(&paths.dir.join("state.toml"), &state_with_deny, 0o600)
            .unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys after vault-only roster deny")
                .holds_out(&node_8.key()),
            "vault/state.toml alone invalidates LocalLog cache and holds out denied m8"
        );
        aterm_secrets::fsx::write_atomic(&paths.records_file(), &records_after_deny, 0o600)
            .unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys after restoring denied records")
                .holds_out(&node_8.key())
        );

        // Removing `m7` from `[machines]` without roster-denying m7 rotates
        // `EpochAudience::Everyone` leaving `m7` out, so `LocalLog::of` holds `node_7` out.
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        assert!(
            doc.machines().iter().any(|m| m.name == "m7"),
            "Everyone-removal control starts with its machine present"
        );
        let roster = aterm_secrets::swarm_roster::SwarmRoster::from_document(&doc)
            .unwrap()
            .unwrap();
        assert!(
            !roster.deny_labels.iter().any(|label| label == "m7"),
            "the independent roster-denial case must not deny m7"
        );
        assert!(
            local_log
                .keys()
                .expect("m7 before removal")
                .admits(&node_7.key()),
            "Everyone-removal control starts admitted"
        );
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

        // A pre-log/unproved member whose 64-hex node key appears only in `aterm-swarm-roster`
        // (and is denied there) is also held out by `log_member_keys_with_roster`.
        let store_now = aterm_secrets::client::load_records(&paths).unwrap();
        let log_now = store_now.log().unwrap();
        let mut r_unproved = aterm_secrets::swarm_roster::SwarmRoster::default();
        r_unproved.deny_nodes.push(node_unproved.key().hex());
        let keys_with_roster_deny = log_member_keys_with_roster(log_now, Some(&r_unproved));
        assert!(keys_with_roster_deny.holds_out(&node_unproved.key()));

        // A stale denied row in `r.members` for the same recipient must not shadow a newer active
        // row for that recipient (either for a log-bound member `m22` or a pre-log member).
        let m22_rec = aterm_secrets::client::machine_identity(&paths)
            .unwrap()
            .recipient()
            .to_bech32();
        let mut r_readmitted = aterm_secrets::swarm_roster::SwarmRoster {
            deny_labels: vec!["m22-old".to_owned(), "m3-old".to_owned()],
            deny_nodes: Vec::new(),
            members: vec![
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m22-old".to_owned(),
                    vault_name: "m22-old".to_owned(),
                    recipient: m22_rec.clone(),
                    node_id: Some("a1".repeat(16)),
                    node_key: Some(node_2.key().hex()),
                    admitted_ms: 100,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m22".to_owned(),
                    vault_name: "m22".to_owned(),
                    recipient: m22_rec,
                    node_id: Some("a1".repeat(16)),
                    node_key: Some(node_2.key().hex()),
                    admitted_ms: 200,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m3-old".to_owned(),
                    vault_name: "m3-old".to_owned(),
                    recipient: "age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq96l3dp"
                        .to_owned(),
                    node_id: Some("c3".repeat(16)),
                    node_key: Some(node_unproved.key().hex()),
                    admitted_ms: 100,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m3".to_owned(),
                    vault_name: "m3".to_owned(),
                    recipient: "age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq96l3dp"
                        .to_owned(),
                    node_id: Some("c3".repeat(16)),
                    node_key: Some(node_unproved.key().hex()),
                    admitted_ms: 200,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
            ],
        };
        let keys_readmitted = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(
            keys_readmitted.admits(&node_2.key()),
            "active row for m22 must keep node_2 admitted despite stale denied row"
        );
        assert!(
            !keys_readmitted.holds_out(&node_unproved.key()),
            "active pre-log row for m3 must prevent stale denied row from holding out node_unproved"
        );
        r_readmitted.deny_labels.push("m22".to_owned());
        r_readmitted.deny_labels.push("m3".to_owned());
        let keys_all_denied = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(keys_all_denied.holds_out(&node_2.key()));
        assert!(keys_all_denied.holds_out(&node_unproved.key()));

        // A member renumbered in the roster (e.g. `label = "m70"`) whose original log `label`/`name`
        // (`"m22"`) is in `deny_labels` must still be admitted while its roster row `m70` is active.
        r_readmitted.members[1].label = "m70".to_owned();
        r_readmitted.members[1].vault_name = "m22-2".to_owned();
        let keys_renumbered = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(
            keys_renumbered.admits(&node_2.key()),
            "renumbered active roster row m70 must not be held out by denied log label/name m22"
        );
        r_readmitted.deny_labels.push("m70".to_owned());
        let keys_renumbered_denied = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(keys_renumbered_denied.holds_out(&node_2.key()));

        // A roster row whose recipient bound a newer node key in the log or was left out of
        // `EpochAudience::Everyone` holds out its stale/pre-log `node_key` even without `deny_labels`.
        let node_0 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x05; 32]).unwrap();
        let node_7_pre = NodeIdentity::from_seed(&"b2".repeat(16), &[0x55; 32]).unwrap();
        assert!(
            ![
                node_1.key(),
                node_2.key(),
                node_7.key(),
                node_unproved.key(),
                node_0.key(),
                node_7_pre.key(),
            ]
            .contains(&node_8.key()),
            "roster denial must have a key distinct from every other control"
        );
        let m7_rec = aterm_secrets::client::machine_identity(&m7_paths)
            .unwrap()
            .recipient()
            .to_bech32();
        let r_stale_and_left_out = aterm_secrets::swarm_roster::SwarmRoster {
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
            members: vec![
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m22".to_owned(),
                    vault_name: "m22".to_owned(),
                    recipient: aterm_secrets::client::machine_identity(&paths)
                        .unwrap()
                        .recipient()
                        .to_bech32(),
                    node_id: Some("a1".repeat(16)),
                    node_key: Some(node_0.key().hex()),
                    admitted_ms: 50,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m7".to_owned(),
                    vault_name: "m7".to_owned(),
                    recipient: m7_rec,
                    node_id: Some("b2".repeat(16)),
                    node_key: Some(node_7_pre.key().hex()),
                    admitted_ms: 50,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
            ],
        };
        let keys_stale_and_left_out =
            log_member_keys_with_roster(log_now, Some(&r_stale_and_left_out));
        assert!(keys_stale_and_left_out.holds_out(&node_0.key()));
        assert!(keys_stale_and_left_out.holds_out(&node_7_pre.key()));

        // A crossed source also fences this small local file. The same warm reader must
        // follow the selected data, observe another rotation, and refuse missing custody.
        aterm_secrets::fsx::write_atomic(
            &paths.records_file().with_extension("crossed"),
            b"crossed\n",
            0o600,
        )
        .unwrap();
        let node_3 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x77; 32]).unwrap();
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_3,
            Some(&keys_after_m7_out)
        ));
        assert!(paths.records_file().is_dir());
        let keys_after_3 = local_log.keys().expect("keys in the fenced layout");
        assert!(keys_after_3.admits(&node_3.key()));
        assert!(keys_after_3.holds_out(&node_2.key()));
        let node_4 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x88; 32]).unwrap();
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_4,
            Some(&keys_after_3)
        ));
        let keys_after_4 = local_log.keys().expect("keys after fenced rotation");
        assert!(keys_after_4.admits(&node_4.key()));
        assert!(keys_after_4.holds_out(&node_3.key()));
        std::fs::rename(
            paths.records_data_file().unwrap(),
            paths.records_file().join("saved-current"),
        )
        .unwrap();
        assert!(local_log.keys().is_none());

        running.stop();
    }
}
