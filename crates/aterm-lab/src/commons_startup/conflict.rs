// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Concurrent offline values must survive ordinary executable restart and repair.

use super::{HANG, contains, fail, proposal_value, save};
use crate::{Fail, stop};
use astream_wire::sync::Digest;
use aterm_db::{
    accounts::proposals::{Proposals, Snapshot},
    database::Change,
};
use aterm_json::{Value, json};
use aterm_secrets::client::Paths;
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};

const KEY: &str = "offline-conflict";

pub(super) struct Probe {
    expected: BTreeSet<Digest>,
    values: [(Digest, Value); 2],
}

impl Probe {
    /// Both executable hosts must have stopped before their local writes.
    pub(super) fn offline(
        paths: &[Paths; 2],
        prior: [Digest; 3],
        out: &Path,
    ) -> Result<Self, Fail> {
        let write = |index: usize, request: u8, label: &str| {
            let value = proposal_value(label);
            let receipt = Proposals::at(paths[index].clone())
                .write(
                    [request; 16],
                    vec![Change {
                        collection: "accounts".into(),
                        key: KEY.into(),
                        value: Some(value.clone()),
                    }],
                )
                .map_err(fail)?;
            Ok::<_, Fail>((receipt.id, value))
        };
        let values = [write(0, 4, "offline-left")?, write(1, 5, "offline-right")?];
        let expected = prior.len() + values.len();
        let probe = Self {
            expected: prior
                .into_iter()
                .chain(values.iter().map(|(id, _)| *id))
                .collect(),
            values,
        };
        if probe.expected.len() != expected {
            return Err(Fail::die(
                "offline conflict operations must have distinct identities",
            ));
        }
        let mut writer = None;
        let mut before = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let snapshot = Proposals::at(path.clone()).snapshot().map_err(fail)?;
            let (own, value) = &probe.values[index];
            let other = probe.values[1 - index].0;
            if !snapshot.history_complete()
                || !contains(&snapshot, *own, KEY, value)
                || snapshot.operations.iter().any(|op| op.header.id == other)
                || probe.complete(&snapshot)
            {
                return Err(Fail::die(
                    "offline conflict needs complete independent histories",
                ));
            }
            let actor = snapshot
                .operations
                .iter()
                .find(|op| op.header.id == *own)
                .ok_or_else(|| Fail::die("offline writer's operation is absent"))?
                .header
                .actor;
            if writer.replace(actor) == Some(actor) {
                return Err(Fail::die("offline conflict needs independent writers"));
            }
            before.push(json!({
                "home": path.home.display().to_string(),
                "writer": aterm_codec::hex::encode_lower(&actor),
                "own_operation": aterm_codec::hex::encode_lower(own),
                "peer_operation_absent": true,
                "complete_conflict": false,
                "known": snapshot.operations.iter().map(|op| aterm_codec::hex::encode_lower(&op.header.id)).collect::<Vec<_>>(),
            }));
        }
        save(
            out,
            "conflict-before.json",
            &json!({"key": KEY, "replicas": before}),
        )?;
        Ok(probe)
    }

    fn complete(&self, snapshot: &Snapshot) -> bool {
        snapshot.history_complete()
            && snapshot.operations.len() == self.expected.len()
            && snapshot
                .operations
                .iter()
                .all(|op| op.eligible && op.catalog_eligible)
            && snapshot
                .operations
                .iter()
                .map(|op| op.header.id)
                .collect::<BTreeSet<_>>()
                == self.expected
            && snapshot
                .cells
                .get(&("accounts".into(), KEY.into()))
                .is_some_and(|cell| {
                    !cell.stopped
                        && cell.candidates.len() == 2
                        && self.values.iter().all(|(id, value)| {
                            cell.candidates.iter().any(|candidate| {
                                candidate.operation == *id && candidate.value == *value
                            })
                        })
                })
    }

    pub(super) fn received(&self, paths: &Paths, out: &Path, name: &str) -> Result<(), Fail> {
        let until = Instant::now() + HANG;
        loop {
            stop::check()?;
            if let Ok(snapshot) = Proposals::at(paths.clone()).snapshot()
                && self.complete(&snapshot)
            {
                // Retained IDs alone cannot establish that the projection kept both values.
                let mut collapsed = snapshot.clone();
                collapsed
                    .cells
                    .get_mut(&("accounts".into(), KEY.into()))
                    .expect("complete conflict has its cell")
                    .candidates
                    .truncate(1);
                let single_winner_rejected = !self.complete(&collapsed);
                if !single_winner_rejected {
                    return Err(Fail::die(
                        "conflict check accepted a single-winner projection",
                    ));
                }
                return save(
                    out,
                    name,
                    &json!({
                        "table": aterm_codec::hex::encode_lower(&snapshot.scope.digest()),
                        "home": paths.home.display().to_string(),
                        "known": snapshot.operations.iter().map(|op| aterm_codec::hex::encode_lower(&op.header.id)).collect::<Vec<_>>(),
                        "withheld": snapshot.withheld,
                        "key": KEY,
                        "candidates": snapshot.cells[&("accounts".into(), KEY.into())].candidates.iter().map(|candidate| json!({"operation": aterm_codec::hex::encode_lower(&candidate.operation), "value": candidate.value})).collect::<Vec<_>>(),
                        "single_winner_rejected": single_winner_rejected,
                    }),
                );
            }
            if Instant::now() >= until {
                return Err(Fail::die(
                    "ordinary runtime did not preserve both offline values",
                ));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }

    pub(super) fn summary(&self) -> Value {
        json!({"key": KEY, "retained_operations": self.expected.len(), "candidates": 2})
    }
}
