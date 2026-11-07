// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Network recovery inside the original authorized setup process.

use super::{
    CommandLineTools, Failure, Placeholder, Unfinished, installed, installers_listed, low_data,
    read_stamp, say,
};
use crate::port::{InstallNetworkWatch, System};
use std::time::Duration;

const TICK: Duration = Duration::from_secs(2);
const DELAYS: [u64; 6] = [5, 15, 30, 60, 120, 300];

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallerState {
    Ready,
    CommandLine,
    Window,
}

/// A live root process and its original placeholder, never an authorization saved on disk.
pub(super) struct Custody<'a> {
    placeholder: Placeholder<'a>,
    network: Option<Box<dyn InstallNetworkWatch + 'a>>,
}

impl Custody<'_> {
    fn check(&self) -> Result<InstallerState, Failure> {
        let sys = self.placeholder.sys;
        if let Some(signal) = sys.stopped() {
            return Err(Failure::new(
                Unfinished::Stopped,
                format!("the authorized install was stopped by signal {signal}"),
            ));
        }
        let observed = read_stamp(sys).map_err(|error| {
            Failure::new(
                if error.kind() == std::io::ErrorKind::NotFound {
                    Unfinished::Busy
                } else {
                    Unfinished::Unread
                },
                format!("the installer placeholder could not be verified: {error}"),
            )
        })?;
        if observed != self.placeholder.stamp {
            return Err(Failure::new(
                Unfinished::Busy,
                "the installer no longer owns its original placeholder".into(),
            ));
        }
        if !aterm_types::macos_setup::placeholder_held_by_a_live_run(&observed, |pid| {
            sys.process_started(pid)
        }) {
            return Err(Failure::new(
                Unfinished::Unread,
                "the installer's process generation could not be verified".into(),
            ));
        }
        match installers_listed(sys) {
            None => Err(Failure::new(
                Unfinished::Unread,
                "the current installer owners could not be read".into(),
            )),
            Some(owners)
                if owners.contains(&aterm_types::macos_setup::ToolsInstaller::CommandLine) =>
            {
                Ok(InstallerState::CommandLine)
            }
            Some(owners)
                if owners.contains(&aterm_types::macos_setup::ToolsInstaller::AppleWindow) =>
            {
                Ok(InstallerState::Window)
            }
            Some(_) => Ok(InstallerState::Ready),
        }
    }

    fn constrained(&self) -> bool {
        self.network
            .as_ref()
            .and_then(|watch| watch.reading())
            .map_or_else(
                || low_data(self.placeholder.sys).is_some(),
                |path| path.constrained,
            )
    }

    /// Recheck custody and policy before catalog access and again before downloading.
    pub(super) fn ready(&self) -> Result<(), Failure> {
        match self.check()? {
            InstallerState::Ready => {}
            InstallerState::CommandLine => {
                return Err(Failure::new(
                    Unfinished::Busy,
                    "another command-line installer is running".into(),
                ));
            }
            InstallerState::Window => {
                return Err(Failure::new(
                    Unfinished::AppleWindow,
                    "Apple's installer window is open".into(),
                ));
            }
        }
        if self.constrained() {
            return Err(Failure::new(
                Unfinished::LowData,
                "Low Data Mode holds the download in the existing authorized run".into(),
            ));
        }
        Ok(())
    }

    fn wait(&mut self, mut remaining: Duration) -> Result<(), Failure> {
        let initial = self.network.as_ref().and_then(|watch| watch.reading());
        let mut noted_installer = false;
        loop {
            let sys = self.placeholder.sys;
            if let Some(signal) = sys.stopped() {
                return Err(Failure::new(
                    Unfinished::Stopped,
                    format!("the authorized install was stopped by signal {signal}"),
                ));
            }
            if installed(sys).map_err(|why| Failure::new(Unfinished::Unread, why))? {
                return Ok(());
            }
            let installer = self.check()?;
            if installer == InstallerState::Window {
                return Err(Failure::new(
                    Unfinished::AppleWindow,
                    "Apple's installer window is open".into(),
                ));
            }
            if installer == InstallerState::CommandLine && !noted_installer {
                say("waiting for the existing installer in this setup run");
                noted_installer = true;
            }
            let path = self.network.as_ref().and_then(|watch| watch.reading());
            let changed = path.is_some_and(|path| {
                Some(path.revision) != initial.map(|initial| initial.revision)
                    && path.available
                    && !path.constrained
            });
            if installer == InstallerState::Ready
                && !self.constrained()
                && (remaining.is_zero() || changed)
            {
                return Ok(());
            }
            // Timer fallback also covers an absent monitor or a missed notification.
            // Continue checking Stop and custody while Low Data Mode holds the download.
            let tick = if remaining.is_zero() {
                TICK
            } else {
                remaining.min(TICK)
            };
            let spent = match self.network.as_mut() {
                Some(watch) => watch.wait(path.map(|path| path.revision), tick),
                None => {
                    sys.sleep(tick);
                    tick
                }
            };
            remaining = remaining.saturating_sub(spent);
        }
    }
}

pub(super) fn install(sys: &dyn System) -> Result<(), Failure> {
    if sys.euid() != 0 {
        return Err(Failure::new(
            Unfinished::Other,
            "retained install custody requires the original root process".into(),
        ));
    }
    let _stops = sys.defer_stops();
    let mut network = sys.install_network();
    let mut noted_installer = false;
    let placeholder = loop {
        if let Some(signal) = sys.stopped() {
            return Err(Failure::new(
                Unfinished::Stopped,
                format!("the authorized install was stopped by signal {signal}"),
            ));
        }
        if installed(sys).map_err(|why| Failure::new(Unfinished::Unread, why))? {
            return Ok(());
        }
        match CommandLineTools::hold_placeholder(sys) {
            Ok(Some(placeholder)) => break placeholder,
            Ok(None) => return CommandLineTools::through_open_window(sys),
            Err(failure) if failure.class == Unfinished::Busy => {
                if !noted_installer {
                    say("waiting for the existing installer before claiming the tools");
                    noted_installer = true;
                }
                let revision = network
                    .as_ref()
                    .and_then(|watch| watch.reading())
                    .map(|path| path.revision);
                if let Some(watch) = network.as_mut() {
                    let _spent = watch.wait(revision, TICK);
                } else {
                    sys.sleep(TICK);
                }
            }
            Err(failure) => return Err(failure),
        }
    };
    let mut custody = Custody {
        placeholder,
        network,
    };
    let mut attempt = 0usize;
    loop {
        if sys.stopped().is_none()
            && installed(sys).map_err(|why| Failure::new(Unfinished::Unread, why))?
        {
            return Ok(());
        }
        let result = CommandLineTools::install_catalog(sys, Some(&custody));
        let held_by_installer = match result {
            Err(failure) if failure.class == Unfinished::Busy => true,
            Err(failure)
                if matches!(
                    failure.class,
                    Unfinished::Catalog
                        | Unfinished::Download
                        | Unfinished::NoneOffered
                        | Unfinished::LowData
                ) =>
            {
                false
            }
            Err(failure) if failure.class == Unfinished::AppleWindow => {
                return CommandLineTools::through_open_window(sys);
            }
            result => return result,
        };
        let seconds = if held_by_installer {
            0
        } else {
            DELAYS[attempt.min(DELAYS.len() - 1)]
        };
        if !held_by_installer {
            attempt = attempt.saturating_add(1);
            say(&format!(
                "waiting for the network or Apple's catalog; retry in at most {seconds} seconds \
             after Low Data Mode clears"
            ));
        }
        match custody.wait(Duration::from_secs(seconds)) {
            Err(failure) if failure.class == Unfinished::AppleWindow => {
                return CommandLineTools::through_open_window(sys);
            }
            result => result?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::Fake;

    #[test]
    fn a_replacement_owner_cannot_witness_an_unverifiable_original_generation() {
        let sys = Fake::root();
        let own = std::process::id();
        let other = own.saturating_add(1);
        sys.born(own, 123).born(other, 456).running(&[]);
        let placeholder = CommandLineTools::hold_placeholder(&sys).unwrap().unwrap();
        let custody = Custody {
            placeholder,
            network: None,
        };
        sys.on_owned_read(move |sys, path| {
            if path == std::path::Path::new(super::super::PLACEHOLDER) {
                sys.file(
                    path.to_str().unwrap(),
                    aterm_types::macos_setup::placeholder_stamp(other, 456).as_bytes(),
                );
                // The original generation is no longer verifiable. The replacement is live.
                sys.born(own, 0);
            }
        });
        assert!(
            matches!(
                custody.check(),
                Err(Failure {
                    class: Unfinished::Unread,
                    ..
                })
            ),
            "a replacement owner's liveness must not vouch for the original custodian"
        );
        drop(custody);
        assert_eq!(
            sys.written(super::super::PLACEHOLDER).unwrap(),
            aterm_types::macos_setup::placeholder_stamp(other, 456).into_bytes()
        );
    }
}
