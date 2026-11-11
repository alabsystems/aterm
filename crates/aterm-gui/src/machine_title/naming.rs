// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication facts belong to an explicit panel read, never the periodic menu-title reader.

use aterm_agent::machine_name::{file::MachineName, publish};
use aterm_types::machine_name::{GeneratedNames, Number, OsSlot};
use std::path::Path;

#[derive(Debug)]
pub(crate) struct NamingStatus {
    fleet: Result<Option<String>, String>,
    available_fleets: Option<(
        aterm_agent::machine_name::InFleet,
        Option<Result<bool, String>>,
    )>,
    names: [Option<String>; 3],
    publication: Result<publish::Publication, String>,
    expected: Option<GeneratedNames>,
    authorization: Option<Result<bool, String>>,
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
        let mut status = Self {
            fleet,
            available_fleets: None,
            names,
            publication,
            expected: name
                .num
                .map(|number| GeneratedNames::new(number, &name.nick)),
            authorization: None,
        };
        if matches!(&status.publication, Ok(publish::Publication::Owed(owed))
            if owed.iter().any(|owed| matches!(owed, publish::Owed::SystemNames(values)
                if status.actionable_system(values))))
        {
            status.authorization = Some(system.authorization(state, config));
        }
        status
    }

    fn actionable_system(&self, values: &[(&'static str, Option<String>)]) -> bool {
        values.iter().any(|(field, value)| {
            !self.expected.as_ref().is_some_and(|names| {
                OsSlot::from_macos_preference(field).is_some_and(|slot| {
                    value
                        .as_deref()
                        .is_some_and(|value| names.collision(slot, value))
                })
            })
        })
    }

    /// Membership is available before the optional browse. An advert grants no membership.
    pub(crate) fn with_fleet_membership(
        mut self,
        membership: aterm_agent::machine_name::InFleet,
    ) -> Self {
        self.available_fleets = Some((membership, None));
        self
    }

    pub(crate) fn discovered(&mut self, available: Result<bool, String>) {
        if let Some((_, result)) = &mut self.available_fleets {
            *result = Some(available);
        }
    }

    /// Offer an explicit retry for pending or unread publication. Its execution
    /// rechecks the current policy and canonical number.
    pub(crate) fn retry(&self) -> bool {
        match &self.publication {
            Ok(publish::Publication::Owed(owed)) => owed.iter().any(|owed| match owed {
                publish::Owed::SystemNames(values) => self.actionable_system(values),
                _ => true,
            }),
            Err(_) => true,
            Ok(_) => false,
        }
    }

    /// Only an explicit panel press can request permission. An alias alone is not repair work.
    pub(crate) fn authorize(&self) -> bool {
        self.retry() && matches!(&self.authorization, Some(Ok(false) | Err(_)))
    }

    pub(crate) fn text(&self, number: &str) -> String {
        self.text_for(self.expected.as_ref(), number)
    }

    pub(crate) fn text_for(&self, expected: Option<&GeneratedNames>, number: &str) -> String {
        let fleet = match &self.fleet {
            Ok(Some(name)) => name.as_str(),
            Ok(None) => "unset",
            Err(_) => "unread",
        };
        let mac = [
            ("Computer", OsSlot::MacComputerName),
            ("Host", OsSlot::MacHostName),
            ("Bonjour", OsSlot::MacLocalHostName),
        ]
        .into_iter()
        .map(|(label, slot)| {
            let index = match slot {
                OsSlot::MacComputerName => 0,
                OsSlot::MacLocalHostName => 1,
                _ => 2,
            };
            let actual = self.names[index].as_deref();
            let wanted = expected.and_then(|names| names.for_slot(slot));
            let suffix = if slot == OsSlot::MacLocalHostName {
                ".local"
            } else {
                ""
            };
            let collision = expected
                .is_some_and(|names| actual.is_some_and(|actual| names.collision(slot, actual)));
            let show = |value: Option<&str>| {
                value.map_or_else(|| "unread".to_owned(), |value| format!("{value}{suffix}"))
            };
            format!(
                "{label}: expected {} · actual {}{}",
                wanted.map_or_else(
                    || "not assigned".to_owned(),
                    |wanted| format!("{wanted}{suffix}")
                ),
                show(actual),
                if collision { " (collision alias)" } else { "" },
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
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
        let authorization = if self.authorize() {
            "\nAuthorize & Sync updates naming permission, then reads this Mac's name again."
        } else {
            ""
        };
        let available = match &self.available_fleets {
            Some((aterm_agent::machine_name::InFleet::In, _)) => "\nFleet: ALab (joined)",
            Some((aterm_agent::machine_name::InFleet::Joining, _)) => "\nFleet: ALab (joining)",
            Some((_, Some(Ok(true)))) => "\nAvailable fleets: ALab",
            Some((_, Some(Ok(false)))) => "\nAvailable fleets: none discovered",
            Some((_, Some(Err(_)))) => "\nAvailable fleets: discovery unavailable",
            Some((_, None)) => "\nAvailable fleets: discovering\u{2026}",
            None => "",
        };
        format!(
            "ID: {number}{settings}   Fleet machine ID: {fleet}\n{mac}{note}{authorization}{available}"
        )
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
