// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Move the authenticated shared release bundle into its own namespace without replacing a
//! member's personal legacy identity. Only the ALab admin's ordinary approver pass publishes.

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use zeroize::Zeroizing;

const LEGACY: [&str; 3] = [
    "~/.aterm/machine.key",
    "~/.aterm/machine.toml",
    "~/.aterm/release-credentials.toml",
];
const CANONICAL: [&str; 3] = [
    "~/.aterm/release-ring/machine.key",
    "~/.aterm/release-ring/machine.toml",
    "~/.aterm/release-ring/release-credentials.toml",
];
const LEGACY_ROSTER: &str = "~/.aterm/roster/aterm-machines.toml";
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

fn roster_floor(paths: &Paths) -> crate::Result<u64> {
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
    let parse =
        |bytes: &SecretBytes| aterm_toml::from_str::<aterm_toml::Value>(bytes.as_str()?).ok();
    let Some(profile) = parse(&bundle[2].value) else {
        return false;
    };
    let Some(machine) = parse(&bundle[1].value) else {
        return false;
    };
    let Some(encoded) = field(&profile, "signing_key") else {
        return false;
    };
    let Ok(profile_key) = aterm_codec::base64::decode_strict(encoded.as_bytes()) else {
        return false;
    };
    let profile_key = Zeroizing::new(profile_key);
    if profile_key.as_slice() != bundle[0].value.as_slice() {
        return false;
    }
    let Ok(key) = Ed25519KeyPair::from_pkcs8(bundle[0].value.as_slice()) else {
        return false;
    };
    let Some(id) = field(&profile, "machine_id") else {
        return false;
    };
    if field(&machine, "id") != Some(id) {
        return false;
    }
    let Some(encoded_public) = field(&machine, "pubkey") else {
        return false;
    };
    let Ok(public) = aterm_codec::base64::decode_strict(encoded_public.as_bytes()) else {
        return false;
    };
    if public != key.public_key().as_ref() {
        return false;
    }
    let roster_name = field(&profile, "machine_roster").unwrap_or(LEGACY_ROSTER);
    let Some(roster) = doc.entry(&Audience::Everyone, roster_name) else {
        return false;
    };
    let Some(signature) = doc.entry(&Audience::Everyone, &format!("{roster_name}.sig")) else {
        return false;
    };
    crate::release_ring::authorize_with(
        bundle[2].value.as_slice(),
        bundle[0].value.as_slice(),
        roster.value.as_slice().to_vec(),
        signature.value.as_slice(),
        anchors,
        floor,
        now,
    )
    .is_ok()
}

fn field<'a>(value: &'a aterm_toml::Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(aterm_toml::Value::as_str)
}

#[cfg(test)]
mod tests;
