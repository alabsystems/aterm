// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Current admin upkeep; provider paging belongs to its owned fetch-only worker.

use aterm_db::accounts::recovery::Queue;
use aterm_secrets::client::{self, Admin, Paths};
use std::time::Instant;

#[derive(Default)]
struct Upkeep {
    queue: Queue,
    turn: u8,
    catalog: super::repository_catalog::Upkeep,
}

impl client::AccountUpkeep for Upkeep {
    fn step(
        &mut self,
        paths: &Paths,
        admin: &Admin,
        deadline: Instant,
    ) -> aterm_secrets::Result<usize> {
        // Every application gets the first slice in turn. Provider paging is never joined
        // here; only owned Drop waits for the bounded worker.
        let mut done = 0;
        let mut error = None;
        for offset in 0..3 {
            let result = match (self.turn + offset) % 3 {
                0 => self
                    .queue
                    .step(paths, admin, deadline)
                    .map_err(|e| e.to_string()),
                1 => super::native::root_upkeep(paths, admin, deadline),
                _ => self.catalog.step(paths, admin, deadline),
            };
            match result {
                Ok(count) => done += count,
                Err(why) => {
                    error.get_or_insert(why);
                }
            }
        }
        self.turn = (self.turn + 1) % 3;
        error.map_or(Ok(done), |why| Err(aterm_secrets::Error::new(why)))
    }
    fn pause(&mut self) {
        self.catalog.pause();
    }
}

pub(super) fn install() {
    client::on_account_upkeep(|| Box::new(Upkeep::default()));
}

#[cfg(test)]
pub(super) fn with_catalog(
    catalog: super::repository_catalog::Upkeep,
) -> Box<dyn client::AccountUpkeep> {
    Box::new(Upkeep {
        catalog,
        ..Upkeep::default()
    })
}
