// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Activate accepted keyboard defaults when a desktop gets a new HID service.

use aterm_secrets::converge::{Last, RunnerRetry};

#[derive(Default)]
struct Keeper {
    settled: Option<(u64, Vec<u64>)>,
    attempted: Option<(u64, Vec<u64>)>,
    retry: RunnerRetry,
}

impl Keeper {
    fn round(
        &mut self,
        now_ms: u64,
        revision: Option<u64>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<bool>, String>,
    ) -> Result<bool, String> {
        let Some(revision) = revision else {
            *self = Self::default();
            return Ok(false);
        };
        let mut services = services()?;
        services.sort_unstable();
        if services.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate keyboard service identity".into());
        }
        let current = (revision, services);
        let new: Vec<_> = current
            .1
            .iter()
            .copied()
            .filter(|id| {
                self.settled
                    .as_ref()
                    .is_none_or(|(before, ids)| *before != revision || !ids.contains(id))
            })
            .collect();
        if new.is_empty() {
            self.settled = Some(current);
            return Ok(false);
        }
        if self.attempted.as_ref() != Some(&current) {
            self.retry = RunnerRetry::default();
            self.attempted = Some(current.clone());
        } else if self
            .retry
            .remaining(now_ms)
            .is_some_and(|pause| !pause.is_zero())
        {
            return Ok(false);
        }
        self.retry.started();
        let result = activate(revision, &new);
        self.retry.reaped(
            match &result {
                Ok(Some(true)) => Last::Ended,
                Ok(None) => Last::Held,
                Ok(Some(false)) => Last::Unfinished,
                Err(_) => Last::Failed,
            },
            now_ms,
        );
        if matches!(result, Ok(Some(true))) {
            self.settled = Some(current);
            return Ok(true);
        }
        result.map(|_| false)
    }
}

pub(super) fn spawn() {
    if aterm_types::dirs::runs_under_private_state_root() || std::env::var_os("CI").is_some() {
        return;
    }
    let worker = || {
        use aterm_machines::port::System as _;
        let _ = aterm_log::wait_for_logger(super::EARLY_LOG_WAIT);
        let clock = std::time::Instant::now();
        let sys = aterm_machines::port::Live::new();
        let mut keeper = Keeper::default();
        let mut said = None;
        loop {
            let result =
                aterm_cli::macos_doctor::input_activation_revision().and_then(|revision| {
                    keeper.round(
                        u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
                        revision,
                        || sys.keyboard_service_ids(),
                        |revision, ids| {
                            aterm_cli::macos_doctor::activate_input_attachments(
                                revision,
                                ids,
                                super::setup_stop_requested,
                            )
                        },
                    )
                });
            match result {
                Ok(true) => {
                    super::request_observation(
                        None,
                        vec![aterm_types::macos_setup::Subject::Setting(
                            aterm_types::macos_setup::Setting::CapsLock,
                        )],
                        false,
                    );
                    said = None;
                }
                Ok(false) => {}
                Err(why) if said.as_ref() != Some(&why) => {
                    aterm_log::info!("keyboard attachment activation waits: {why}");
                    said = Some(why);
                }
                Err(_) => {}
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    };
    if let Err(why) = std::thread::Builder::new()
        .name("aterm-input-attachments".into())
        .spawn(worker)
    {
        super::warn_when_logging(format!(
            "keyboard attachment activation could not start: {why}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_and_reattach_activate_only_new_services_and_off_reads_no_hardware() {
        let mut keeper = Keeper::default();
        for (ids, expected) in [
            (vec![1], vec![1]),
            (vec![1, 2], vec![2]),
            (vec![1, 3], vec![3]),
        ] {
            assert!(
                keeper
                    .round(
                        0,
                        Some(7),
                        || Ok(ids),
                        |revision, new| {
                            assert_eq!(revision, 7);
                            assert_eq!(new, expected);
                            Ok(Some(true))
                        }
                    )
                    .unwrap()
            );
        }
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| panic!("removal only"))
                .unwrap()
        );
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| panic!("unchanged"))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    0,
                    None,
                    || panic!("Off reads no HID state"),
                    |_, _| panic!("Off")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    0,
                    Some(8),
                    || Ok(vec![1]),
                    |revision, ids| {
                        assert_eq!((revision, ids), (8, &[1][..]));
                        Ok(Some(true))
                    }
                )
                .unwrap()
        );
    }

    #[test]
    fn failed_or_held_attachments_retry_without_a_hot_loop_and_new_devices_wake_now() {
        let mut keeper = Keeper::default();
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| Ok(None))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    4999,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("lock retry paced")
                )
                .unwrap()
        );
        assert!(
            !keeper
                .round(5000, Some(7), || Ok(vec![1]), |_, _| Ok(Some(false)))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    9999,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("unfinished retry paced")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(10000, Some(7), || Ok(vec![1]), |_, _| Ok(Some(true)))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    10001,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("success stops")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10001,
                    Some(7),
                    || Ok(vec![1, 2]),
                    |_, ids| {
                        assert_eq!(ids, [2]);
                        Ok(Some(true))
                    }
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10002,
                    Some(7),
                    || Ok(vec![1, 2, 2]),
                    |_, _| panic!("duplicate identity")
                )
                .is_err()
        );
    }
}
