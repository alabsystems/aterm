// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-owned executor for reserved asynchronous editor storage.

use amacs::store::{
    PreparationReservation, RetirementExecutor, RetirementService, WeakRetirementService,
};
use std::io;
use std::sync::Mutex;

struct Host {
    service: WeakRetirementService,
    worker: std::thread::JoinHandle<()>,
}
static HOST: Mutex<Option<Host>> = Mutex::new(None);

pub(crate) fn reservation() -> io::Result<PreparationReservation> {
    reservation_on(&HOST)
}

fn reservation_on(registry: &Mutex<Option<Host>>) -> io::Result<PreparationReservation> {
    let service = {
        let mut owner = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(service) = owner.as_ref().and_then(|host| host.service.upgrade()) {
            service
        } else {
            if let Some(previous) = owner.take() {
                previous
                    .worker
                    .join()
                    .map_err(|_| io::Error::other("retirement executor failed"))?;
            }
            let (service, driver) =
                RetirementService::channel(aterm_cores::available_parallelism()?);
            // Publish only after successful startup. Every reserved node retains
            // producer custody, including canceled worker results and histories
            // that outlive their App. Last producer closes the queue; the unique
            // drainer releases every queued value before exiting.
            let worker = std::thread::Builder::new()
                .name("amacs-prepared-retire".into())
                .spawn(move || {
                    #[cfg(not(debug_assertions))]
                    let executor = RetirementExecutor::default();
                    #[cfg(debug_assertions)]
                    let executor = {
                        use std::hash::{Hash, Hasher};
                        let mut hash = std::collections::hash_map::DefaultHasher::new();
                        std::thread::current().id().hash(&mut hash);
                        RetirementExecutor {
                            identity: hash.finish(),
                        }
                    };
                    driver.run(executor);
                })?;
            *owner = Some(Host {
                service: service.downgrade(),
                worker,
            });
            service
        }
    };
    // Never hold the host registry lock during closed-drain admission.
    Ok(service.reserve())
}

#[cfg(test)]
#[path = "retirement_tests.rs"]
mod tests;
