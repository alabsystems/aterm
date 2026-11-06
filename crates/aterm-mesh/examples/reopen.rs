// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mesh logs' reopen, measured (`docs/measured/mesh-logs-reopen-2026-11-05.md`): a node
//! service's mesh is filled through [`aterm_mesh::Mesh::publish`] (origin-signed records, sealed
//! at rest, as shipped), closed, and its [`aterm_mesh::Mesh::open`] timed; beside it, a plaintext
//! broker of the same tier holding the same records is reopened, so the seal's share shows.
//!
//! A release build of a dev tool: `targo --unverified run --release -p aterm-mesh --example
//! reopen [RECORDS...]` (default 10000 100000 400000 per log). Each reopen runs on a warm page
//! cache, right after the close.

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
fn main() {
    unix::main();
}

#[cfg(unix)]
mod unix {
    use astream_broker::{Boot, Broker, Prepared};
    use aterm_link::durability::Durability;
    use aterm_mesh::logs::MeshConfig;
    use aterm_mesh::{Mesh, MeshLog, Standings};
    use aterm_net::fleet::link::{LinkClock, SystemClock};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    const BODY: usize = 512;

    pub fn main() {
        let sizes: Vec<u64> = std::env::args()
            .skip(1)
            .map(|arg| arg.parse().expect("a record count"))
            .collect();
        let sizes = if sizes.is_empty() {
            vec![10_000, 100_000, 400_000]
        } else {
            sizes
        };
        println!(
            "| records per log | tables.log MiB | metrics.log MiB | Mesh::open ms (both logs, sealed) \
             | tables.log plaintext open ms | metrics.log plaintext open ms |"
        );
        println!("|---|---|---|---|---|---|");
        for records in sizes {
            measure(records);
        }
    }

    fn mib(path: &Path) -> f64 {
        std::fs::metadata(path).map_or(0.0, |m| m.len() as f64 / (1024.0 * 1024.0))
    }

    fn config(state: &Path, key: [u8; 32]) -> MeshConfig {
        let alone = Standings {
            current: [key].into(),
            ..Standings::default()
        };
        MeshConfig {
            state_dir: state.to_path_buf(),
            backups: aterm_uds::backup::Backups::INERT,
            clock: Arc::new(SystemClock::new()) as Arc<dyn LinkClock>,
            wall: Arc::new(SystemTime::now),
            standings: Arc::new(move || Some(alone.clone())),
            changed: None,
            watch_every: Duration::from_secs(60),
            observe: None,
            stores: None,
        }
    }

    fn measure(records: u64) {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("am-reopen")
            .tempdir_in("/tmp")
            .expect("scratch");
        let state = scratch.path().join("state");
        aterm_uds::private_file::ensure_private_dir(&state).expect("state dir");
        let seed = [0x42_u8; 32];
        let mut text = aterm_codec::hex::encode_lower(&seed);
        text.push('\n');
        aterm_uds::private_file::write_private(
            &state,
            aterm_net::fleet_member::NODE_KEY_FILE,
            text.as_bytes(),
        )
        .expect("node key");
        let identity = aterm_net::fleet_member::node_identity(&state).expect("node identity");
        let key = *identity.key().bytes();
        let body = vec![0x5a_u8; BODY];

        let mesh = Mesh::open(config(&state, key)).expect("open");
        let me = mesh.principal().to_owned();
        for n in 0..records {
            mesh.publish(MeshLog::Tables, &format!("/t/{:02x}/{me}", n % 64), &body)
                .expect("a table record");
            mesh.publish(
                MeshLog::Metrics,
                &format!("/m/{me}/shared/1m/s{}", n % 64),
                &body,
            )
            .expect("a metrics row");
        }
        mesh.close();
        drop(mesh);
        let dir = state.join(aterm_mesh::logs::MESH_DIR);
        let tables = mib(&dir.join("tables").join("tables.log"));
        let metrics = mib(&dir.join("metrics").join("metrics.log"));

        let started = Instant::now();
        let mesh = Mesh::open(config(&state, key)).expect("reopen");
        let sealed = started.elapsed().as_secs_f64() * 1000.0;
        mesh.close();
        drop(mesh);

        let plain: Vec<f64> = [
            (MeshLog::Tables, Durability::Ordered),
            (MeshLog::Metrics, Durability::Relaxed),
        ]
        .into_iter()
        .map(|(log, tier)| plaintext_open(scratch.path(), log, tier, records, &body))
        .collect();
        println!(
            "| {records} | {tables:.1} | {metrics:.1} | {sealed:.1} | {:.1} | {:.1} |",
            plain[0], plain[1]
        );
    }

    /// A plaintext broker of `tier` filled with `records` records of `body`, closed and reopened:
    /// the reopen's milliseconds.
    fn plaintext_open(
        root: &Path,
        log: MeshLog,
        tier: Durability,
        records: u64,
        body: &[u8],
    ) -> f64 {
        let file = root.join(format!("plain-{}", log.file()));
        let opts = || {
            let mut opts = tier.options();
            opts.boot = Boot::Known([7; 16]);
            opts
        };
        {
            let broker = Broker::open_opts(&file, opts(), || Ok(Prepared::Nothing)).expect("open");
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a pair");
            drop(
                broker
                    .serve_stream(Arc::new(theirs), [1; 32])
                    .expect("adopted"),
            );
            let mut client = astream_broker::connect_stream(Arc::new(ours)).0;
            for seq in 1..=records {
                client
                    .publish(
                        1,
                        seq,
                        &format!("/t/{:02x}/n-0000000000000000", seq % 64),
                        body,
                    )
                    .expect("publish");
            }
        }
        let started = Instant::now();
        let broker = Broker::open_opts(&file, opts(), || Ok(Prepared::Nothing)).expect("reopen");
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        drop(broker);
        ms
    }
}
