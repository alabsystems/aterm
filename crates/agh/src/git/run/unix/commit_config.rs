// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Config admission for the immutable commit reader, never Git transport.
//!
//! A normal blob-filtered clone names paired promisor/filter metadata. The
//! reader's isolated object view adopts neither configuration nor refs, and
//! every Reader child disables lazy fetching. Permit only that canonical pair;
//! all other keys retain the ordinary read-only transport screen. Values are
//! examined inside the child projection and never enter its recorded answer.

use std::collections::BTreeMap;

use super::{ConfigClass, ConfigVerdict, SCREEN_MAX, project_config_names};

#[derive(Default)]
struct Filter {
    promisor: bool,
    blobs: bool,
}

enum Marker {
    Promisor,
    Blobs,
}

fn marker(key: &str) -> Option<(&str, Marker)> {
    let (section, rest) = key.split_once('.')?;
    let (name, variable) = rest.rsplit_once('.')?;
    if !section.eq_ignore_ascii_case("remote") || name.is_empty() || name.contains([':', '/']) {
        return None;
    }
    if variable.eq_ignore_ascii_case("promisor") {
        Some((name, Marker::Promisor))
    } else if variable.eq_ignore_ascii_case("partialclonefilter") {
        Some((name, Marker::Blobs))
    } else {
        None
    }
}

fn refused(class: ConfigClass) -> Vec<u8> {
    aterm_json::to_vec(&ConfigVerdict::Refused { class }).unwrap_or_default()
}

pub(super) fn project(listed: &[u8]) -> Vec<u8> {
    if listed.len() > SCREEN_MAX {
        return aterm_json::to_vec(&ConfigVerdict::TooLong {
            bytes: listed.len(),
        })
        .unwrap_or_default();
    }
    if listed.is_empty() {
        return project_config_names(listed, true);
    }
    let Some(entries) = listed.strip_suffix(&[0]) else {
        return refused(ConfigClass::Unknown);
    };
    let mut filters = BTreeMap::<&str, Filter>::new();
    let mut names = Vec::new();
    let mut keys = 0;
    for entry in entries.split(|byte| *byte == 0) {
        if entry.is_empty() {
            return refused(ConfigClass::Unknown);
        }
        keys += 1;
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&entry[..at], Some(&entry[at + 1..])),
            None => (entry, None),
        };
        if let Some((name, kind)) = std::str::from_utf8(key).ok().and_then(marker) {
            let filter = filters.entry(name).or_default();
            let accepted = match kind {
                Marker::Promisor => {
                    let accepted = !filter.promisor
                        && value.is_some_and(|value| value.eq_ignore_ascii_case(b"true"));
                    filter.promisor = true;
                    accepted
                }
                Marker::Blobs => {
                    let accepted = !filter.blobs && value == Some(b"blob:none".as_slice());
                    filter.blobs = true;
                    accepted
                }
            };
            if !accepted {
                return refused(ConfigClass::Remote);
            }
        } else {
            names.extend_from_slice(key);
            names.push(0);
        }
    }
    if filters
        .values()
        .any(|filter| !filter.promisor || !filter.blobs)
    {
        return refused(ConfigClass::Remote);
    }
    let ordinary = project_config_names(&names, true);
    match aterm_json::from_slice::<ConfigVerdict>(&ordinary) {
        Ok(ConfigVerdict::Allowed { .. }) => {
            aterm_json::to_vec(&ConfigVerdict::Allowed { keys }).unwrap_or_default()
        }
        _ => ordinary,
    }
}

#[cfg(test)]
#[path = "commit_config_tests.rs"]
mod tests;
