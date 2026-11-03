// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A new row must survive the observer restart and reach the returning peer.

use super::{Cycle, save};
use crate::Fail;
use astream_wire::sync::Digest;
use aterm_json::json;
use aterm_secrets::client::Paths;

impl Cycle<'_> {
    pub(super) fn write_while_offline(&mut self) -> Result<Digest, Fail> {
        let recipient = Paths::for_home(&self.recipient.home);
        let source = Paths::for_home(&self.source.home);
        let (operation, value) = self.application.write(&recipient, "peer-offline")?;
        let (source_rows, known) = self
            .application
            .observe(&source, &self.source.cfg.join("aterm/aterm.toml"))?;
        let original = self
            .rows
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if known.contains(&operation) || source_rows != *self.rows || known != original {
            return Err(Fail::die(
                "stopped source does not retain exactly the pre-outage table inventory",
            ));
        }
        self.rows.insert(operation, value);
        self.application.received(
            &recipient,
            &self.recipient.cfg.join("aterm/aterm.toml"),
            self.rows,
            self.out,
            "peer-cycle-local-write.json",
        )?;
        save(
            self.out,
            "peer-cycle-source-before-return.json",
            &json!({
                "table":self.application.id(),
                "offline_operation":aterm_codec::hex::encode_lower(&operation),
                "source_known":known.len(),"source_has_offline_operation":false,
                "recipient_expected":self.rows.len(),
                "scope":"Read-only source inventory after the recipient write, while the source process remains stopped",
            }),
        )?;
        Ok(operation)
    }

    pub(super) fn require_offline_row_after_restart(&self) -> Result<(), Fail> {
        self.application.received(
            &Paths::for_home(&self.recipient.home),
            &self.recipient.cfg.join("aterm/aterm.toml"),
            self.rows,
            self.out,
            "peer-cycle-local-after-restart.json",
        )
    }

    pub(super) fn require_returned_peer_caught_up(&self) -> Result<(), Fail> {
        for (world, name) in [
            (self.source, "peer-cycle-source-caught-up.json"),
            (self.recipient, "peer-cycle-recipient-final.json"),
        ] {
            self.application.received(
                &Paths::for_home(&world.home),
                &world.cfg.join("aterm/aterm.toml"),
                self.rows,
                self.out,
                name,
            )?;
        }
        Ok(())
    }
}
