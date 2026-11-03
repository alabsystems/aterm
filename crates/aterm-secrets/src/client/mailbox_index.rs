// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A non-secret component-presence index, minted while the registered mailbox
//! entry is already open for authorized use or materialization. Holder probes
//! read this index and signed-version metadata, never mailbox plaintext.

use super::*;
use crate::mailbox_component::{Component, InlineSource, SOURCE_ENTRY};
use std::os::unix::fs::MetadataExt as _;

const INDEX: &str = "mailbox-components.toml";
const MAX_INDEX: usize = 256 * 1024;

struct Index {
    stamp: [u8; 32],
    file: String,
    components: BTreeSet<Component>,
}

fn file_stamp(paths: &Paths) -> Option<String> {
    let path = materialize::target_path(&paths.home, SOURCE_ENTRY).ok()?;
    let mut parent = path.parent();
    loop {
        let directory = parent.filter(|directory| directory.starts_with(&paths.home))?;
        if fsx::kind(directory).ok()? != fsx::Kind::Dir {
            return None;
        }
        if directory == paths.home {
            break;
        }
        parent = directory.parent();
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != fsx::euid()
        || meta.mode() & 0o6077 != 0
        || meta.len() > 4 * 1024 * 1024
    {
        return None;
    }
    Some(format!(
        "{}:{}:{}:{}:{}:{}:{}",
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    ))
}

fn write(paths: &Paths, stamp: [u8; 32], value: &SecretBytes) -> crate::Result<()> {
    let Some(source) = value.as_str().and_then(InlineSource::parse) else {
        return Err(Error::new(
            "registered mailbox configuration is not valid inline configuration",
        ));
    };
    let Some(file) = file_stamp(paths) else {
        return Err(Error::new("registered mailbox file is unavailable"));
    };
    let components: Vec<String> = source
        .components()
        .map(|component| toml_str(&component.key()))
        .collect();
    let text = format!(
        "version = 1\nstamp = {}\nfile = {}\ncomponents = [{}]\n",
        toml_str(&aterm_codec::hex::encode_lower(&stamp)),
        toml_str(&file),
        components.join(", ")
    );
    if text.len() > MAX_INDEX {
        return Err(Error::new("mailbox component index is too large"));
    }
    fsx::write_atomic(&paths.dir.join(INDEX), text.as_bytes(), 0o600)
}

fn read(paths: &Paths) -> crate::Result<Option<Index>> {
    let Some(bytes) = fsx::read_private(&paths.dir.join(INDEX), MAX_INDEX)? else {
        return Ok(None);
    };
    let invalid = || Error::new("invalid mailbox component index");
    let table: aterm_toml::Table =
        aterm_toml::from_str(bytes.as_str().ok_or_else(invalid)?).map_err(|_| invalid())?;
    if table.get("version").and_then(aterm_toml::Value::as_integer) != Some(1) {
        return Err(invalid());
    }
    let stamp = table
        .get("stamp")
        .and_then(aterm_toml::Value::as_str)
        .and_then(|s| aterm_codec::hex::decode_vec(s.as_bytes()))
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(invalid)?;
    let file = table
        .get("file")
        .and_then(aterm_toml::Value::as_str)
        .ok_or_else(invalid)?
        .to_owned();
    let mut components = BTreeSet::new();
    for word in table
        .get("components")
        .and_then(aterm_toml::Value::as_array)
        .ok_or_else(invalid)?
    {
        let component = word
            .as_str()
            .and_then(Component::parse)
            .ok_or_else(invalid)?;
        if !components.insert(component) {
            return Err(invalid());
        }
    }
    Ok(Some(Index {
        stamp,
        file,
        components,
    }))
}

/// Compare only admitted source state and signed encrypted cache/log metadata.
/// This recognizes an unchanged prior verified read; it admits no new plaintext.
fn current_stamp(paths: &Paths) -> crate::Result<Option<[u8; 32]>> {
    let Some(_guard) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let state = load_state(paths)?;
    if config.signer.is_none()
        || !pin_admitted(&config, None)
        || state.membership != Stored::In
        || state.failed
        || !state.pending.is_empty()
        || !state.manifest.files.contains_key(SOURCE_ENTRY)
        || !(source_bound(&config, &state) || log_only_source_bound(&config, &state))
    {
        return Ok(None);
    }
    if state.digest.is_some() && held_copy(paths, &config, &state).is_none() {
        return Ok(None);
    }
    if log_entries::current_records_digest(paths, &config, &state) != state.records_digest {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    Ok(Some(entry::public_state_stamp(
        &config,
        &state,
        identity.recipient(),
    )))
}

/// A holder's metadata-only claim about the fixed registered source.
pub fn mailbox_component_held(paths: &Paths, component: Component) -> crate::Result<bool> {
    let Some(index) = read(paths)? else {
        return Ok(false);
    };
    Ok(index.components.contains(&component)
        && file_stamp(paths).as_deref() == Some(index.file.as_str())
        && current_stamp(paths)? == Some(index.stamp))
}

/// Refresh during an explicitly authorized registered-entry read. The caller
/// must not invoke this from a holder's pre-custody probe.
pub fn refresh_mailbox_component_index(paths: &Paths) -> crate::Result<bool> {
    let Some(config) = config(paths)? else {
        return Ok(false);
    };
    let Some(value) = read_verified_entry(paths, &config, SOURCE_ENTRY)? else {
        return Ok(false);
    };
    record_verified(paths, &value)?;
    Ok(true)
}

pub(super) fn backfill_after_sync(paths: &Paths) -> crate::Result<()> {
    if !registered_names(paths)?
        .iter()
        .any(|name| name == SOURCE_ENTRY)
    {
        return Ok(());
    }
    if let Some(index) = read(paths).ok().flatten()
        && file_stamp(paths).as_deref() == Some(index.file.as_str())
        && current_stamp(paths)? == Some(index.stamp)
    {
        return Ok(());
    }
    let _ = refresh_mailbox_component_index(paths)?;
    Ok(())
}

pub(super) fn record_verified(paths: &Paths, value: &VerifiedEntry) -> crate::Result<()> {
    let Some(stamp) = value.snapshot().version().mailbox_source_stamp() else {
        return Ok(());
    };
    write(paths, stamp, value.value())
}

pub(super) fn record_applied(
    paths: &Paths,
    config: &Config,
    state: &State,
    entries: &[Entry],
) -> crate::Result<()> {
    if config.signer.is_none()
        || state.membership != Stored::In
        || state.failed
        || !state.pending.is_empty()
        || !state.manifest.files.contains_key(SOURCE_ENTRY)
    {
        return Ok(());
    }
    let Some(value) = entries.iter().find(|entry| entry.name == SOURCE_ENTRY) else {
        return Ok(());
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(());
    };
    write(
        paths,
        entry::public_state_stamp(config, state, identity.recipient()),
        &value.value,
    )
}

/// Read one exact component after the existing holder/custodian protocol has
/// authorized disclosure. A changed source or ambiguous field provides nothing.
pub fn read_mailbox_component(
    paths: &Paths,
    component: Component,
) -> crate::Result<Option<SecretBytes>> {
    let Some(index) = read(paths)? else {
        return Ok(None);
    };
    if !index.components.contains(&component)
        || file_stamp(paths).as_deref() != Some(index.file.as_str())
        || current_stamp(paths)? != Some(index.stamp)
    {
        return Ok(None);
    }
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let Some(value) = read_verified_entry(paths, &config, SOURCE_ENTRY)? else {
        return Ok(None);
    };
    if value.snapshot().version().mailbox_source_stamp() != Some(index.stamp) {
        return Ok(None);
    }
    let Some(source) = value.value().as_str().and_then(InlineSource::parse) else {
        return Ok(None);
    };
    Ok(source.value(component).cloned())
}
