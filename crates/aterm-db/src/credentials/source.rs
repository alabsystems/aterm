// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Holder sources are an explicit local give, a compiled catalog path, or one
//! exact component of the authenticated registered mailbox source after custody.
//! No packet, record, entry name or environment variable chooses a path to read.

use super::*;
use aterm_secrets::{
    SecretBytes, fsx,
    need::transfer::{MAX_VALUE, Values},
};
use std::path::PathBuf;

const PREFIX: &str = "credential-give-";
const HEADER: usize = 4 + 16 + 16 + 8;

fn name(network: [u8; 16], request: [u8; 16]) -> String {
    let mut out = String::with_capacity(PREFIX.len() + 32 + 1 + 32);
    out.push_str(PREFIX);
    hex::push_lower(&mut out, &network);
    out.push('-');
    hex::push_lower(&mut out, &request);
    out
}

fn path(paths: &Paths, network: [u8; 16], request: [u8; 16]) -> PathBuf {
    paths.dir.join(name(network, request))
}

fn header(paths: &Paths, network: [u8; 16], request: [u8; 16]) -> Result<Option<u64>> {
    header_named(&paths.dir, &name(network, request), network, request)
}

fn header_named(
    dir: &std::path::Path,
    name: &str,
    network: [u8; 16],
    request: [u8; 16],
) -> Result<Option<u64>> {
    use std::io::Read as _;
    let (mut file, _) = match aterm_uds::private_file::open_private(dir, name) {
        Ok(opened) => opened,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
        Err(_) => {
            return Err(Error::new(
                "the retained give is not a private regular file",
            ));
        }
    };
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
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(HEADER + encoded.as_slice().len()));
    bytes.extend_from_slice(b"GIV1");
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
    if let Want::MailboxComponent(component) = &thread.want {
        let entry = component.entry();
        let file = paths.home.join(".secrets").join(&entry);
        let value = fsx::read_private(&file, MAX_VALUE)
            .map_err(crate::table::failure)?
            .filter(|value| value::is_line(value.as_slice()));
        let value = match value {
            Some(value) => Some(value),
            None => client::read_mailbox_component(paths, *component)
                .map_err(crate::table::failure)?
                .filter(|value| value::is_line(value.as_slice())),
        }
        .ok_or_else(|| Error::new("the registered mailbox component is absent or changed"))?;
        let values = Values(vec![(entry, value)]);
        values.encode().map_err(crate::table::failure)?;
        return Ok(values);
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
        if let Some(value) = fsx::read_private(&totp, MAX_VALUE)
            .map_err(crate::table::failure)?
            .filter(|value| value::is_line(value.as_slice()))
        {
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
        let name = name(network, request);
        match aterm_uds::private_file::open_private(&paths.dir, &name) {
            Ok(_) => {
                std::fs::remove_file(paths.dir.join(&name))?;
                aterm_durable::ordered_dir(&paths.dir)?;
            }
            Err(aterm_uds::private_file::PrivateFileError::Missing) => {}
            Err(_) => {
                return Err(aterm_secrets::Error::new(
                    "the retained give is not a private regular file",
                ));
            }
        }
        Ok(())
    })
    .map_err(crate::table::failure)
}

pub(super) fn cleanup(paths: &Paths, network: [u8; 16], book: &Book, now_ms: u64) -> Result<()> {
    for file in std::fs::read_dir(&paths.dir)? {
        let file = file?;
        let name = file.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some((net, request)) = name_str
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.split_once('-'))
        else {
            continue;
        };
        let (Ok(net), Ok(request)) = (ids(net), ids(request)) else {
            continue;
        };
        let Some(expiry) = header_named(&paths.dir, name_str, net, request)? else {
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
