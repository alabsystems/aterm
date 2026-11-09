// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication facts belong to an explicit panel read, never the periodic menu-title reader.

use aterm_agent::machine_name::{file::MachineName, publish};
use aterm_types::machine_name::Number;
use std::path::Path;

#[derive(Debug)]
pub(crate) struct NamingStatus {
    fleet: Result<Option<String>, String>,
    names: [Option<String>; 3],
    publication: Result<publish::Publication, String>,
}

impl NamingStatus {
    pub(crate) fn read(
        state: &Path,
        config: &Path,
        name: &MachineName,
        system: &dyn publish::SystemNames,
    ) -> Self {
        let names = system.read();
        let fleet = aterm_net::fleet_member::read_primary_name(state);
        let publication = (|| {
            let config =
                aterm_uds::atomic_file::ConfigFile::open(config).map_err(|e| e.to_string())?;
            let text = config.text().map_err(|e| e.to_string())?;
            let fleet = fleet.as_ref().map_err(Clone::clone)?;
            publish::publication_of(text, fleet.as_deref(), name, Some(&names))
        })();
        Self {
            fleet,
            names,
            publication,
        }
    }

    /// Offer an explicit retry for pending or unread publication. Its execution
    /// rechecks the current policy and canonical number.
    pub(crate) fn retry(&self) -> bool {
        match &self.publication {
            Ok(publish::Publication::Owed(owed)) => !owed.is_empty(),
            Err(_) => true,
            Ok(_) => false,
        }
    }

    pub(crate) fn text(&self, number: &str) -> String {
        let fleet = match &self.fleet {
            Ok(Some(name)) => name.as_str(),
            Ok(None) => "unset",
            Err(_) => "unread",
        };
        let mac = if self
            .names
            .iter()
            .all(|value| value.as_deref() == Some(number))
        {
            number.to_owned()
        } else {
            publish::SYSTEM_NAMES
                .iter()
                .zip(&self.names)
                .map(|(field, value)| format!("{field}: {}", value.as_deref().unwrap_or("unread")))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let note = match &self.publication {
            Ok(publish::Publication::Off(_)) => "\nName synchronization is held in settings.",
            Err(_) => "\nName synchronization could not be checked.",
            _ => "",
        };
        let settings = match &self.publication {
            Ok(publish::Publication::Owed(owed))
                if owed
                    .iter()
                    .any(|owed| matches!(owed, publish::Owed::Policy(_))) =>
            {
                " (settings pending)"
            }
            _ => "",
        };
        format!("aterm: {number}{settings}   Fleet: {fleet}\nmacOS: {mac}{note}")
    }
}

/// Retry publication without choosing a number again. The publisher's canonical-number fence
/// refuses a stale request before its policy or OS effects.
pub(super) fn synchronize(
    state: &Path,
    config: &Path,
    number: Number,
    system: &dyn publish::SystemNames,
) -> Result<(), publish::Pending> {
    let policy = publish::Policy::read(config, number).map_err(publish::Pending::Policy)?;
    publish::publish(state, config, &policy, Some(system))
}
