// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Init and System State share these independently read requirements.
use super::{Access, Observation, Standing, Subject};

pub(super) const fn index(access: Access) -> Option<usize> {
    match access {
        Access::LocalNetwork => Some(0),
        Access::NameRule => Some(1),
        Access::SpotlightScope => Some(2),
        Access::NetworkAuth => Some(3),
        Access::NetworkHelper => Some(4),
        _ => None,
    }
}

fn row(access: Access, result: Result<(Standing, String, Option<String>), String>) -> Observation {
    let mut row = Observation::unknown(Subject::Access(access));
    match result {
        Ok((standing, now, note)) => {
            row.standing = standing;
            row.now = Some(now);
            row.note = note;
        }
        Err(why) => row.note = Some(why),
    }
    row
}

pub(super) fn read() -> [Option<Observation>; 5] {
    #[cfg(target_os = "macos")]
    let local_network = local_network(aterm_lan::grant::authorization_here());
    #[cfg(not(target_os = "macos"))]
    let local_network = local_network(Ok(None));
    #[cfg(target_os = "macos")]
    let network_helper = network_helper(aterm_uds::lan::broker::installed_hello(), || {
        aterm_uds::lan::install::owed_here(
            || aterm_secrets::service::system_job_running(aterm_uds::lan::LABEL),
            || aterm_uds::launchd::system_job_switched_off(aterm_uds::lan::LABEL),
        )
    });
    #[cfg(not(target_os = "macos"))]
    let network_helper = row(
        Access::NetworkHelper,
        Ok((Standing::InPlace, "No helper required".into(), None)),
    );
    [
        Some(local_network),
        Some(row(Access::NameRule, read_one(Access::NameRule))),
        Some(row(
            Access::SpotlightScope,
            read_one(Access::SpotlightScope),
        )),
        Some(row(Access::NetworkAuth, read_one(Access::NetworkAuth))),
        Some(network_helper),
    ]
}

/// App-level consent is diagnostic: all of aterm's own LAN traffic requires the separately
/// confirmed root helper, with no fallback that asks macOS for this app's permission.
pub(super) fn local_network(permission: Result<Option<bool>, String>) -> Observation {
    let (now, error) = match permission {
        Ok(Some(true)) => ("Allowed", None),
        Ok(Some(false)) => ("Denied", None),
        Ok(None) => ("Not answered", None),
        Err(why) => ("Not read", Some(why)),
    };
    let mut seen = Observation::unknown(Subject::Access(Access::LocalNetwork));
    seen.standing = Standing::NotNeeded;
    seen.now = Some(now.into());
    let mut detail = format!(
        "Not required for aterm's own LAN traffic; the network helper is checked separately. macOS app permission: {now}."
    );
    if let Some(error) = error {
        detail.push(' ');
        detail.push_str(&error);
    }
    seen.note = Some(detail.clone());
    seen.detail = Some(detail);
    seen
}

/// One current installed-root hello owns the helper requirement. A missing or refused proof
/// remains incomplete; app-level consent never supplies root custody.
#[cfg(target_os = "macos")]
pub(super) fn network_helper(
    hello: Result<aterm_uds::lan::BrokerHello, aterm_uds::lan::LanError>,
    repair: impl FnOnce() -> bool,
) -> Observation {
    let result = match hello {
        Ok(hello) if hello.direct.is_none() => {
            Ok((Standing::InPlace, "Installed and authorized".into(), None))
        }
        Ok(_) => Err("The installed helper did not establish root custody".into()),
        Err(_) if repair() => Ok((
            Standing::NeedsYou,
            "Setup incomplete".into(),
            Some(
                "Your Mac password is required to finish the local-network helper's setup.".into(),
            ),
        )),
        Err(why) => Err(format!("Network helper not confirmed: {why}")),
    };
    row(Access::NetworkHelper, result)
}

fn read_one(access: Access) -> Result<(Standing, String, Option<String>), String> {
    use aterm_machines::root_steps::{self, NameRuleReading};
    use aterm_types::macos_setup::spotlight_scope::{self, State};
    let ready = |word: &str| Ok((Standing::InPlace, word.to_owned(), None));
    let needs = |word: &str, why: String| Ok((Standing::NeedsYou, word.to_owned(), Some(why)));
    match access {
        Access::NameRule => {
            if !cfg!(target_os = "macos") {
                return Ok((Standing::NotNeeded, "macOS only".into(), None));
            }
            let sys = aterm_machines::port::Live::new();
            match root_steps::name_rule_reading(&sys)? {
                NameRuleReading::Current => ready("Installed"),
                _ => needs("Setup incomplete", "Your Mac password is required to install the permission that lets aterm set this Mac's network name.".into()),
            }
        }
        Access::SpotlightScope => {
            if !cfg!(target_os = "macos") {
                return Ok((Standing::NotNeeded, "macOS only".into(), None));
            }
            use spotlight_scope::Reads as _;
            let live = spotlight_scope::Live;
            let home = live
                .home()
                .ok_or("The account's home folder could not be read")?;
            match spotlight_scope::state(&live, &home)? {
                State::Disabled => ready("Indexing disabled"),
                State::Scoped(_) => ready("Excluded"),
                State::Indexed(paths) => needs(
                    "Folders still indexed",
                    format!(
                        "Your Mac password is required to exclude: {}. Documents, Desktop, Library and Applications stay searchable. Completed exclusions are not requested again.",
                        paths
                            .iter()
                            .map(|p| p.file_name().unwrap_or_default().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
            }
        }
        Access::NetworkAuth => {
            use aterm_secrets::client::{self, Membership, Paths};
            let paths = Paths::live().map_err(|e| e.to_string())?;
            let status = client::status(&paths).map_err(|e| e.to_string())?;
            match &status.membership {
                Membership::In { .. } => {
                    let config = status.config.as_ref().ok_or("No configured authorization source")?;
                    if !client::alab_vault(config) || !client::pin_admitted(config, None) {
                        return Err("The configured vault is not an authenticated ALab source".into());
                    }
                    let recipient = status.machine.as_ref().map(|(recipient, _)| *recipient)
                        .ok_or("This Mac has no local network identity")?;
                    let records = client::load_acknowledged_records(&paths).map_err(|e| e.to_string())?;
                    let log = records.log().ok_or("No acknowledged network authorization is available")?;
                    let membership = log.membership_ref();
                    if !membership.recipient_removed(&recipient)
                        && membership.members().values().any(|member| member.binding.recipient() == recipient && !membership.member_node_removed(member)) {
                        ready("Authorized")
                    } else {
                        needs("Not authorized", "The acknowledged network membership does not admit this Mac. Join again in Secrets & identity.".into())
                    }
                },
                Membership::Plain => needs("No ALab authorization", "A local credentials file does not establish ALab membership. Join the network in Secrets & identity.".into()),
                Membership::Pending => needs("Awaiting authorization", status.approve_command().map_or_else(
                    || "Join the network in Secrets & identity.".into(),
                    |command| format!("Approve this Mac on an authorized Mac: {command}"))),
                Membership::Revoked { .. } => needs("Authorization revoked", "This Mac must be admitted again in Secrets & identity.".into()),
                Membership::Unconfigured => needs("Not joined", "Join the ALab network in Secrets & identity.".into()),
            }
        }
        _ => Err("Not an additional setup requirement".into()),
    }
}
