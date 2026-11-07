// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Move the authenticated shared release bundle into its own namespace without replacing a
//! member's personal legacy identity. Only the ALab admin's ordinary approver pass publishes.

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use zeroize::Zeroizing;

const LEGACY: [&str; 3] = crate::entry_class::LEGACY_RELEASE_RING;
const CANONICAL: [&str; 3] = crate::entry_class::RELEASE_RING;
const LEGACY_ROSTER: &str = crate::entry_class::LEGACY_ROSTER[0];
const CANONICAL_ROSTER: [&str; 2] = crate::entry_class::PUBLIC_ROSTER;
const RECHECK: Duration = Duration::from_secs(300);

#[derive(Default)]
pub(super) struct Rounds {
    checked: Option<Checked>,
    failed_at: Option<Instant>,
}

struct Checked {
    source: [u8; 32],
    seq: u64,
    floor: u64,
    at: Instant,
}

impl Rounds {
    /// True only after a publication; the caller immediately reads the new admin version.
    pub(super) fn step(
        &mut self,
        paths: &Paths,
        config: &Config,
        seq: u64,
        log: &dyn Fn(&str),
    ) -> bool {
        if self
            .failed_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
        {
            return false;
        }
        match self.try_step(paths, config, seq, log) {
            Ok(published) => {
                self.failed_at = None;
                published
            }
            Err(error) => {
                // Migration is independent of admitting members. A bad local floor or a
                // failed publication must not prevent the ordinary join queue being served.
                self.failed_at = Some(Instant::now());
                log(&format!("shared release namespace: {error}"));
                false
            }
        }
    }

    fn try_step(
        &mut self,
        paths: &Paths,
        config: &Config,
        seq: u64,
        log: &dyn Fn(&str),
    ) -> crate::Result<bool> {
        if config.name != crate::ALAB_VAULT
            || !alab_vault(config)
            || crate::service::subscriber_turned_off(&paths.home)
        {
            self.checked = None;
            return Ok(false);
        }
        let floor = roster_floor(paths)?;
        let source = source_identity(config);
        if self.checked.as_ref().is_some_and(|checked| {
            checked.source == source
                && checked.seq == seq
                && checked.floor == floor
                && checked.at.elapsed() < RECHECK
        }) {
            return Ok(false);
        }
        // Neither an unavailable clock nor a failed read/publish is memoized. A later normal
        // pass retries, including a lost publish response or another admin winning parent-CAS.
        let now = aterm_time::unix_secs()
            .and_then(|now| i64::try_from(now).ok())
            .ok_or_else(|| Error::new("current time is unavailable"))?;
        let admin = admin_open(paths)?;
        if admin.config != *config || admin.seq != seq {
            return Err(Error::new(
                "the vault changed while checking the release namespace",
            ));
        }
        match migrate(
            &admin.published_doc,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
            floor,
            now,
        ) {
            Migration::Ready(doc) => {
                if crate::service::subscriber_turned_off(&paths.home) {
                    return Ok(false);
                }
                // admin_publish retains the authenticated baseline and serializes its normal
                // parent-CAS publication; no state lock is held across network operations.
                admin_publish(paths, &admin, &doc)?;
                self.checked = None;
                log("published the existing shared release bundle in its canonical namespace");
                return Ok(true);
            }
            Migration::Unchanged => {}
            Migration::Refused(reason) => log(&format!("shared release namespace: {reason}")),
        }
        self.checked = Some(Checked {
            source,
            seq,
            floor,
            at: Instant::now(),
        });
        Ok(false)
    }
}

pub(super) fn roster_floor(paths: &Paths) -> crate::Result<u64> {
    match fsx::read_private(&paths.dir.join("release-ring-roster-floor"), 32)? {
        None => Ok(0),
        Some(bytes) => bytes
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| Error::new("release roster floor is invalid")),
    }
}

enum Migration {
    Unchanged,
    Refused(&'static str),
    Ready(Box<Document>),
}

fn migrate(doc: &Document, anchors: &[&str], floor: u64, now: i64) -> Migration {
    let targets: Vec<_> = doc
        .entries()
        .into_iter()
        .filter(|entry| CANONICAL_ROSTER.contains(&entry.name.as_str()))
        .collect();
    if !targets.is_empty()
        && (targets.len() != 2
            || CANONICAL_ROSTER.iter().enumerate().any(|(index, name)| {
                doc.entry(&Audience::Everyone, name).is_none_or(|entry| {
                    !entry.if_absent || entry.anchor.as_deref() != Some(CANONICAL_ROSTER[1 - index])
                })
            }))
    {
        return Migration::Unchanged;
    }
    let identity = match migrate_identity(doc, anchors, floor, now) {
        Migration::Ready(next) => Some(next),
        Migration::Unchanged => None,
        refused @ Migration::Refused(_) => return refused,
    };
    let current = identity.as_deref().unwrap_or(doc);
    match complete_roster(current, anchors, floor, now) {
        Migration::Unchanged => identity.map_or(Migration::Unchanged, Migration::Ready),
        result => result,
    }
}

fn complete_roster(doc: &Document, anchors: &[&str], floor: u64, now: i64) -> Migration {
    // A partial or host-specific signer is an explicit choice, not a shared bundle to repair.
    if doc
        .entries()
        .iter()
        .filter(|entry| CANONICAL.contains(&entry.name.as_str()))
        .count()
        != 3
    {
        return Migration::Unchanged;
    }
    let Some(bundle) = CANONICAL
        .iter()
        .map(|name| doc.entry(&Audience::Everyone, name))
        .collect::<Option<Vec<_>>>()
    else {
        return Migration::Unchanged;
    };
    if bundle.iter().enumerate().any(|(index, entry)| {
        !entry.if_absent || entry.anchor.as_deref() != Some(CANONICAL[(index + 1) % 3])
    }) {
        return Migration::Unchanged;
    }
    let Some(pair) = authorized_pair(doc, &bundle, anchors, floor, now) else {
        return Migration::Refused("shared release bundle has no current matching roster");
    };
    let values = [&pair.roster.value, &pair.signature.value];
    let mut next = doc.clone();
    if let Some(current) = doc.entry(&Audience::Everyone, CANONICAL_ROSTER[0]) {
        let Some(signature) = doc.entry(&Audience::Everyone, CANONICAL_ROSTER[1]) else {
            return Migration::Refused("canonical release roster pair is incomplete");
        };
        let old = aterm_update_core::roster::verify_roster(
            anchors,
            current.value.as_slice().to_vec(),
            signature.value.as_slice(),
        )
        .and_then(|verified| aterm_update_core::roster::Roster::parse(&verified));
        let Ok(old) = old else {
            return Migration::Refused("existing canonical roster is not master-signed");
        };
        if old.roster_seq > pair.seq {
            return Migration::Refused("canonical release roster would move backwards");
        }
        if current.value == *values[0] {
            return Migration::Unchanged;
        }
        if old.roster_seq == pair.seq {
            return Migration::Refused("different signed release rosters share one generation");
        }
        for (index, name) in CANONICAL_ROSTER.iter().enumerate() {
            if next
                .set_entry_with(
                    &Audience::Everyone,
                    name,
                    values[index].as_slice(),
                    true,
                    Some(CANONICAL_ROSTER[1 - index]),
                )
                .is_err()
            {
                return Migration::Refused("canonical roster conflicts with the current document");
            }
        }
        return Migration::Ready(Box::new(next));
    }
    let entries: Vec<_> = CANONICAL_ROSTER
        .iter()
        .enumerate()
        .map(|(index, name)| Entry {
            name: (*name).to_owned(),
            value: (*values[index]).clone(),
            audience: Audience::Everyone,
            if_absent: true,
            anchor: Some(CANONICAL_ROSTER[1 - index].to_owned()),
        })
        .collect();
    let mut bytes = Zeroizing::new(doc.render().as_slice().to_vec());
    bytes.push(b'\n');
    bytes.extend_from_slice(text::render_everyone_locker(&entries).as_slice());
    match std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| Document::parse(text).ok())
    {
        Some(next) => Migration::Ready(Box::new(next)),
        None => Migration::Refused("canonical roster conflicts with the current document"),
    }
}

fn migrate_identity(doc: &Document, anchors: &[&str], floor: u64, now: i64) -> Migration {
    let entries = doc.entries();
    // Even a host-specific or partially supplied canonical bundle is an explicit existing
    // choice. Never add competing Everyone entries or complete it with a different signer.
    if entries
        .iter()
        .any(|entry| CANONICAL.contains(&entry.name.as_str()))
    {
        return Migration::Unchanged;
    }
    let Some(bundle) = LEGACY
        .iter()
        .map(|name| doc.entry(&Audience::Everyone, name))
        .collect::<Option<Vec<_>>>()
    else {
        return Migration::Unchanged;
    };
    if bundle.iter().enumerate().any(|(index, entry)| {
        !entry.if_absent || entry.anchor.as_deref() != Some(LEGACY[(index + 1) % 3])
    }) {
        return Migration::Refused("legacy release entries are not one protected bundle");
    }
    if !valid_bundle(doc, &bundle, anchors, floor, now) {
        return Migration::Refused(
            "legacy release bundle has no current matching release authority",
        );
    }
    let copied: Vec<_> = bundle
        .iter()
        .enumerate()
        .map(|(index, entry)| Entry {
            name: CANONICAL[index].to_owned(),
            value: entry.value.clone(),
            audience: Audience::Everyone,
            if_absent: true,
            anchor: Some(CANONICAL[(index + 1) % 3].to_owned()),
        })
        .collect();
    // Parse all three together: each anchor must already exist in the same document, so
    // individual set_entry calls cannot safely construct the new cycle.
    let original = doc.render();
    let block = text::render_everyone_locker(&copied);
    let mut bytes = Zeroizing::new(original.as_slice().to_vec());
    bytes.push(b'\n');
    bytes.extend_from_slice(block.as_slice());
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Migration::Refused("release namespace could not be represented");
    };
    match Document::parse(text) {
        Ok(next) => Migration::Ready(Box::new(next)),
        Err(_) => Migration::Refused("release namespace conflicts with the current document"),
    }
}

fn valid_bundle(doc: &Document, bundle: &[&Entry], anchors: &[&str], floor: u64, now: i64) -> bool {
    authorized_pair(doc, bundle, anchors, floor, now).is_some()
}

struct RosterPair<'a> {
    roster: &'a Entry,
    signature: &'a Entry,
    seq: u64,
}

fn authorized_pair<'a>(
    doc: &'a Document,
    bundle: &[&Entry],
    anchors: &[&str],
    floor: u64,
    now: i64,
) -> Option<RosterPair<'a>> {
    let parse =
        |bytes: &SecretBytes| aterm_toml::from_str::<aterm_toml::Value>(bytes.as_str()?).ok();
    let profile = parse(&bundle[2].value)?;
    let machine = parse(&bundle[1].value)?;
    let encoded = field(&profile, "signing_key")?;
    let profile_key = aterm_codec::base64::decode_strict(encoded.as_bytes()).ok()?;
    let profile_key = Zeroizing::new(profile_key);
    if profile_key.as_slice() != bundle[0].value.as_slice() {
        return None;
    }
    let key = Ed25519KeyPair::from_pkcs8(bundle[0].value.as_slice()).ok()?;
    let id = field(&profile, "machine_id")?;
    if field(&machine, "id") != Some(id) {
        return None;
    }
    let encoded_public = field(&machine, "pubkey")?;
    let public = aterm_codec::base64::decode_strict(encoded_public.as_bytes()).ok()?;
    if public != key.public_key().as_ref() {
        return None;
    }
    let named = match profile.get("machine_roster") {
        Some(value) => value.as_str().filter(|s| !s.is_empty())?,
        None => LEGACY_ROSTER,
    };
    let mut candidates = Vec::new();
    for name in [named, CANONICAL_ROSTER[0], LEGACY_ROSTER] {
        let Some(roster) = doc.entry(&Audience::Everyone, name) else {
            continue;
        };
        let signature = doc.entry(&Audience::Everyone, &format!("{name}.sig"))?;
        candidates.push((
            (roster, signature),
            roster.value.as_slice(),
            signature.value.as_slice(),
        ));
    }
    let ((roster, signature), seq) = crate::release_ring::newest_roster(anchors, candidates)?;
    let chosen = RosterPair {
        roster,
        signature,
        seq,
    };
    // A newer signed revocation or expiry must not fall back to an older authorizing roster.
    crate::release_ring::authorize_with(
        bundle[2].value.as_slice(),
        bundle[0].value.as_slice(),
        chosen.roster.value.as_slice().to_vec(),
        chosen.signature.value.as_slice(),
        anchors,
        floor,
        now,
    )
    .ok()?;
    Some(chosen)
}

fn field<'a>(value: &'a aterm_toml::Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(aterm_toml::Value::as_str)
}

#[cfg(test)]
mod tests;
