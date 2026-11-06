// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A possession exchange that may have signed admission remains with that authenticated
//! member across route wakes and restarts. Another member must not sign a second seat move
//! before the first one's records propagate.

use super::join::ProofLock;
use super::joiner::Joining;
use crate::client::Paths;
use crate::commons::binding::Binding;
use crate::{Error, Result, fsx};

const FILE: &str = "native-proof-pending";
const MAGIC: &[u8] = b"ATERM-PROOF-RETRY-1\n";
const ADDRESS: usize = 512;
const FIXED: usize = 8 + 32 + 32 + crate::commons::binding::BINDING_LEN + 2;
const PAUSE: u64 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Peer {
    pub at: String,
    pub key: [u8; 32],
    pub binding: Binding,
}

struct Pending {
    peer: Peer,
    node: [u8; 32],
    after: u64,
}

pub(crate) enum Round {
    Fresh,
    Cooling,
    Pinned(Peer),
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn malformed() -> Error {
    Error::new("the retained native proof retry is malformed; no possession was sent")
}

fn read(paths: &Paths) -> Result<Option<Pending>> {
    let Some(bytes) = fsx::read_private(&paths.dir.join(FILE), MAGIC.len() + FIXED + ADDRESS)?
    else {
        return Ok(None);
    };
    let bytes = bytes.as_slice();
    if !bytes.starts_with(MAGIC) || bytes.len() < MAGIC.len() + FIXED {
        return Err(malformed());
    }
    let mut bytes = &bytes[MAGIC.len()..];
    let (after, rest) = bytes.split_at(8);
    let after = u64::from_le_bytes(after.try_into().map_err(|_| malformed())?);
    bytes = rest;
    let (node, rest) = bytes.split_at(32);
    let node = node.try_into().map_err(|_| malformed())?;
    let (key, rest) = rest.split_at(32);
    let key = key.try_into().map_err(|_| malformed())?;
    let (binding, rest) = rest.split_at(crate::commons::binding::BINDING_LEN);
    let binding = Binding::decode(binding)?;
    let (length, address) = rest.split_at(2);
    let length = u16::from_le_bytes(length.try_into().map_err(|_| malformed())?) as usize;
    if length == 0 || length > ADDRESS || address.len() != length {
        return Err(malformed());
    }
    let at = std::str::from_utf8(address).map_err(|_| malformed())?;
    if !at.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(malformed());
    }
    Ok(Some(Pending {
        peer: Peer {
            at: at.to_owned(),
            key,
            binding,
        },
        node,
        after,
    }))
}

pub(crate) fn pending(paths: &Paths) -> Result<bool> {
    read(paths).map(|pending| pending.is_some())
}

fn write(paths: &Paths, pending: &Pending) -> Result<()> {
    let at = pending.peer.at.as_bytes();
    if at.is_empty() || at.len() > ADDRESS || !at.iter().all(u8::is_ascii_graphic) {
        return Err(malformed());
    }
    let mut bytes = Vec::with_capacity(MAGIC.len() + FIXED + at.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&pending.after.to_le_bytes());
    bytes.extend_from_slice(&pending.node);
    bytes.extend_from_slice(&pending.peer.key);
    bytes.extend_from_slice(&pending.peer.binding.bytes());
    bytes.extend_from_slice(&(at.len() as u16).to_le_bytes());
    bytes.extend_from_slice(at);
    fsx::write_atomic(&paths.dir.join(FILE), &bytes, 0o600)
}

fn same_device(pending: &Pending, joining: &Joining<'_>) -> Result<()> {
    let binding = &pending.peer.binding;
    let request = joining
        .device
        .request(*binding.network(), *binding.authority())?;
    if request.binding() != binding || joining.node != pending.node {
        return Err(Error::new(
            "this identity changed while a native admission remained ambiguous; no new possession was sent",
        ));
    }
    Ok(())
}

/// Read the prior round under the caller's proof lock. Restarting or moving networks neither
/// changes the signing member nor spends its propagation pause. A pin permits trying fresh
/// addresses, but the authenticated witness must name this same key before possession.
pub(crate) fn before(
    paths: &Paths,
    proving: &ProofLock,
    joining: &Joining<'_>,
    now: u64,
) -> Result<Round> {
    proving.guards(paths)?;
    let Some(mut pending) = read(paths)? else {
        return Ok(Round::Fresh);
    };
    same_device(&pending, joining)?;
    if now < pending.after {
        return Ok(Round::Cooling);
    }
    // A failed reconnect also keeps route bursts from redialing every instant.
    pending.after = now.saturating_add(PAUSE);
    write(paths, &pending)?;
    Ok(Round::Pinned(pending.peer))
}

/// Retain the authenticated destination and exact request BEFORE possession can leave.
/// A failed write sends nothing. Only the same member may receive a later possession.
pub(crate) fn sending(
    paths: &Paths,
    proving: &ProofLock,
    joining: &Joining<'_>,
    at: &str,
    key: [u8; 32],
    binding: &Binding,
    now: u64,
) -> Result<()> {
    proving.guards(paths)?;
    if let Some(pending) = read(paths)? {
        same_device(&pending, joining)?;
        if pending.peer.key != key || &pending.peer.binding != binding {
            return Err(Error::new(
                "another native member may have signed; no possession was sent",
            ));
        }
    }
    write(
        paths,
        &Pending {
            peer: Peer {
                at: at.to_owned(),
                key,
                binding: binding.clone(),
            },
            node: joining.node,
            after: now.saturating_add(PAUSE),
        },
    )
}

/// Clear ambiguity only after a verified admission was committed locally.
pub(crate) fn clear(paths: &Paths, proving: &ProofLock) -> Result<()> {
    proving.guards(paths)?;
    fsx::wipe_and_remove(&paths.dir.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commons::bootstrap::Device;

    #[test]
    fn restart_and_route_wakes_keep_the_same_member_request_and_pause() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        crate::client::machine_identity(&paths).unwrap();
        let device = Device::open(&paths).unwrap().unwrap();
        let joining = Joining {
            device: &device,
            records_bound: crate::client::records_bound(
                &paths,
                &crate::client::load_records(&paths).unwrap(),
            )
            .unwrap(),
            node: [7; 32],
            name: "newcomer".into(),
            model: String::new(),
        };
        let request = device.request([1; 16], [2; 32]).unwrap();
        let proving = ProofLock::take(&paths, None).unwrap().unwrap();
        assert!(matches!(
            before(&paths, &proving, &joining, 100).unwrap(),
            Round::Fresh
        ));
        sending(
            &paths,
            &proving,
            &joining,
            "127.0.0.1:7",
            [3; 32],
            request.binding(),
            100,
        )
        .unwrap();
        drop(proving);
        let restarted = Paths::for_home(root.path());
        let proving = ProofLock::take(&restarted, None).unwrap().unwrap();
        assert!(matches!(
            before(&restarted, &proving, &joining, 129).unwrap(),
            Round::Cooling
        ));
        let Round::Pinned(peer) = before(&restarted, &proving, &joining, 130).unwrap() else {
            panic!("pin lost across restart")
        };
        assert_eq!(peer.key, [3; 32]);
        assert_eq!(peer.binding, *request.binding());
        assert!(
            sending(
                &restarted,
                &proving,
                &joining,
                "127.0.0.1:8",
                [4; 32],
                request.binding(),
                130
            )
            .is_err()
        );
        assert!(matches!(
            before(&restarted, &proving, &joining, 131).unwrap(),
            Round::Cooling
        ));
        clear(&restarted, &proving).unwrap();
        assert!(matches!(
            before(&restarted, &proving, &joining, 131).unwrap(),
            Round::Fresh
        ));
    }
}
