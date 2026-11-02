// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The only two holder sources: an explicit local give, or a compiled catalog path.
//! No packet, record, entry name or environment variable chooses a path to read.

use super::*;
use aterm_secrets::{
    SecretBytes, fsx,
    need::transfer::{MAX_VALUE, Values},
};
use std::path::PathBuf;

const PREFIX: &str = "credential-give-";
const HEADER: usize = 4 + 16 + 16 + 8;

fn path(paths: &Paths, network: [u8; 16], request: [u8; 16]) -> PathBuf {
    paths.dir.join(format!(
        "{PREFIX}{}-{}",
        hex::encode_lower(&network),
        hex::encode_lower(&request)
    ))
}

fn header(paths: &Paths, network: [u8; 16], request: [u8; 16]) -> Result<Option<u64>> {
    use std::io::Read as _;
    let file = path(paths, network, request);
    if !file.try_exists()? {
        return Ok(None);
    }
    let name = file.file_name().unwrap().to_string_lossy();
    let (mut file, _) = aterm_uds::private_file::open_private(&paths.dir, &name)
        .map_err(|_| Error::new("the retained give is not a private regular file"))?;
    let mut bytes = [0; HEADER];
    file.read_exact(&mut bytes)?;
    if &bytes[..4] != b"GIV1" || bytes[4..20] != network || bytes[20..36] != request {
        return Err(Error::new("the retained give header is malformed"));
    }
    Ok(Some(u64::from_be_bytes(bytes[36..44].try_into().unwrap())))
}

pub(super) fn held(paths: &Paths, network: [u8; 16], thread: &Thread, now_ms: u64) -> bool {
    // The give header is public metadata; credential bytes are opened only after custody.
    header(paths, network, thread.id)
        .ok()
        .flatten()
        .is_some_and(|expiry| now_ms < expiry)
        || holds(paths, &thread.want)
}

/// Retain one value per request. The need ledger lock serializes local contributions and
/// cleanup, and an existing different value is never overwritten by another give.
pub(super) fn keep(
    paths: &Paths,
    network: [u8; 16],
    thread: &Thread,
    values: &Values,
) -> Result<()> {
    let encoded = values.encode().map_err(crate::table::failure)?;
    let expiry = now_ms().saturating_add(need::WANT_WINDOW_SECS * MS);
    let mut bytes = zeroize::Zeroizing::new(b"GIV1".to_vec());
    bytes.extend_from_slice(&network);
    bytes.extend_from_slice(&thread.id);
    bytes.extend_from_slice(&expiry.to_be_bytes());
    bytes.extend_from_slice(encoded.as_slice());
    need::with_ledger(paths, |_, _| {
        let path = path(paths, network, thread.id);
        if let Some(existing) = fsx::read_private(&path, MAX_VALUE + 2048)? {
            if existing.as_slice().get(HEADER..) != Some(encoded.as_slice())
                || existing.as_slice().get(..36) != Some(&bytes[..36])
            {
                return Err(aterm_secrets::Error::new(
                    "this request already has a different retained give",
                ));
            }
        } else if !fsx::write_new_ordered(&path, &bytes, 0o600)? {
            return Err(aterm_secrets::Error::new(
                "another give arrived for this request",
            ));
        }
        Ok(())
    })
    .map_err(crate::table::failure)
}

pub(super) fn read(
    paths: &Paths,
    network: [u8; 16],
    thread: &Thread,
    now_ms: u64,
) -> Result<Values> {
    if let Some(bytes) = fsx::read_private(&path(paths, network, thread.id), MAX_VALUE + 2048)
        .map_err(crate::table::failure)?
    {
        let bytes = bytes.as_slice();
        if bytes.len() < HEADER
            || &bytes[..4] != b"GIV1"
            || bytes[4..20] != network
            || bytes[20..36] != thread.id
        {
            return Err(Error::new("credential give file is malformed"));
        }
        let expiry = u64::from_be_bytes(bytes[36..44].try_into().unwrap());
        if now_ms < expiry {
            return Values::decode(&bytes[HEADER..]).map_err(crate::table::failure);
        }
    }
    let first = thread
        .want
        .hold_path(&paths.home)
        .ok_or_else(|| Error::new("this want has no compiled holder source"))?;
    let value = fsx::read_private(&first, MAX_VALUE)
        .map_err(crate::table::failure)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::new("the holder's compiled source is absent"))?;
    let mut values = vec![(thread.want.entries()[0].clone(), value)];
    if let Want::Mailbox(label) = &thread.want {
        let totp = paths.home.join(".secrets").join(format!("{label}_totp"));
        if let Some(value) = fsx::read_private(&totp, MAX_VALUE).map_err(crate::table::failure)? {
            values.push((format!("{label}_totp"), value));
        }
    }
    let values = Values(values);
    values.encode().map_err(crate::table::failure)?;
    Ok(values)
}

/// Remove only the exact program-owned contribution after a proven publication, or after
/// the request ended/expired. Never follows a symlink or accepts a peer-supplied path.
pub(super) fn forget(paths: &Paths, network: [u8; 16], request: [u8; 16]) -> Result<()> {
    need::with_ledger(paths, |_, _| {
        let path = path(paths, network, request);
        if std::fs::symlink_metadata(&path).is_ok() {
            let name = path.file_name().unwrap().to_string_lossy();
            aterm_uds::private_file::open_private(&paths.dir, &name).map_err(|_| {
                aterm_secrets::Error::new("the retained give is not a private regular file")
            })?;
            std::fs::remove_file(&path)?;
            aterm_durable::ordered_dir(&paths.dir)?;
        }
        Ok(())
    })
    .map_err(crate::table::failure)
}

pub(super) fn cleanup(paths: &Paths, network: [u8; 16], book: &Book, now_ms: u64) -> Result<()> {
    for file in std::fs::read_dir(&paths.dir)? {
        let file = file?;
        let name = file.file_name();
        let Some((net, request)) = name
            .to_str()
            .and_then(|name| name.strip_prefix(PREFIX))
            .and_then(|name| name.split_once('-'))
        else {
            continue;
        };
        let (Ok(net), Ok(request)) = (ids(net), ids(request)) else {
            continue;
        };
        if path(paths, net, request) != file.path() {
            continue;
        }
        let Some(expiry) = header(paths, net, request)? else {
            continue;
        };
        let ended = net == network
            && book
                .threads
                .get(&request)
                .is_some_and(|thread| thread.ended() || thread.published.is_some());
        if ended || now_ms >= expiry {
            forget(paths, net, request)?;
        }
    }
    Ok(())
}

pub(super) fn single(want: &Want, value: SecretBytes) -> Result<Values> {
    let entry = want
        .entries()
        .into_iter()
        .next()
        .ok_or_else(|| Error::new("this want cannot be contributed"))?;
    Ok(Values(vec![(entry, value)]))
}
